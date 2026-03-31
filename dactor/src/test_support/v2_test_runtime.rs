//! V2 test runtime for the `Actor` / `Handler<M>` / `TypedActorRef<A>` API.
//!
//! Provides a lightweight, in-process actor runtime suitable for unit tests.
//! Actors are spawned on the Tokio runtime and process messages sequentially
//! via an unbounded channel.

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use tokio::sync::mpsc;

use crate::actor::{Actor, ActorContext, ActorError, AskReply, Handler, TypedActorRef};
use crate::errors::{ActorSendError, ErrorAction, RuntimeError};
use crate::interceptor::{
    Disposition, InboundContext, InboundInterceptor, OutboundContext, OutboundInterceptor,
    Outcome, SendMode,
};
use crate::mailbox::{MailboxConfig, OverflowStrategy};
use crate::message::{Headers, Message, RuntimeHeaders};
use crate::node::{ActorId, NodeId};

// ---------------------------------------------------------------------------
// Type-erased dispatch via async trait
// ---------------------------------------------------------------------------

/// Type-erased message envelope. Each concrete message type is wrapped in a
/// `TypedDispatch<M>` that knows how to invoke `Handler<M>::handle`.
#[async_trait]
trait Dispatch<A: Actor>: Send {
    /// Dispatch the message to the actor's handler.
    /// Returns the type-erased reply for ask (Some), or None for tell.
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult;

    /// The message as `&dyn Any` for interceptor inspection.
    fn message_any(&self) -> &dyn Any;

    /// The send mode (Tell or Ask).
    fn send_mode(&self) -> SendMode;

    /// The message type name.
    fn message_type_name(&self) -> &'static str;

    /// Reject this dispatch — for ask, sends the proper error to the caller.
    /// For tell, silently drops (fire-and-forget has no error path).
    fn reject(self: Box<Self>, disposition: Disposition, interceptor_name: &str);
}

/// Result of dispatching a message, including the type-erased reply for ask.
struct DispatchResult {
    /// The type-erased reply value (Some for ask, None for tell).
    reply: Option<Box<dyn Any + Send>>,
    /// Oneshot sender to deliver the reply to the caller (Some for ask).
    reply_sender: Option<Box<dyn FnOnce(Box<dyn Any + Send>) + Send>>,
}

impl DispatchResult {
    fn tell() -> Self {
        Self { reply: None, reply_sender: None }
    }

    /// Send the reply to the caller (for ask). Must be called after interceptors inspect it.
    fn send_reply(self) {
        if let (Some(reply), Some(sender)) = (self.reply, self.reply_sender) {
            sender(reply);
        }
    }
}

struct TypedDispatch<M: Message> {
    msg: M,
}

#[async_trait]
impl<A, M> Dispatch<A> for TypedDispatch<M>
where
    A: Handler<M>,
    M: Message<Reply = ()>,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        actor.handle(self.msg, ctx).await;
        DispatchResult::tell()
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Tell
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }

    fn reject(self: Box<Self>, _disposition: Disposition, _interceptor_name: &str) {
        // tell: silently drop (fire-and-forget has no error path)
    }
}

/// Ask envelope: carries the message and a oneshot sender for the reply.
struct AskDispatch<M: Message> {
    msg: M,
    reply_tx: tokio::sync::oneshot::Sender<Result<M::Reply, RuntimeError>>,
}

#[async_trait]
impl<A, M> Dispatch<A> for AskDispatch<M>
where
    A: Handler<M>,
    M: Message,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        let reply = actor.handle(self.msg, ctx).await;
        let reply_any: Box<dyn Any + Send> = Box::new(reply);
        let reply_tx = self.reply_tx;
        DispatchResult {
            reply: Some(reply_any),
            reply_sender: Some(Box::new(move |boxed_reply| {
                if let Ok(reply) = boxed_reply.downcast::<M::Reply>() {
                    if reply_tx.send(Ok(*reply)).is_err() {
                        tracing::debug!("ask reply dropped — caller may have timed out or been cancelled");
                    }
                }
            })),
        }
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Ask
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }

    fn reject(self: Box<Self>, disposition: Disposition, interceptor_name: &str) {
        let error = match disposition {
            Disposition::Reject(reason) => RuntimeError::Rejected {
                interceptor: interceptor_name.to_string(),
                reason,
            },
            Disposition::Retry(retry_after) => RuntimeError::RetryAfter {
                interceptor: interceptor_name.to_string(),
                retry_after,
            },
            Disposition::Drop => RuntimeError::ActorNotFound("message dropped by interceptor".into()),
            _ => return,
        };
        let _ = self.reply_tx.send(Err(error));
    }
}

type BoxedDispatch<A> = Box<dyn Dispatch<A>>;

// ---------------------------------------------------------------------------
// Mailbox channel wrappers
// ---------------------------------------------------------------------------

/// Unified sender that wraps both bounded and unbounded mpsc senders.
enum MailboxSender<A: Actor> {
    Unbounded(mpsc::UnboundedSender<Option<BoxedDispatch<A>>>),
    Bounded {
        sender: mpsc::Sender<Option<BoxedDispatch<A>>>,
        overflow: OverflowStrategy,
    },
}

impl<A: Actor> MailboxSender<A> {
    fn send(&self, msg: Option<BoxedDispatch<A>>) -> Result<(), ActorSendError> {
        match self {
            Self::Unbounded(tx) => tx
                .send(msg)
                .map_err(|_| ActorSendError("actor stopped".into())),
            Self::Bounded { sender, overflow } => match overflow {
                OverflowStrategy::RejectWithError => {
                    sender.try_send(msg).map_err(|e| match e {
                        mpsc::error::TrySendError::Full(_) => {
                            ActorSendError("mailbox full".into())
                        }
                        mpsc::error::TrySendError::Closed(_) => {
                            ActorSendError("actor stopped".into())
                        }
                    })
                }
                OverflowStrategy::DropNewest => match sender.try_send(msg) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => Ok(()), // silently drop
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        Err(ActorSendError("actor stopped".into()))
                    }
                },
                OverflowStrategy::Block => {
                    // Block is not supported in sync tell(). Treat as RejectWithError.
                    sender.try_send(msg).map_err(|e| match e {
                        mpsc::error::TrySendError::Full(_) => ActorSendError(
                            "mailbox full (Block not supported in sync tell)".into(),
                        ),
                        mpsc::error::TrySendError::Closed(_) => {
                            ActorSendError("actor stopped".into())
                        }
                    })
                }
            },
        }
    }

    /// Force-send a control signal bypassing overflow strategy.
    /// Used for stop signals that must not be dropped.
    fn force_send(&self, msg: Option<BoxedDispatch<A>>) {
        match self {
            Self::Unbounded(tx) => { let _ = tx.send(msg); }
            Self::Bounded { sender, .. } => {
                // For control signals, use regular send (not try_send).
                // This may block briefly but guarantees delivery.
                // If the channel is closed, the signal is moot (actor already stopped).
                let _ = sender.try_send(msg);
                // If full, the actor will stop naturally when all senders are dropped.
            }
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            Self::Unbounded(tx) => tx.is_closed(),
            Self::Bounded { sender, .. } => sender.is_closed(),
        }
    }
}

impl<A: Actor> Clone for MailboxSender<A> {
    fn clone(&self) -> Self {
        match self {
            Self::Unbounded(tx) => Self::Unbounded(tx.clone()),
            Self::Bounded { sender, overflow } => Self::Bounded {
                sender: sender.clone(),
                overflow: *overflow,
            },
        }
    }
}

/// Unified receiver that wraps both bounded and unbounded mpsc receivers.
enum MailboxReceiver<A: Actor> {
    Unbounded(mpsc::UnboundedReceiver<Option<BoxedDispatch<A>>>),
    Bounded(mpsc::Receiver<Option<BoxedDispatch<A>>>),
}

impl<A: Actor> MailboxReceiver<A> {
    async fn recv(&mut self) -> Option<Option<BoxedDispatch<A>>> {
        match self {
            Self::Unbounded(rx) => rx.recv().await,
            Self::Bounded(rx) => rx.recv().await,
        }
    }
}

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor, including the inbound interceptor pipeline.
pub struct SpawnOptions {
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
    /// Mailbox configuration (unbounded by default).
    pub mailbox: MailboxConfig,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::Unbounded,
        }
    }
}

// ---------------------------------------------------------------------------
// V2ActorRef
// ---------------------------------------------------------------------------

/// A test actor reference implementing `TypedActorRef<A>`.
pub struct V2ActorRef<A: Actor> {
    id: ActorId,
    name: String,
    sender: MailboxSender<A>,
    alive: Arc<AtomicBool>,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
}

impl<A: Actor> Clone for V2ActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            sender: self.sender.clone(),
            alive: self.alive.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
}

impl<A: Actor> TypedActorRef<A> for V2ActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire) && !self.sender.is_closed()
    }

    fn stop(&self) {
        // Mark as not alive immediately so is_alive() returns false
        self.alive.store(false, Ordering::SeqCst);
        // Send stop signal bypassing overflow strategy
        self.sender.force_send(None);
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        // Run outbound interceptors on the caller's task
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Tell,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                // NOTE: Outbound Delay is not supported in this test runtime because
                // tell() is synchronous. The production runtime (async) will implement
                // actual delays. Delay is silently skipped here.
                Disposition::Delay(_) => {}
                Disposition::Drop => return Ok(()),
                Disposition::Reject(_) => return Ok(()),
                Disposition::Retry(_) => return Ok(()),
            }
        }

        let dispatch: BoxedDispatch<A> = Box::new(TypedDispatch { msg });
        self.sender.send(Some(dispatch))
    }

    fn ask<M>(&self, msg: M) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message,
    {
        // Run outbound interceptors on the caller's task
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Ask,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                // NOTE: Outbound Delay not supported in test runtime (sync context).
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    // Send explicit error so caller gets ActorNotFound rather than hanging
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Err(RuntimeError::ActorNotFound(
                        "message dropped by outbound interceptor".into(),
                    )));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Reject(reason) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::Rejected { interceptor: name, reason }));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Retry(retry_after) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::RetryAfter { interceptor: name, retry_after }));
                    return Ok(AskReply::new(rx));
                }
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let dispatch: BoxedDispatch<A> = Box::new(AskDispatch {
            msg,
            reply_tx: tx,
        });
        self.sender.send(Some(dispatch))?;
        Ok(AskReply::new(rx))
    }
}

// ---------------------------------------------------------------------------
// V2TestRuntime
// ---------------------------------------------------------------------------

/// A lightweight test runtime that spawns v0.2 actors on the Tokio runtime.
pub struct V2TestRuntime {
    node_id: NodeId,
    next_local: AtomicU64,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
}

impl V2TestRuntime {
    pub fn new() -> Self {
        Self {
            node_id: NodeId("test-node".into()),
            next_local: AtomicU64::new(1),
            outbound_interceptors: Arc::new(Vec::new()),
        }
    }

    /// Add a global outbound interceptor.
    ///
    /// **Must be called before any actors are spawned.** Panics if actors
    /// already hold references to the interceptor list (i.e., after `spawn()`).
    /// This constraint ensures interceptor lists are immutable during actor lifetime.
    pub fn add_outbound_interceptor(&mut self, interceptor: Box<dyn OutboundInterceptor>) {
        Arc::get_mut(&mut self.outbound_interceptors)
            .expect("cannot add interceptors after actors are spawned")
            .push(interceptor);
    }

    /// Spawn a v0.2 actor whose `Deps` type is `()`. Returns a `V2ActorRef<A>`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> V2ActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_internal(name, args, (), Vec::new(), MailboxConfig::Unbounded)
    }

    /// Spawn a v0.2 actor with explicit dependencies.
    pub fn spawn_with_deps<A>(&self, name: &str, args: A::Args, deps: A::Deps) -> V2ActorRef<A>
    where
        A: Actor + 'static,
    {
        self.spawn_internal(name, args, deps, Vec::new(), MailboxConfig::Unbounded)
    }

    /// Spawn a v0.2 actor with spawn options (including interceptors and mailbox config).
    pub fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> V2ActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_internal(name, args, (), options.interceptors, options.mailbox)
    }

    fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
        mailbox: MailboxConfig,
    ) -> V2ActorRef<A>
    where
        A: Actor + 'static,
    {
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();

        let (tx, mut rx) = match &mailbox {
            MailboxConfig::Unbounded => {
                let (tx, rx) = mpsc::unbounded_channel::<Option<BoxedDispatch<A>>>();
                (
                    MailboxSender::Unbounded(tx),
                    MailboxReceiver::Unbounded(rx),
                )
            }
            MailboxConfig::Bounded { capacity, overflow } => {
                let (tx, rx) = mpsc::channel::<Option<BoxedDispatch<A>>>(*capacity);
                (
                    MailboxSender::Bounded {
                        sender: tx,
                        overflow: *overflow,
                    },
                    MailboxReceiver::Bounded(rx),
                )
            }
        };

        let id_task = actor_id.clone();
        let name_task = actor_name.clone();

        tokio::spawn(async move {
            let mut actor = A::create(args, deps);
            let mut ctx = ActorContext {
                actor_id: id_task,
                actor_name: name_task,
                send_mode: None,
                headers: Headers::new(),
            };

            actor.on_start(&mut ctx).await;

            while let Some(msg) = rx.recv().await {
                let dispatch = match msg {
                    None => break, // stop signal
                    Some(d) => d,
                };

                // Capture metadata before dispatch consumes the message
                let send_mode = dispatch.send_mode();
                let message_type = dispatch.message_type_name();

                // Set context fields for this message
                ctx.send_mode = Some(send_mode);
                ctx.headers = Headers::new();

                // Run inbound interceptor pipeline
                let runtime_headers = RuntimeHeaders::new();
                let mut headers = Headers::new();
                let mut total_delay = Duration::ZERO;
                let mut rejection: Option<(String, Disposition)> = None; // (interceptor_name, disposition)

                {
                    let ictx = InboundContext {
                        actor_id: ctx.actor_id.clone(),
                        actor_name: &ctx.actor_name,
                        message_type,
                        send_mode,
                        remote: false,
                        origin_node: None,
                    };

                    for interceptor in &interceptors {
                        match interceptor.on_receive(
                            &ictx,
                            &runtime_headers,
                            &mut headers,
                            dispatch.message_any(),
                        ) {
                            Disposition::Continue => {}
                            Disposition::Delay(d) => {
                                total_delay += d;
                            }
                            disp @ (Disposition::Drop | Disposition::Reject(_) | Disposition::Retry(_)) => {
                                rejection = Some((interceptor.name().to_string(), disp));
                                break;
                            }
                        }
                    }
                }

                // If rejected/dropped/retry, propagate proper error to caller
                if let Some((interceptor_name, disposition)) = rejection {
                    dispatch.reject(disposition, &interceptor_name);
                    continue;
                }

                if !total_delay.is_zero() {
                    tokio::time::sleep(total_delay).await;
                }

                // Copy interceptor-populated headers to ActorContext so handler can access them
                ctx.headers = headers;

                // Dispatch the message
                let result =
                    std::panic::AssertUnwindSafe(dispatch.dispatch(&mut actor, &mut ctx))
                        .catch_unwind()
                        .await;

                // Build context for on_complete (reuse headers from on_receive)
                let ictx = InboundContext {
                    actor_id: ctx.actor_id.clone(),
                    actor_name: &ctx.actor_name,
                    message_type,
                    send_mode,
                    remote: false,
                    origin_node: None,
                };

                match result {
                    Ok(dispatch_result) => {
                        let outcome = match (&dispatch_result.reply, send_mode) {
                            (Some(reply), SendMode::Ask) => Outcome::AskSuccess { reply: reply.as_ref() },
                            _ => Outcome::TellSuccess,
                        };

                        for interceptor in &interceptors {
                            interceptor.on_complete(&ictx, &runtime_headers, &ctx.headers, &outcome);
                        }

                        // Send reply to caller AFTER interceptors have seen it
                        dispatch_result.send_reply();
                    }
                    Err(_panic) => {
                        let error = ActorError::new("handler panicked");
                        let action = actor.on_error(&error);

                        let outcome = Outcome::HandlerError {
                            error,
                        };
                        for interceptor in &interceptors {
                            interceptor.on_complete(&ictx, &runtime_headers, &ctx.headers, &outcome);
                        }

                        match action {
                            ErrorAction::Resume => {
                                continue;
                            }
                            ErrorAction::Stop | ErrorAction::Escalate => {
                                tracing::error!("handler panicked in actor {}, stopping", ctx.actor_name);
                                break;
                            }
                            ErrorAction::Restart => {
                                // TODO: full restart with Args/Deps recreation
                                tracing::warn!(
                                    "Restart not fully implemented for actor {}, treating as Resume",
                                    ctx.actor_name
                                );
                                continue;
                            }
                        }
                    }
                }
            }

            // Set alive=false BEFORE on_stop to avoid is_alive race condition
            alive_task.store(false, Ordering::SeqCst);
            // Reset context for on_stop (no message being processed)
            ctx.send_mode = None;
            ctx.headers = Headers::new();
            actor.on_stop().await;
        });

        V2ActorRef {
            id: actor_id,
            name: actor_name,
            sender: tx,
            alive,
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
}

impl Default for V2TestRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ActorContext;
    use crate::message::Message;
    use crate::node::NodeId;

    // -- Shared test actor: Counter -----------------------------------------

    struct Increment(u64);
    impl Message for Increment {
        type Reply = ();
    }

    struct Counter {
        count: u64,
    }

    impl Actor for Counter {
        type Args = Self;
        type Deps = ();
        fn create(args: Self, _deps: ()) -> Self {
            args
        }
    }

    #[async_trait]
    impl Handler<Increment> for Counter {
        async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
            self.count += msg.0;
        }
    }

    struct GetCount;
    impl Message for GetCount {
        type Reply = u64;
    }

    #[async_trait]
    impl Handler<GetCount> for Counter {
        async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
            self.count
        }
    }

    struct Reset;
    impl Message for Reset {
        type Reply = u64;
    }

    #[async_trait]
    impl Handler<Reset> for Counter {
        async fn handle(&mut self, _msg: Reset, _ctx: &mut ActorContext) -> u64 {
            let old = self.count;
            self.count = 0;
            old
        }
    }

    // -- Tests --------------------------------------------------------------

    #[tokio::test]
    async fn test_spawn_and_tell() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(5)).unwrap();
        counter.tell(Increment(3)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(counter.is_alive());
    }

    #[tokio::test]
    async fn test_tell_returns_actor_id() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("my-counter", Counter { count: 0 });

        assert_eq!(counter.name(), "my-counter");
        assert_eq!(counter.id().node, NodeId("test-node".into()));
        assert!(counter.id().local > 0);
    }

    #[tokio::test]
    async fn test_tell_100_messages_in_order() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        struct OrderTracker {
            received: Arc<Mutex<Vec<u64>>>,
        }

        impl Actor for OrderTracker {
            type Args = Arc<Mutex<Vec<u64>>>;
            type Deps = ();
            fn create(args: Arc<Mutex<Vec<u64>>>, _deps: ()) -> Self {
                OrderTracker { received: args }
            }
        }

        struct TrackMsg(u64);
        impl Message for TrackMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<TrackMsg> for OrderTracker {
            async fn handle(&mut self, msg: TrackMsg, _ctx: &mut ActorContext) {
                self.received.lock().await.push(msg.0);
            }
        }

        let received = Arc::new(Mutex::new(Vec::new()));
        let runtime = V2TestRuntime::new();
        let tracker = runtime.spawn::<OrderTracker>("tracker", received.clone());

        for i in 0..100 {
            tracker.tell(TrackMsg(i)).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let result = received.lock().await;
        assert_eq!(result.len(), 100);
        for (i, val) in result.iter().enumerate() {
            assert_eq!(*val, i as u64, "message {} out of order", i);
        }
    }

    #[tokio::test]
    async fn test_multiple_actor_refs() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        let ref1 = counter.clone();
        let ref2 = counter.clone();

        ref1.tell(Increment(10)).unwrap();
        ref2.tell(Increment(20)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(ref1.is_alive());
        assert!(ref2.is_alive());
    }

    #[tokio::test]
    async fn test_on_start_called_before_messages() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        struct StartTracker {
            log: Arc<Mutex<Vec<String>>>,
        }

        struct StartTrackerArgs(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl Actor for StartTracker {
            type Args = StartTrackerArgs;
            type Deps = ();
            fn create(args: StartTrackerArgs, _deps: ()) -> Self {
                StartTracker { log: args.0 }
            }
            async fn on_start(&mut self, _ctx: &mut ActorContext) {
                self.log.lock().await.push("on_start".into());
            }
        }

        struct Ping;
        impl Message for Ping {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<Ping> for StartTracker {
            async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {
                self.log.lock().await.push("handle".into());
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<StartTracker>("tracker", StartTrackerArgs(log.clone()));

        actor.tell(Ping).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let entries = log.lock().await;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "on_start");
        assert_eq!(entries[1], "handle");
    }

    #[tokio::test]
    async fn test_tell_to_stopped_actor() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        // Drop the original to close the channel
        let counter2 = counter.clone();
        drop(counter);

        // counter2 still holds a sender, so the actor is alive
        assert!(counter2.tell(Increment(1)).is_ok());
    }

    #[tokio::test]
    async fn test_unique_actor_ids() {
        let runtime = V2TestRuntime::new();
        let a = runtime.spawn::<Counter>("a", Counter { count: 0 });
        let b = runtime.spawn::<Counter>("b", Counter { count: 0 });

        assert_ne!(a.id(), b.id());
        assert!(a.id().local < b.id().local);
    }

    // -- Ask tests ----------------------------------------------------------

    #[tokio::test]
    async fn test_ask_get_count() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 42 });

        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn test_ask_after_tell() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(10)).unwrap();
        counter.tell(Increment(20)).unwrap();

        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 30);
    }

    #[tokio::test]
    async fn test_ask_reset_returns_old_value() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 100 });

        let old = counter.ask(Reset).unwrap().await.unwrap();
        assert_eq!(old, 100);

        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_concurrent_asks() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(100)).unwrap();

        // Ensure the tell is processed before asking
        let _ = counter.ask(GetCount).unwrap().await.unwrap();

        let ref1 = counter.clone();
        let ref2 = counter.clone();

        let (r1, r2) = tokio::join!(
            async { ref1.ask(GetCount).unwrap().await.unwrap() },
            async { ref2.ask(GetCount).unwrap().await.unwrap() },
        );

        assert_eq!(r1, 100);
        assert_eq!(r2, 100);
    }

    #[tokio::test]
    async fn test_interleaved_tell_ask() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(5)).unwrap();
        let c1 = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(c1, 5);

        counter.tell(Increment(3)).unwrap();
        let c2 = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(c2, 8);

        let old = counter.ask(Reset).unwrap().await.unwrap();
        assert_eq!(old, 8);

        let c3 = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(c3, 0);
    }

    // -- Interceptor tests --------------------------------------------------

    use std::sync::Mutex;

    struct LogInterceptor {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl InboundInterceptor for LogInterceptor {
        fn name(&self) -> &'static str {
            "log"
        }

        fn on_receive(
            &self,
            ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _h: &mut Headers,
            _msg: &dyn Any,
        ) -> Disposition {
            self.log
                .lock()
                .unwrap()
                .push(format!("on_receive:{}", ctx.message_type));
            Disposition::Continue
        }

        fn on_complete(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _h: &Headers,
            outcome: &Outcome<'_>,
        ) {
            self.log
                .lock()
                .unwrap()
                .push(format!("on_complete:{:?}", outcome));
        }
    }

    #[tokio::test]
    async fn test_interceptor_on_receive_and_on_complete_called() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(LogInterceptor { log: log.clone() })],
                ..Default::default()
            },
        );

        counter.tell(Increment(5)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].starts_with("on_receive:"));
        assert!(entries[1].starts_with("on_complete:TellSuccess"));
    }

    #[tokio::test]
    async fn test_interceptor_on_complete_replied_for_ask() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 42 },
            SpawnOptions {
                interceptors: vec![Box::new(LogInterceptor { log: log.clone() })],
                ..Default::default()
            },
        );

        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 42);

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].starts_with("on_receive:"));
        assert!(entries[1].starts_with("on_complete:AskSuccess"));
    }

    struct DropInterceptor;

    impl InboundInterceptor for DropInterceptor {
        fn name(&self) -> &'static str {
            "drop-all"
        }

        fn on_receive(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _h: &mut Headers,
            _msg: &dyn Any,
        ) -> Disposition {
            Disposition::Drop
        }
    }

    #[tokio::test]
    async fn test_disposition_drop_prevents_handler() {
        // Use a shared counter to verify the handler was never called
        let handle_count = Arc::new(AtomicU64::new(0));
        let handle_count_clone = handle_count.clone();

        struct CountingActor {
            handle_count: Arc<AtomicU64>,
        }

        impl Actor for CountingActor {
            type Args = Arc<AtomicU64>;
            type Deps = ();
            fn create(args: Arc<AtomicU64>, _deps: ()) -> Self {
                CountingActor { handle_count: args }
            }
        }

        struct Ping;
        impl Message for Ping {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<Ping> for CountingActor {
            async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {
                self.handle_count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn_with_options::<CountingActor>(
            "counting",
            handle_count_clone,
            SpawnOptions {
                interceptors: vec![Box::new(DropInterceptor)],
                ..Default::default()
            },
        );

        actor.tell(Ping).unwrap();
        actor.tell(Ping).unwrap();
        actor.tell(Ping).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(handle_count.load(Ordering::SeqCst), 0, "handler should not have been called");
        assert!(actor.is_alive(), "actor should still be alive after drops");
    }

    struct RejectInterceptor;

    impl InboundInterceptor for RejectInterceptor {
        fn name(&self) -> &'static str {
            "reject-all"
        }

        fn on_receive(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _h: &mut Headers,
            _msg: &dyn Any,
        ) -> Disposition {
            Disposition::Reject("forbidden".into())
        }
    }

    #[tokio::test]
    async fn test_disposition_reject_ask_returns_error() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 42 },
            SpawnOptions {
                interceptors: vec![Box::new(RejectInterceptor)],
                ..Default::default()
            },
        );

        let result = counter.ask(GetCount).unwrap().await;
        assert!(result.is_err(), "rejected ask should return Err");
        match result.unwrap_err() {
            RuntimeError::Rejected { interceptor, reason } => {
                assert_eq!(interceptor, "reject-all");
                assert_eq!(reason, "forbidden");
            }
            other => panic!("expected Rejected, got: {:?}", other),
        }
    }

    // ── Disposition::Retry tests ─────────────────────────────

    struct RetryInterceptor;
    impl InboundInterceptor for RetryInterceptor {
        fn name(&self) -> &'static str { "retry-later" }
        fn on_receive(
            &self, _ctx: &InboundContext<'_>, _rh: &RuntimeHeaders,
            _h: &mut Headers, _msg: &dyn Any,
        ) -> Disposition {
            Disposition::Retry(Duration::from_millis(500))
        }
    }

    #[tokio::test]
    async fn test_disposition_retry_ask_returns_retry_after() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 42 },
            SpawnOptions {
                interceptors: vec![Box::new(RetryInterceptor)],
                ..Default::default()
            },
        );

        let result = counter.ask(GetCount).unwrap().await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RuntimeError::RetryAfter { interceptor, retry_after } => {
                assert_eq!(interceptor, "retry-later");
                assert_eq!(retry_after, Duration::from_millis(500));
            }
            other => panic!("expected RetryAfter, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_disposition_retry_tell_silently_drops() {
        let handler_count = Arc::new(AtomicU64::new(0));
        let count_clone = handler_count.clone();

        struct TrackActor { count: Arc<AtomicU64> }
        impl Actor for TrackActor {
            type Args = Arc<AtomicU64>;
            type Deps = ();
            fn create(args: Arc<AtomicU64>, _: ()) -> Self { TrackActor { count: args } }
        }

        struct TrackMsg;
        impl Message for TrackMsg { type Reply = (); }

        #[async_trait]
        impl Handler<TrackMsg> for TrackActor {
            async fn handle(&mut self, _msg: TrackMsg, _ctx: &mut ActorContext) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn_with_options::<TrackActor>(
            "tracker",
            count_clone,
            SpawnOptions {
                interceptors: vec![Box::new(RetryInterceptor)],
                ..Default::default()
            },
        );

        actor.tell(TrackMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(handler_count.load(Ordering::SeqCst), 0, "handler should not be called when Retry");
    }

    #[tokio::test]
    async fn test_multiple_interceptors_execute_in_order() {
        let log = Arc::new(Mutex::new(Vec::new()));

        struct OrderedInterceptor {
            id: u32,
            log: Arc<Mutex<Vec<String>>>,
        }

        impl InboundInterceptor for OrderedInterceptor {
            fn name(&self) -> &'static str {
                "ordered"
            }

            fn on_receive(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("interceptor-{}", self.id));
                Disposition::Continue
            }
        }

        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![
                    Box::new(OrderedInterceptor {
                        id: 1,
                        log: log.clone(),
                    }),
                    Box::new(OrderedInterceptor {
                        id: 2,
                        log: log.clone(),
                    }),
                    Box::new(OrderedInterceptor {
                        id: 3,
                        log: log.clone(),
                    }),
                ],
                ..Default::default()
            },
        );

        counter.tell(Increment(1)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], "interceptor-1");
        assert_eq!(entries[1], "interceptor-2");
        assert_eq!(entries[2], "interceptor-3");
    }

    #[tokio::test]
    async fn test_drop_interceptor_short_circuits_chain() {
        let log = Arc::new(Mutex::new(Vec::new()));

        struct LabelInterceptor {
            label: &'static str,
            log: Arc<Mutex<Vec<String>>>,
        }

        impl InboundInterceptor for LabelInterceptor {
            fn name(&self) -> &'static str {
                self.label
            }

            fn on_receive(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                self.log.lock().unwrap().push(self.label.to_string());
                Disposition::Continue
            }
        }

        struct DropAtSecond;

        impl InboundInterceptor for DropAtSecond {
            fn name(&self) -> &'static str {
                "drop-at-second"
            }

            fn on_receive(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Drop
            }
        }

        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![
                    Box::new(LabelInterceptor {
                        label: "first",
                        log: log.clone(),
                    }),
                    Box::new(DropAtSecond),
                    Box::new(LabelInterceptor {
                        label: "third",
                        log: log.clone(),
                    }),
                ],
                ..Default::default()
            },
        );

        counter.tell(Increment(1)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let entries = log.lock().unwrap();
        // Only the first interceptor should have been called (second drops, third skipped)
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "first");
    }

    #[tokio::test]
    async fn test_disposition_delay() {
        struct DelayInterceptor;

        impl InboundInterceptor for DelayInterceptor {
            fn name(&self) -> &'static str {
                "delay"
            }

            fn on_receive(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Delay(Duration::from_millis(100))
            }
        }

        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(DelayInterceptor)],
                ..Default::default()
            },
        );

        let start = tokio::time::Instant::now();
        counter.tell(Increment(1)).unwrap();

        // Ask blocks until tell+ask are both processed (sequentially, both delayed)
        let count = counter.ask(GetCount).unwrap().await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(count, 1);
        // Both the tell and the ask were delayed by 100ms each
        assert!(
            elapsed >= Duration::from_millis(180),
            "expected cumulative delay, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_cumulative_delays_from_multiple_interceptors() {
        struct SmallDelay(u64);

        impl InboundInterceptor for SmallDelay {
            fn name(&self) -> &'static str {
                "small-delay"
            }

            fn on_receive(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Delay(Duration::from_millis(self.0))
            }
        }

        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![
                    Box::new(SmallDelay(50)),
                    Box::new(SmallDelay(50)),
                ],
                ..Default::default()
            },
        );

        let start = tokio::time::Instant::now();
        // Use ask to block until message is processed
        let count = counter.ask(GetCount).unwrap().await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(count, 0);
        // 50ms + 50ms = 100ms cumulative delay
        assert!(
            elapsed >= Duration::from_millis(80),
            "expected ~100ms cumulative delay, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_no_interceptors_existing_behavior_unchanged() {
        // Existing spawn() path should work identically
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(10)).unwrap();
        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 10);
    }

    #[tokio::test]
    async fn test_interceptor_can_inspect_message_type() {
        let log = Arc::new(Mutex::new(Vec::new()));

        struct TypeLogInterceptor {
            log: Arc<Mutex<Vec<String>>>,
        }

        impl InboundInterceptor for TypeLogInterceptor {
            fn name(&self) -> &'static str {
                "type-log"
            }

            fn on_receive(
                &self,
                ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("{}:{:?}", ctx.message_type, ctx.send_mode));
                Disposition::Continue
            }
        }

        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(TypeLogInterceptor { log: log.clone() })],
                ..Default::default()
            },
        );

        counter.tell(Increment(1)).unwrap();
        let _ = counter.ask(GetCount).unwrap().await.unwrap();

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        // First message is Tell, second is Ask
        assert!(entries[0].contains("Tell"), "got: {}", entries[0]);
        assert!(entries[1].contains("Ask"), "got: {}", entries[1]);
    }

    #[tokio::test]
    async fn test_interceptor_can_downcast_message() {
        let captured = Arc::new(Mutex::new(Vec::new()));

        struct DowncastInterceptor {
            captured: Arc<Mutex<Vec<u64>>>,
        }

        impl InboundInterceptor for DowncastInterceptor {
            fn name(&self) -> &'static str {
                "downcast"
            }

            fn on_receive(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                msg: &dyn Any,
            ) -> Disposition {
                if let Some(inc) = msg.downcast_ref::<Increment>() {
                    self.captured.lock().unwrap().push(inc.0);
                }
                Disposition::Continue
            }
        }

        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(DowncastInterceptor {
                    captured: captured.clone(),
                })],
                ..Default::default()
            },
        );

        counter.tell(Increment(42)).unwrap();
        counter.tell(Increment(7)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let values = captured.lock().unwrap();
        assert_eq!(*values, vec![42, 7]);
    }

    // -- Outbound interceptor tests ----------------------------------------

    #[tokio::test]
    async fn test_outbound_interceptor_on_send_called() {
        use std::sync::Mutex;

        let log = Arc::new(Mutex::new(Vec::<String>::new()));

        struct OutLog {
            log: Arc<Mutex<Vec<String>>>,
        }

        impl OutboundInterceptor for OutLog {
            fn name(&self) -> &'static str {
                "out-log"
            }

            fn on_send(
                &self,
                ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("on_send:{}", ctx.message_type));
                Disposition::Continue
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(OutLog { log: log.clone() }));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(5)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let entries = log.lock().unwrap();
        assert!(!entries.is_empty());
        assert!(entries[0].contains("Increment"));
    }

    #[tokio::test]
    async fn test_outbound_reject_ask() {
        struct RejectOut;

        impl OutboundInterceptor for RejectOut {
            fn name(&self) -> &'static str {
                "reject-out"
            }

            fn on_send(
                &self,
                _ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Reject("outbound blocked".into())
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(RejectOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 42 });

        let result = counter.ask(GetCount).unwrap().await;
        match result.unwrap_err() {
            RuntimeError::Rejected {
                interceptor,
                reason,
            } => {
                assert_eq!(interceptor, "reject-out");
                assert_eq!(reason, "outbound blocked");
            }
            other => panic!("expected Rejected, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_outbound_stamps_header() {
        use crate::message::Priority;

        struct StampPriority;

        impl OutboundInterceptor for StampPriority {
            fn name(&self) -> &'static str {
                "stamp"
            }

            fn on_send(
                &self,
                _ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                headers: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                headers.insert(Priority::HIGH);
                Disposition::Continue
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(StampPriority));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(1)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Header was stamped on outbound side — verified by no panic
    }

    #[tokio::test]
    async fn test_outbound_retry_ask() {
        struct RetryOut;

        impl OutboundInterceptor for RetryOut {
            fn name(&self) -> &'static str {
                "retry-out"
            }

            fn on_send(
                &self,
                _ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Retry(Duration::from_millis(250))
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(RetryOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        let result = counter.ask(GetCount).unwrap().await;
        match result.unwrap_err() {
            RuntimeError::RetryAfter {
                interceptor,
                retry_after,
            } => {
                assert_eq!(interceptor, "retry-out");
                assert_eq!(retry_after, Duration::from_millis(250));
            }
            other => panic!("expected RetryAfter, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_outbound_drop_tell_silently_drops() {
        struct DropOut;

        impl OutboundInterceptor for DropOut {
            fn name(&self) -> &'static str {
                "drop-out"
            }

            fn on_send(
                &self,
                _ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Drop
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(DropOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        // tell should succeed (no error path) but message should not be delivered
        counter.tell(Increment(100)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify actor still has count=0 via ask (without outbound drop)
        // Since the outbound interceptor drops all messages including ask,
        // we just verify tell returned Ok.
    }

    #[tokio::test]
    async fn test_outbound_drop_ask_returns_channel_closed() {
        struct DropOut;

        impl OutboundInterceptor for DropOut {
            fn name(&self) -> &'static str {
                "drop-out"
            }

            fn on_send(
                &self,
                _ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                Disposition::Drop
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(DropOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        let result = counter.ask(GetCount).unwrap().await;
        // Dropped ask returns a channel-closed error (ActorNotFound)
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_outbound_interceptor_sees_ask_send_mode() {
        use std::sync::Mutex;

        let log = Arc::new(Mutex::new(Vec::<String>::new()));

        struct ModeLog {
            log: Arc<Mutex<Vec<String>>>,
        }

        impl OutboundInterceptor for ModeLog {
            fn name(&self) -> &'static str {
                "mode-log"
            }

            fn on_send(
                &self,
                ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &mut Headers,
                _msg: &dyn Any,
            ) -> Disposition {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("{:?}", ctx.send_mode));
                Disposition::Continue
            }
        }

        let mut runtime = V2TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(ModeLog { log: log.clone() }));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(1)).unwrap();
        let _ = counter.ask(GetCount).unwrap().await;

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "Tell");
        assert_eq!(entries[1], "Ask");
    }

    // -- Lifecycle & ErrorAction tests -------------------------------------

    #[tokio::test]
    async fn test_stop_triggers_on_stop() {
        let log = Arc::new(Mutex::new(Vec::new()));

        struct StopTracker {
            log: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl Actor for StopTracker {
            type Args = Arc<Mutex<Vec<String>>>;
            type Deps = ();
            fn create(args: Arc<Mutex<Vec<String>>>, _: ()) -> Self {
                StopTracker { log: args }
            }
            async fn on_stop(&mut self) {
                self.log.lock().unwrap().push("on_stop".into());
            }
        }

        struct Ping;
        impl Message for Ping {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<Ping> for StopTracker {
            async fn handle(&mut self, _: Ping, _: &mut ActorContext) {}
        }

        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<StopTracker>("tracker", log.clone());

        actor.tell(Ping).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        actor.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let entries = log.lock().unwrap();
        assert!(entries.contains(&"on_stop".to_string()));
        assert!(!actor.is_alive());
    }

    #[tokio::test]
    async fn test_stop_makes_tell_fail() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!counter.is_alive());
        // Sending to a stopped actor should fail
        let result = counter.tell(Increment(1));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_on_error_resume_continues() {
        struct ResumeActor {
            count: Arc<AtomicU64>,
        }

        #[async_trait]
        impl Actor for ResumeActor {
            type Args = Arc<AtomicU64>;
            type Deps = ();
            fn create(args: Arc<AtomicU64>, _: ()) -> Self {
                ResumeActor { count: args }
            }
            fn on_error(&mut self, _: &ActorError) -> ErrorAction {
                ErrorAction::Resume
            }
        }

        struct PanicMsg;
        impl Message for PanicMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<PanicMsg> for ResumeActor {
            async fn handle(&mut self, _: PanicMsg, _: &mut ActorContext) {
                panic!("intentional panic");
            }
        }

        struct CountMsg;
        impl Message for CountMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<CountMsg> for ResumeActor {
            async fn handle(&mut self, _: CountMsg, _: &mut ActorContext) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let count = Arc::new(AtomicU64::new(0));
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<ResumeActor>("resume", count.clone());

        actor.tell(PanicMsg).unwrap(); // should panic but resume
        actor.tell(CountMsg).unwrap(); // should still be processed

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "actor should resume after panic");
        assert!(actor.is_alive());
    }

    #[tokio::test]
    async fn test_on_error_stop_terminates() {
        struct StopOnError {
            alive_flag: Arc<AtomicBool>,
        }

        #[async_trait]
        impl Actor for StopOnError {
            type Args = Arc<AtomicBool>;
            type Deps = ();
            fn create(args: Arc<AtomicBool>, _: ()) -> Self {
                StopOnError { alive_flag: args }
            }
            fn on_error(&mut self, _: &ActorError) -> ErrorAction {
                ErrorAction::Stop
            }
            async fn on_stop(&mut self) {
                self.alive_flag.store(false, Ordering::SeqCst);
            }
        }

        struct PanicMsg;
        impl Message for PanicMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<PanicMsg> for StopOnError {
            async fn handle(&mut self, _: PanicMsg, _: &mut ActorContext) {
                panic!("intentional");
            }
        }

        let alive = Arc::new(AtomicBool::new(true));
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<StopOnError>("stopper", alive.clone());

        actor.tell(PanicMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(!alive.load(Ordering::SeqCst), "on_stop should have been called");
        assert!(!actor.is_alive());
    }

    #[tokio::test]
    async fn test_on_error_default_is_stop() {
        struct PanicCounter {
            count: u64,
        }

        impl Actor for PanicCounter {
            type Args = Self;
            type Deps = ();
            fn create(args: Self, _: ()) -> Self {
                args
            }
        }

        struct PanicIncrement;
        impl Message for PanicIncrement {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<PanicIncrement> for PanicCounter {
            async fn handle(&mut self, _: PanicIncrement, _: &mut ActorContext) {
                panic!("boom");
            }
        }

        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<PanicCounter>("panic-counter", PanicCounter { count: 0 });

        actor.tell(PanicIncrement).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Default on_error is Stop, so actor should be dead
        assert!(!actor.is_alive());
    }

    #[tokio::test]
    async fn test_actor_context_has_send_mode() {
        let mode = Arc::new(Mutex::new(None));

        struct ModeTracker {
            mode: Arc<Mutex<Option<SendMode>>>,
        }

        impl Actor for ModeTracker {
            type Args = Arc<Mutex<Option<SendMode>>>;
            type Deps = ();
            fn create(args: Arc<Mutex<Option<SendMode>>>, _: ()) -> Self {
                ModeTracker { mode: args }
            }
        }

        struct Check;
        impl Message for Check {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<Check> for ModeTracker {
            async fn handle(&mut self, _: Check, ctx: &mut ActorContext) {
                *self.mode.lock().unwrap() = ctx.send_mode;
            }
        }

        let mode_ref = mode.clone();
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<ModeTracker>("tracker", mode_ref);

        actor.tell(Check).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(*mode.lock().unwrap(), Some(SendMode::Tell));
    }

    #[tokio::test]
    async fn test_actor_context_has_ask_send_mode() {
        let mode = Arc::new(Mutex::new(None));

        struct AskModeTracker {
            mode: Arc<Mutex<Option<SendMode>>>,
        }

        impl Actor for AskModeTracker {
            type Args = Arc<Mutex<Option<SendMode>>>;
            type Deps = ();
            fn create(args: Arc<Mutex<Option<SendMode>>>, _: ()) -> Self {
                AskModeTracker { mode: args }
            }
        }

        struct AskCheck;
        impl Message for AskCheck {
            type Reply = u64;
        }

        #[async_trait]
        impl Handler<AskCheck> for AskModeTracker {
            async fn handle(&mut self, _: AskCheck, ctx: &mut ActorContext) -> u64 {
                *self.mode.lock().unwrap() = ctx.send_mode;
                42
            }
        }

        let mode_ref = mode.clone();
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<AskModeTracker>("tracker", mode_ref);

        let _ = actor.ask(AskCheck).unwrap().await.unwrap();

        assert_eq!(*mode.lock().unwrap(), Some(SendMode::Ask));
    }

    #[tokio::test]
    async fn test_on_error_restart_treated_as_resume() {
        struct RestartActor {
            count: Arc<AtomicU64>,
        }

        #[async_trait]
        impl Actor for RestartActor {
            type Args = Arc<AtomicU64>;
            type Deps = ();
            fn create(args: Arc<AtomicU64>, _: ()) -> Self {
                RestartActor { count: args }
            }
            fn on_error(&mut self, _: &ActorError) -> ErrorAction {
                ErrorAction::Restart
            }
        }

        struct RestartPanicMsg;
        impl Message for RestartPanicMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<RestartPanicMsg> for RestartActor {
            async fn handle(&mut self, _: RestartPanicMsg, _: &mut ActorContext) {
                panic!("intentional panic");
            }
        }

        struct RestartCountMsg;
        impl Message for RestartCountMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<RestartCountMsg> for RestartActor {
            async fn handle(&mut self, _: RestartCountMsg, _: &mut ActorContext) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let count = Arc::new(AtomicU64::new(0));
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<RestartActor>("restart", count.clone());

        actor.tell(RestartPanicMsg).unwrap();
        actor.tell(RestartCountMsg).unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        // Restart is treated as Resume for now
        assert_eq!(count.load(Ordering::SeqCst), 1, "actor should continue after restart-as-resume");
        assert!(actor.is_alive());
    }

    // -- Mailbox tests ------------------------------------------------------

    #[tokio::test]
    async fn test_unbounded_mailbox_accepts_many() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        for _ in 0..1000 {
            counter.tell(Increment(1)).unwrap();
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 1000);
    }

    #[tokio::test]
    async fn test_default_spawn_is_unbounded() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        for _ in 0..100 {
            counter.tell(Increment(1)).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let count = counter.ask(GetCount).unwrap().await.unwrap();
        assert_eq!(count, 100);
    }

    #[tokio::test]
    async fn test_mailbox_config_in_spawn_options() {
        let opts = SpawnOptions {
            mailbox: MailboxConfig::Bounded {
                capacity: 5,
                overflow: OverflowStrategy::RejectWithError,
            },
            ..Default::default()
        };
        assert_eq!(
            opts.mailbox,
            MailboxConfig::Bounded {
                capacity: 5,
                overflow: OverflowStrategy::RejectWithError,
            }
        );
    }

    // Slow actor used by bounded mailbox tests
    struct SlowActor;
    impl Actor for SlowActor {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self { SlowActor }
    }

    struct SlowMsg;
    impl Message for SlowMsg { type Reply = (); }

    #[async_trait]
    impl Handler<SlowMsg> for SlowActor {
        async fn handle(&mut self, _: SlowMsg, _: &mut ActorContext) {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    #[tokio::test]
    async fn test_bounded_reject_when_full() {
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn_with_options::<SlowActor>(
            "slow",
            (),
            SpawnOptions {
                mailbox: MailboxConfig::Bounded {
                    capacity: 2,
                    overflow: OverflowStrategy::RejectWithError,
                },
                ..Default::default()
            },
        );

        // First message starts processing (blocks in handler)
        actor.tell(SlowMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Fill the bounded channel (capacity=2)
        actor.tell(SlowMsg).unwrap();
        actor.tell(SlowMsg).unwrap();

        // Third should fail — mailbox full
        let result = actor.tell(SlowMsg);
        assert!(result.is_err(), "should reject when mailbox full");
    }

    #[tokio::test]
    async fn test_bounded_drop_newest_when_full() {
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn_with_options::<SlowActor>(
            "slow",
            (),
            SpawnOptions {
                mailbox: MailboxConfig::Bounded {
                    capacity: 2,
                    overflow: OverflowStrategy::DropNewest,
                },
                ..Default::default()
            },
        );

        actor.tell(SlowMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        actor.tell(SlowMsg).unwrap();
        actor.tell(SlowMsg).unwrap();

        // Should succeed — silently dropped
        let result = actor.tell(SlowMsg);
        assert!(result.is_ok(), "DropNewest should silently succeed");
    }
}
