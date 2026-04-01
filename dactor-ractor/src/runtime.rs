//! V0.2 ractor adapter runtime for the dactor actor framework.
//!
//! Bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API with ractor's
//! single-message-type `ractor::Actor` trait using type-erased dispatch.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use dactor::actor::{
    Actor, ActorContext, ActorError, AskReply, FeedHandler, FeedMessage, Handler, StreamHandler,
    ActorRef,
};
use dactor::errors::{ActorSendError, ErrorAction, RuntimeError};
use dactor::interceptor::{
    Disposition, InboundContext, InboundInterceptor, OutboundContext, OutboundInterceptor, Outcome,
    SendMode,
};
use dactor::message::{Headers, Message, RuntimeHeaders};
use dactor::node::{ActorId, NodeId};
use dactor::stream::{BoxStream, StreamReceiver, StreamSender};

use crate::cluster::RactorClusterEvents;

// ---------------------------------------------------------------------------
// Type-erased dispatch (same pattern as TestRuntime)
// ---------------------------------------------------------------------------

#[async_trait]
trait Dispatch<A: Actor>: Send {
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult;
    fn message_any(&self) -> &dyn Any;
    fn send_mode(&self) -> SendMode;
    fn message_type_name(&self) -> &'static str;
    fn reject(self: Box<Self>, disposition: Disposition, interceptor_name: &str);
    fn cancel(self: Box<Self>);
    fn cancel_token(&self) -> Option<CancellationToken>;
}

struct DispatchResult {
    reply: Option<Box<dyn Any + Send>>,
    reply_sender: Option<Box<dyn FnOnce(Box<dyn Any + Send>) + Send>>,
}

impl DispatchResult {
    fn tell() -> Self {
        Self {
            reply: None,
            reply_sender: None,
        }
    }

    fn send_reply(self) {
        if let (Some(reply), Some(sender)) = (self.reply, self.reply_sender) {
            sender(reply);
        }
    }
}

// --- TypedDispatch (tell) ---

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
    fn reject(self: Box<Self>, _: Disposition, _: &str) {}
    fn cancel(self: Box<Self>) {}
    fn cancel_token(&self) -> Option<CancellationToken> {
        None
    }
}

// --- AskDispatch ---

struct AskDispatch<M: Message> {
    msg: M,
    reply_tx: tokio::sync::oneshot::Sender<Result<M::Reply, RuntimeError>>,
    cancel: Option<CancellationToken>,
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
                    let _ = reply_tx.send(Ok(*reply));
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
            Disposition::Drop => {
                RuntimeError::ActorNotFound("message dropped by interceptor".into())
            }
            _ => return,
        };
        let _ = self.reply_tx.send(Err(error));
    }
    fn cancel(self: Box<Self>) {
        let _ = self.reply_tx.send(Err(RuntimeError::Cancelled));
    }
    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}

// --- StreamDispatch ---

struct StreamDispatch<M: Message> {
    msg: M,
    sender: StreamSender<M::Reply>,
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, M> Dispatch<A> for StreamDispatch<M>
where
    A: StreamHandler<M>,
    M: Message,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        actor.handle_stream(self.msg, self.sender, ctx).await;
        DispatchResult::tell()
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }
    fn send_mode(&self) -> SendMode {
        SendMode::Stream
    }
    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }
    fn reject(self: Box<Self>, _: Disposition, _: &str) {}
    fn cancel(self: Box<Self>) {}
    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}

// --- FeedDispatch ---

struct FeedDispatch<M: FeedMessage> {
    msg: M,
    receiver: StreamReceiver<M::Item>,
    reply_tx: tokio::sync::oneshot::Sender<Result<M::Reply, RuntimeError>>,
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, M> Dispatch<A> for FeedDispatch<M>
where
    A: FeedHandler<M>,
    M: FeedMessage,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        let reply = actor.handle_feed(self.msg, self.receiver, ctx).await;
        let reply_any: Box<dyn Any + Send> = Box::new(reply);
        let reply_tx = self.reply_tx;
        DispatchResult {
            reply: Some(reply_any),
            reply_sender: Some(Box::new(move |boxed_reply| {
                if let Ok(reply) = boxed_reply.downcast::<M::Reply>() {
                    let _ = reply_tx.send(Ok(*reply));
                }
            })),
        }
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }
    fn send_mode(&self) -> SendMode {
        SendMode::Feed
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
            Disposition::Drop => {
                RuntimeError::ActorNotFound("message dropped by interceptor".into())
            }
            _ => return,
        };
        let _ = self.reply_tx.send(Err(error));
    }
    fn cancel(self: Box<Self>) {
        let _ = self.reply_tx.send(Err(RuntimeError::Cancelled));
    }
    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}

// ---------------------------------------------------------------------------
// Ractor wrapper actor
// ---------------------------------------------------------------------------

/// The ractor message type wrapping a type-erased dactor dispatch envelope.
struct DactorMsg<A: Actor>(Box<dyn Dispatch<A>>);

/// The ractor `Actor` implementation that bridges to a dactor `Actor`.
struct RactorDactorActor<A: Actor> {
    _phantom: PhantomData<fn() -> A>,
}

/// State held by the ractor actor, containing the dactor `Actor` instance.
struct RactorActorState<A: Actor> {
    actor: A,
    ctx: ActorContext,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
}

/// Arguments passed to the ractor actor at spawn time.
struct RactorSpawnArgs<A: Actor> {
    args: A::Args,
    deps: A::Deps,
    actor_id: ActorId,
    actor_name: String,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
}

impl<A: Actor + 'static> ractor::Actor for RactorDactorActor<A> {
    type Msg = DactorMsg<A>;
    type State = RactorActorState<A>;
    type Arguments = RactorSpawnArgs<A>;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        let mut actor = A::create(args.args, args.deps);
        let mut ctx = ActorContext::new(args.actor_id, args.actor_name);
        actor.on_start(&mut ctx).await;
        Ok(RactorActorState {
            actor,
            ctx,
            interceptors: args.interceptors,
        })
    }

    async fn handle(
        &self,
        myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        let dispatch = message.0;

        // Capture metadata before dispatch consumes the message
        let send_mode = dispatch.send_mode();
        let message_type = dispatch.message_type_name();

        state.ctx.send_mode = Some(send_mode);
        state.ctx.headers = Headers::new();

        // Run inbound interceptor pipeline
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let mut total_delay = Duration::ZERO;
        let mut rejection: Option<(String, Disposition)> = None;

        {
            let ictx = InboundContext {
                actor_id: state.ctx.actor_id.clone(),
                actor_name: &state.ctx.actor_name,
                message_type,
                send_mode,
                remote: false,
                origin_node: None,
            };

            for interceptor in &state.interceptors {
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
            return Ok(());
        }

        if !total_delay.is_zero() {
            tokio::time::sleep(total_delay).await;
        }

        // Copy interceptor-populated headers to ActorContext so handler can access them
        state.ctx.headers = headers;

        // Propagate cancellation token to context
        let cancel_token = dispatch.cancel_token();
        state.ctx.set_cancellation_token(cancel_token.clone());

        // Check if already cancelled before dispatching
        if let Some(ref token) = cancel_token {
            if token.is_cancelled() {
                dispatch.cancel();
                state.ctx.set_cancellation_token(None);
                return Ok(());
            }
        }

        // Dispatch with panic catching and cancellation racing
        let result = if let Some(ref token) = cancel_token {
            let dispatch_fut = std::panic::AssertUnwindSafe(
                dispatch.dispatch(&mut state.actor, &mut state.ctx),
            )
            .catch_unwind();
            tokio::select! {
                biased;
                r = dispatch_fut => r,
                _ = token.cancelled() => {
                    // In-flight cancellation: dispatch_fut is dropped, which drops
                    // reply_tx inside it. Caller's AskReply sees channel closed.
                    // Pre-dispatch cancellation (above) sends RuntimeError::Cancelled.
                    state.ctx.set_cancellation_token(None);
                    return Ok(());
                }
            }
        } else {
            std::panic::AssertUnwindSafe(
                dispatch.dispatch(&mut state.actor, &mut state.ctx),
            )
            .catch_unwind()
            .await
        };

        state.ctx.set_cancellation_token(None);

        // Build context for on_complete
        let ictx = InboundContext {
            actor_id: state.ctx.actor_id.clone(),
            actor_name: &state.ctx.actor_name,
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

                for interceptor in &state.interceptors {
                    interceptor.on_complete(&ictx, &runtime_headers, &state.ctx.headers, &outcome);
                }

                // Send reply to caller AFTER interceptors have seen it
                dispatch_result.send_reply();
            }
            Err(_panic) => {
                let error = ActorError::internal("handler panicked");
                let action = state.actor.on_error(&error);

                let outcome = Outcome::HandlerError { error };
                for interceptor in &state.interceptors {
                    interceptor.on_complete(&ictx, &runtime_headers, &state.ctx.headers, &outcome);
                }

                match action {
                    ErrorAction::Resume => {
                        // Continue processing next message
                    }
                    ErrorAction::Stop | ErrorAction::Escalate => {
                        myself.stop(None);
                    }
                    ErrorAction::Restart => {
                        tracing::warn!("Restart not fully implemented, treating as Resume");
                    }
                }
            }
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        // Reset context to lifecycle semantics before on_stop
        state.ctx.send_mode = None;
        state.ctx.headers = Headers::new();
        state.ctx.set_cancellation_token(None);
        state.actor.on_stop().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RactorActorRef — dactor ActorRef backed by ractor
// ---------------------------------------------------------------------------

/// A dactor `ActorRef` backed by a ractor `ActorRef`.
///
/// Messages are delivered through ractor's mailbox as type-erased dispatch
/// envelopes, enabling multiple `Handler<M>` impls per actor.
pub struct RactorActorRef<A: Actor> {
    id: ActorId,
    name: String,
    inner: ractor::ActorRef<DactorMsg<A>>,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
}

impl<A: Actor> Clone for RactorActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            inner: self.inner.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
}

impl<A: Actor> std::fmt::Debug for RactorActorRef<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RactorActorRef({}, {:?})", self.name, self.id)
    }
}

impl<A: Actor + 'static> ActorRef<A> for RactorActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        matches!(
            self.inner.get_status(),
            ractor::ActorStatus::Running
                | ractor::ActorStatus::Starting
                | ractor::ActorStatus::Upgrading
        )
    }

    fn stop(&self) {
        self.inner.stop(None);
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        // Run outbound interceptors
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
                Disposition::Delay(_) => {} // Not supported in sync tell
                Disposition::Drop => return Ok(()),
                Disposition::Reject(_) => return Ok(()),
                Disposition::Retry(_) => return Ok(()),
            }
        }

        let dispatch: Box<dyn Dispatch<A>> = Box::new(TypedDispatch { msg });
        self.inner
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))
    }

    fn ask<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message,
    {
        // Run outbound interceptors
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
                Disposition::Delay(_) => {} // Not supported in sync context
                Disposition::Drop => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Err(RuntimeError::ActorNotFound(
                        "message dropped by outbound interceptor".into(),
                    )));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Reject(reason) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::Rejected {
                        interceptor: name,
                        reason,
                    }));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Retry(retry_after) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::RetryAfter {
                        interceptor: name,
                        retry_after,
                    }));
                    return Ok(AskReply::new(rx));
                }
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(AskDispatch {
            msg,
            reply_tx: tx,
            cancel,
        });
        self.inner
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;
        Ok(AskReply::new(rx))
    }

    fn stream<M>(
        &self,
        msg: M,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        // Run outbound interceptors
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Stream,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    return Err(ActorSendError(
                        "stream dropped by outbound interceptor".into(),
                    ));
                }
                Disposition::Reject(reason) => {
                    return Err(ActorSendError(format!("stream rejected: {}", reason)));
                }
                Disposition::Retry(_) => {
                    return Err(ActorSendError(
                        "stream retry requested by interceptor".into(),
                    ));
                }
            }
        }

        let buffer = buffer.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(buffer);
        let sender = StreamSender::new(tx);
        let dispatch: Box<dyn Dispatch<A>> = Box::new(StreamDispatch { msg, sender, cancel });
        self.inner
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn feed<M>(
        &self,
        msg: M,
        input: BoxStream<M::Item>,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: FeedHandler<M>,
        M: FeedMessage,
    {
        // Run outbound interceptors
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Feed,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Err(RuntimeError::ActorNotFound(
                        "message dropped by outbound interceptor".into(),
                    )));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Reject(reason) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::Rejected {
                        interceptor: name,
                        reason,
                    }));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Retry(retry_after) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::RetryAfter {
                        interceptor: name,
                        retry_after,
                    }));
                    return Ok(AskReply::new(rx));
                }
            }
        }

        let buffer = buffer.max(1);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(FeedDispatch {
            msg,
            receiver,
            reply_tx,
            cancel: cancel.clone(),
        });
        self.inner
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Drain the input stream into the item channel
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut input = input;
            let result = std::panic::AssertUnwindSafe(async {
                if let Some(ref token) = cancel {
                    loop {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => break,
                            item = input.next() => match item {
                                Some(item) => {
                                    if item_tx.send(item).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                } else {
                    while let Some(item) = input.next().await {
                        if item_tx.send(item).await.is_err() {
                            break;
                        }
                    }
                }
            })
            .catch_unwind()
            .await;
            if result.is_err() {
                tracing::error!("feed drain task panicked");
            }
        });

        Ok(AskReply::new(reply_rx))
    }
}

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor, including the inbound interceptor pipeline.
pub struct SpawnOptions {
    /// Inbound interceptors to attach to the actor.
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            interceptors: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// RactorRuntime
// ---------------------------------------------------------------------------

/// A dactor v0.2 runtime backed by ractor.
///
/// Actors are spawned as real ractor actors via [`ractor::Actor::spawn`].
/// Messages are delivered through ractor's mailbox as type-erased dispatch
/// envelopes, supporting multiple `Handler<M>` impls per actor.
pub struct RactorRuntime {
    node_id: NodeId,
    next_local: AtomicU64,
    cluster_events: RactorClusterEvents,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
}

impl RactorRuntime {
    /// Create a new `RactorRuntime`.
    pub fn new() -> Self {
        Self {
            node_id: NodeId("ractor-node".into()),
            next_local: AtomicU64::new(1),
            cluster_events: RactorClusterEvents::new(),
            outbound_interceptors: Arc::new(Vec::new()),
        }
    }

    /// Add a global outbound interceptor.
    ///
    /// **Must be called before any actors are spawned.** Panics if actors
    /// already hold references to the interceptor list (i.e., after `spawn()`).
    pub fn add_outbound_interceptor(&mut self, interceptor: Box<dyn OutboundInterceptor>) {
        Arc::get_mut(&mut self.outbound_interceptors)
            .expect("cannot add interceptors after actors are spawned")
            .push(interceptor);
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events_handle(&self) -> &RactorClusterEvents {
        &self.cluster_events
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events(&self) -> &RactorClusterEvents {
        &self.cluster_events
    }

    /// Spawn an actor with `Deps = ()`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> RactorActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_internal::<A>(name, args, (), Vec::new())
    }

    /// Spawn an actor with explicit dependencies.
    pub fn spawn_with_deps<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
    ) -> RactorActorRef<A>
    where
        A: Actor + 'static,
    {
        self.spawn_internal::<A>(name, args, deps, Vec::new())
    }

    /// Spawn an actor with spawn options (including inbound interceptors).
    pub fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> RactorActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_internal::<A>(name, args, (), options.interceptors)
    }

    fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
    ) -> RactorActorRef<A>
    where
        A: Actor + 'static,
    {
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();

        let wrapper = RactorDactorActor::<A> {
            _phantom: PhantomData,
        };
        let spawn_args = RactorSpawnArgs {
            args,
            deps,
            actor_id: actor_id.clone(),
            actor_name: actor_name.clone(),
            interceptors,
        };

        // Bridge sync → async: use a std thread to avoid blocking the
        // current tokio runtime while awaiting ractor::Actor::spawn.
        let name_for_ractor = name.to_string();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            handle.block_on(async move {
                let result =
                    ractor::Actor::spawn(Some(name_for_ractor), wrapper, spawn_args).await;
                match result {
                    Ok((actor_ref, _join)) => {
                        let _ = tx.send(Ok(actor_ref));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                    }
                }
            });
        });

        let actor_ref = match rx.recv() {
            Ok(Ok(actor_ref)) => actor_ref,
            Ok(Err(e)) => panic!("failed to spawn ractor actor: {e}"),
            Err(_) => panic!("ractor actor spawn channel closed unexpectedly"),
        };

        RactorActorRef {
            id: actor_id,
            name: actor_name,
            inner: actor_ref,
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
}

impl Default for RactorRuntime {
    fn default() -> Self {
        Self::new()
    }
}
