//! V0.2 ractor adapter runtime for the dactor actor framework.
//!
//! Bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API with ractor's
//! single-message-type `ractor::Actor` trait using type-erased dispatch.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use dactor::actor::{
    Actor, ActorContext, ActorError, ActorRef, AskReply, ReduceHandler, Handler, ExpandHandler,
};
use dactor::dead_letter::{DeadLetterEvent, DeadLetterHandler, DeadLetterReason};
use dactor::dispatch::{AskDispatch, Dispatch, ReduceDispatch, ExpandDispatch, TypedDispatch};
use dactor::errors::{ActorSendError, ErrorAction, RuntimeError};
use dactor::interceptor::{
    Disposition, DropObserver, InboundContext, InboundInterceptor, OutboundInterceptor, Outcome,
    SendMode,
};
use dactor::mailbox::MailboxConfig;
use dactor::message::{Headers, Message, RuntimeHeaders};
use dactor::node::{ActorId, NodeId};
use dactor::runtime_support::{
    spawn_reduce_batched_drain, spawn_reduce_drain, wrap_batched_stream_with_interception,
    wrap_stream_with_interception, OutboundPipeline,
};
use dactor::stream::{
    BatchConfig, BatchReader, BatchWriter, BoxStream, StreamReceiver, StreamSender,
};
use dactor::supervision::ChildTerminated;
use dactor::system_actors::{
    CancelManager, CancelResponse, NodeDirectory, PeerStatus, SpawnManager, SpawnRequest,
    SpawnResponse, WatchManager, WatchNotification,
};
use dactor::type_registry::TypeRegistry;

use crate::cluster::RactorClusterEvents;

// ---------------------------------------------------------------------------
// Watch registry
// ---------------------------------------------------------------------------

/// A type-erased entry in the watch registry.
struct WatchEntry {
    watcher_id: ActorId,
    /// Closure that delivers a [`ChildTerminated`] to the watcher actor.
    notify: Box<dyn Fn(ChildTerminated) + Send + Sync>,
}

/// Shared watch registry mapping watched actor ID → list of watcher entries.
type WatcherMap = Arc<Mutex<HashMap<ActorId, Vec<WatchEntry>>>>;

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
    watchers: WatcherMap,
    stop_reason: Option<String>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
    /// Notified when the actor stops (for await_stop).
    stop_notifier: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

/// Arguments passed to the ractor actor at spawn time.
struct RactorSpawnArgs<A: Actor> {
    args: A::Args,
    deps: A::Deps,
    actor_id: ActorId,
    actor_name: String,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
    watchers: WatcherMap,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
    stop_notifier: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
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
            watchers: args.watchers,
            stop_reason: None,
            dead_letter_handler: args.dead_letter_handler,
            stop_notifier: args.stop_notifier,
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
            if matches!(disposition, Disposition::Drop) {
                if let Some(ref handler) = *state.dead_letter_handler {
                    let event = DeadLetterEvent {
                        target_id: state.ctx.actor_id.clone(),
                        target_name: Some(state.ctx.actor_name.clone()),
                        message_type,
                        send_mode,
                        reason: DeadLetterReason::DroppedByInterceptor {
                            interceptor: interceptor_name.clone(),
                        },
                        message: None,
                    };
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler.on_dead_letter(event);
                    }));
                }
            }
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
            let dispatch_fut =
                std::panic::AssertUnwindSafe(dispatch.dispatch(&mut state.actor, &mut state.ctx))
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
            std::panic::AssertUnwindSafe(dispatch.dispatch(&mut state.actor, &mut state.ctx))
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
                    (Some(reply), SendMode::Ask) => Outcome::AskSuccess {
                        reply: reply.as_ref(),
                    },
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
                        // Actor resumes — do NOT set stop_reason
                    }
                    ErrorAction::Stop | ErrorAction::Escalate => {
                        state.stop_reason = Some("handler panicked".into());
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

        // Run on_stop with panic catching so panics propagate as errors
        // through await_stop() instead of aborting the ractor task.
        let on_stop_panicked =
            std::panic::AssertUnwindSafe(state.actor.on_stop())
                .catch_unwind()
                .await
                .is_err();
        if on_stop_panicked && state.stop_reason.is_none() {
            state.stop_reason = Some("actor panicked in on_stop".to_string());
        }

        // Notify all watchers that this actor has terminated.
        let actor_id = state.ctx.actor_id.clone();
        let actor_name = state.ctx.actor_name.clone();
        let entries = {
            let mut watchers = state.watchers.lock().unwrap();
            // Remove entries where this actor is the TARGET (watchers of this actor)
            let target_entries = watchers.remove(&actor_id).unwrap_or_default();
            // Also clean up entries where this actor is the WATCHER (prevent leak)
            for entries in watchers.values_mut() {
                entries.retain(|e| e.watcher_id != actor_id);
            }
            watchers.retain(|_, v| !v.is_empty());
            target_entries
        };
        if !entries.is_empty() {
            let notification = ChildTerminated {
                child_id: actor_id,
                child_name: actor_name,
                reason: state.stop_reason.clone(),
            };
            for entry in &entries {
                (entry.notify)(notification.clone());
            }
        }

        // Notify await_stop() waiters — track on_stop panic independently
        // from stop_reason (which may already be set by a handler panic).
        if let Some(tx) = state.stop_notifier.take() {
            let result = if on_stop_panicked {
                Err("actor panicked in on_stop".to_string())
            } else {
                Ok(())
            };
            let _ = tx.send(result);
        }

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
    drop_observer: Option<Arc<dyn DropObserver>>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
}

impl<A: Actor> Clone for RactorActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            inner: self.inner.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
        }
    }
}

impl<A: Actor> std::fmt::Debug for RactorActorRef<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RactorActorRef({}, {:?})", self.name, self.id)
    }
}

impl<A: Actor + 'static> RactorActorRef<A> {
    fn outbound_pipeline(&self) -> OutboundPipeline {
        OutboundPipeline {
            interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            target_id: self.id.clone(),
            target_name: self.name.clone(),
        }
    }

    fn notify_dead_letter(
        &self,
        message_type: &'static str,
        send_mode: SendMode,
        reason: DeadLetterReason,
    ) {
        if let Some(ref handler) = *self.dead_letter_handler {
            let event = DeadLetterEvent {
                target_id: self.id.clone(),
                target_name: Some(self.name.clone()),
                message_type,
                send_mode,
                reason,
                message: None,
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler.on_dead_letter(event);
            }));
        }
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
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Tell, &msg);
        match result.disposition {
            Disposition::Continue => {}
            Disposition::Delay(_) => {} // Not supported in sync tell
            Disposition::Drop | Disposition::Reject(_) | Disposition::Retry(_) => return Ok(()),
        }

        let dispatch: Box<dyn Dispatch<A>> = Box::new(TypedDispatch { msg });
        self.inner.cast(DactorMsg(dispatch)).map_err(|e| {
            self.notify_dead_letter(
                std::any::type_name::<M>(),
                SendMode::Tell,
                DeadLetterReason::ActorStopped,
            );
            ActorSendError(e.to_string())
        })
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
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Ask, &msg);
        match result.disposition {
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
                let _ = tx.send(Err(RuntimeError::Rejected {
                    interceptor: result.interceptor_name.to_string(),
                    reason,
                }));
                return Ok(AskReply::new(rx));
            }
            Disposition::Retry(retry_after) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(Err(RuntimeError::RetryAfter {
                    interceptor: result.interceptor_name.to_string(),
                    retry_after,
                }));
                return Ok(AskReply::new(rx));
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(AskDispatch {
            msg,
            reply_tx: tx,
            cancel,
        });
        self.inner.cast(DactorMsg(dispatch)).map_err(|e| {
            self.notify_dead_letter(
                std::any::type_name::<M>(),
                SendMode::Ask,
                DeadLetterReason::ActorStopped,
            );
            ActorSendError(e.to_string())
        })?;
        Ok(AskReply::new(rx))
    }

    fn expand<M>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: ExpandHandler<M>,
        M: Message,
    {
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Expand, &msg);
        match result.disposition {
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

        let buffer = buffer.max(1);
        let (tx, mut rx) = tokio::sync::mpsc::channel(buffer);
        let sender = StreamSender::new(tx);
        let dispatch: Box<dyn Dispatch<A>> = Box::new(ExpandDispatch {
            msg,
            sender,
            cancel,
        });
        self.inner
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;

        match batch_config {
            Some(batch_config) => {
                let (batch_tx, batch_rx) = tokio::sync::mpsc::channel::<Vec<M::Reply>>(buffer);
                let reader = BatchReader::new(batch_rx);
                let batch_delay = batch_config.max_delay;
                tokio::spawn(async move {
                    let mut writer = BatchWriter::new(batch_tx, batch_config);
                    loop {
                        if writer.buffered_count() > 0 {
                            let deadline = tokio::time::Instant::now() + batch_delay;
                            tokio::select! {
                                biased;
                                item = rx.recv() => match item {
                                    Some(item) => {
                                        if writer.push(item).await.is_err() { break; }
                                    }
                                    None => break,
                                },
                                _ = tokio::time::sleep_until(deadline) => {
                                    if writer.check_deadline().await.is_err() { break; }
                                }
                            }
                        } else {
                            match rx.recv().await {
                                Some(item) => {
                                    if writer.push(item).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                    let _ = writer.flush().await;
                });
                Ok(wrap_batched_stream_with_interception(
                    reader,
                    buffer,
                    pipeline,
                    std::any::type_name::<M>(),
                ))
            }
            None => Ok(wrap_stream_with_interception(
                rx,
                buffer,
                pipeline,
                std::any::type_name::<M>(),
            )),
        }
    }

    fn reduce<Item, Reply>(
        &self,
        input: BoxStream<Item>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: ReduceHandler<Item, Reply>,
        Item: Send + 'static,
        Reply: Send + 'static,
    {
        let buffer = buffer.max(1);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(ReduceDispatch {
            receiver,
            reply_tx,
            cancel: cancel.clone(),
        });
        self.inner
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;

        let pipeline = self.outbound_pipeline();
        match batch_config {
            Some(batch_config) => {
                spawn_reduce_batched_drain(
                    input,
                    item_tx,
                    buffer,
                    batch_config,
                    cancel,
                    pipeline,
                    std::any::type_name::<Item>(),
                );
            }
            None => {
                spawn_reduce_drain(
                    input,
                    item_tx,
                    cancel,
                    pipeline,
                    std::any::type_name::<Item>(),
                );
            }
        }

        Ok(AskReply::new(reply_rx))
    }
}

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor, including the inbound interceptor pipeline.
/// Options for spawning an actor. Use `..Default::default()` to future-proof
/// against new fields.
pub struct SpawnOptions {
    /// Inbound interceptors to attach to the actor.
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
    /// Mailbox capacity configuration.
    ///
    /// **Note:** The ractor adapter currently only supports unbounded mailboxes.
    /// Setting `Bounded` will log a warning and fall back to unbounded.
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
// RactorRuntime
// ---------------------------------------------------------------------------

/// A dactor v0.2 runtime backed by ractor.
///
/// Actors are spawned as real ractor actors via [`ractor::Actor::spawn`].
/// Messages are delivered through ractor's mailbox as type-erased dispatch
/// envelopes, supporting multiple `Handler<M>` impls per actor.
///
/// ## System Actors
///
/// The runtime spawns native ractor system actors on creation:
/// - [`SpawnManagerActor`](crate::system_actors::SpawnManagerActor) — handles remote spawn requests
/// - [`WatchManagerActor`](crate::system_actors::WatchManagerActor) — handles remote watch/unwatch
/// - [`CancelManagerActor`](crate::system_actors::CancelManagerActor) — handles remote cancellation
/// - [`NodeDirectoryActor`](crate::system_actors::NodeDirectoryActor) — tracks peer connections
///
/// System actors are accessible via `system_actor_refs()` for transport routing.
pub struct RactorRuntime {
    node_id: NodeId,
    next_local: Arc<AtomicU64>,
    cluster_events: RactorClusterEvents,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    drop_observer: Option<Arc<dyn DropObserver>>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
    watchers: WatcherMap,
    /// Plain struct system actors (backward-compatible sync API).
    spawn_manager: SpawnManager,
    watch_manager: WatchManager,
    cancel_manager: CancelManager,
    node_directory: NodeDirectory,
    /// Native ractor system actor refs (lazily started via `start_system_actors()`).
    system_actors: Option<RactorSystemActorRefs>,
    /// Stop notification receivers for await_stop(), keyed by ActorId.
    #[allow(clippy::type_complexity)]
    stop_receivers: Arc<Mutex<HashMap<ActorId, tokio::sync::oneshot::Receiver<Result<(), String>>>>>,
}

/// References to the native ractor system actors spawned by the runtime.
pub struct RactorSystemActorRefs {
    pub spawn_manager: ractor::ActorRef<crate::system_actors::SpawnManagerMsg>,
    pub watch_manager: ractor::ActorRef<crate::system_actors::WatchManagerMsg>,
    pub cancel_manager: ractor::ActorRef<crate::system_actors::CancelManagerMsg>,
    pub node_directory: ractor::ActorRef<crate::system_actors::NodeDirectoryMsg>,
}

impl RactorRuntime {
    /// Create a new `RactorRuntime`.
    ///
    /// System actors are not spawned until `start_system_actors()` is called.
    /// This allows the runtime to be constructed outside a tokio context.
    pub fn new() -> Self {
        Self::create(NodeId("ractor-node".into()))
    }

    /// Create a new `RactorRuntime` with a specific node ID.
    pub fn with_node_id(node_id: NodeId) -> Self {
        Self::create(node_id)
    }

    fn create(node_id: NodeId) -> Self {
        Self {
            node_id,
            next_local: Arc::new(AtomicU64::new(1)),
            cluster_events: RactorClusterEvents::new(),
            outbound_interceptors: Arc::new(Vec::new()),
            drop_observer: None,
            dead_letter_handler: Arc::new(None),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            spawn_manager: SpawnManager::new(TypeRegistry::new()),
            watch_manager: WatchManager::new(),
            cancel_manager: CancelManager::new(),
            node_directory: NodeDirectory::new(),
            system_actors: None,
            stop_receivers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn native ractor system actors for transport routing.
    ///
    /// Must be called from within a tokio runtime context. After this call,
    /// `system_actor_refs()` returns the native actor references.
    ///
    /// Factory registrations made via `register_factory()` before this call
    /// are forwarded to the native SpawnManagerActor.
    pub async fn start_system_actors(&mut self) {
        use crate::system_actors::*;

        let (spawn_ref, _) = ractor::Actor::spawn(
            None, SpawnManagerActor,
            (self.node_id.clone(), TypeRegistry::new(), self.next_local.clone()),
        ).await.expect("failed to spawn SpawnManagerActor");

        let (watch_ref, _) = ractor::Actor::spawn(
            None, WatchManagerActor, (),
        ).await.expect("failed to spawn WatchManagerActor");

        let (cancel_ref, _) = ractor::Actor::spawn(
            None, CancelManagerActor, (),
        ).await.expect("failed to spawn CancelManagerActor");

        let (node_dir_ref, _) = ractor::Actor::spawn(
            None, NodeDirectoryActor, (),
        ).await.expect("failed to spawn NodeDirectoryActor");

        self.system_actors = Some(RactorSystemActorRefs {
            spawn_manager: spawn_ref,
            watch_manager: watch_ref,
            cancel_manager: cancel_ref,
            node_directory: node_dir_ref,
        });
    }

    /// Returns the node ID of this runtime.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Access the native system actor references for transport routing.
    ///
    /// Returns `None` if `start_system_actors()` has not been called yet.
    pub fn system_actor_refs(&self) -> Option<&RactorSystemActorRefs> {
        self.system_actors.as_ref()
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

    /// Set a global drop observer that is notified whenever an outbound
    /// interceptor returns `Disposition::Drop`.
    pub fn set_drop_observer(&mut self, observer: Arc<dyn DropObserver>) {
        self.drop_observer = Some(observer);
    }

    /// Set a global dead letter handler. Called whenever a message cannot be
    /// delivered (actor stopped, mailbox full, dropped by inbound interceptor).
    pub fn set_dead_letter_handler(&mut self, handler: Arc<dyn DeadLetterHandler>) {
        self.dead_letter_handler = Arc::new(Some(handler));
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
    pub async fn spawn<A>(&self, name: &str, args: A::Args) -> Result<RactorActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_internal::<A>(name, args, (), Vec::new()).await
    }

    /// Spawn an actor with explicit dependencies.
    pub async fn spawn_with_deps<A>(&self, name: &str, args: A::Args, deps: A::Deps) -> Result<RactorActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor + 'static,
    {
        self.spawn_internal::<A>(name, args, deps, Vec::new()).await
    }

    /// Spawn an actor with spawn options (including inbound interceptors).
    pub async fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> Result<RactorActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor<Deps = ()> + 'static,
    {
        if !matches!(options.mailbox, MailboxConfig::Unbounded) {
            tracing::warn!("ractor adapter: bounded mailbox not yet implemented, using unbounded");
        }
        self.spawn_internal::<A>(name, args, (), options.interceptors).await
    }

    async fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
    ) -> Result<RactorActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor + 'static,
    {
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

        let wrapper = RactorDactorActor::<A> {
            _phantom: PhantomData,
        };
        let spawn_args = RactorSpawnArgs {
            args,
            deps,
            actor_id: actor_id.clone(),
            actor_name: actor_name.clone(),
            interceptors,
            watchers: self.watchers.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
            stop_notifier: Some(stop_tx),
        };

        let (actor_ref, _join_handle) = ractor::Actor::spawn(Some(name.to_string()), wrapper, spawn_args)
            .await
            .map_err(|e| dactor::errors::RuntimeError::SpawnFailed(e.to_string()))?;

        // Store stop receiver for await_stop()
        self.stop_receivers.lock().unwrap().insert(actor_id.clone(), stop_rx);

        Ok(RactorActorRef {
            id: actor_id,
            name: actor_name,
            inner: actor_ref,
            outbound_interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
        })
    }

    /// Register actor `watcher` to be notified when `target_id` terminates.
    ///
    /// The watcher must implement `Handler<ChildTerminated>`. When the target
    /// stops, the runtime delivers a [`ChildTerminated`] message to the watcher.
    pub fn watch<W>(&self, watcher: &RactorActorRef<W>, target_id: ActorId)
    where
        W: Actor + Handler<ChildTerminated> + 'static,
    {
        let watcher_id = watcher.id();
        let watcher_inner = watcher.inner.clone();

        let entry = WatchEntry {
            watcher_id,
            notify: Box::new(move |msg: ChildTerminated| {
                let dispatch: Box<dyn Dispatch<W>> = Box::new(TypedDispatch { msg });
                if watcher_inner.cast(DactorMsg(dispatch)).is_err() {
                    tracing::debug!("watch notification dropped — watcher may have stopped");
                }
            }),
        };

        let mut watchers = self.watchers.lock().unwrap();
        watchers.entry(target_id).or_default().push(entry);
    }

    /// Unregister `watcher_id` from notifications about `target_id`.
    pub fn unwatch(&self, watcher_id: &ActorId, target_id: &ActorId) {
        let mut watchers = self.watchers.lock().unwrap();
        if let Some(entries) = watchers.get_mut(target_id) {
            entries.retain(|e| &e.watcher_id != watcher_id);
            if entries.is_empty() {
                watchers.remove(target_id);
            }
        }
    }

    // -----------------------------------------------------------------------
    // SA1: SpawnManager wiring
    // -----------------------------------------------------------------------

    /// Access the spawn manager.
    pub fn spawn_manager(&self) -> &SpawnManager {
        &self.spawn_manager
    }

    /// Access the spawn manager mutably (for registering actor factories).
    pub fn spawn_manager_mut(&mut self) -> &mut SpawnManager {
        &mut self.spawn_manager
    }

    /// Register an actor type for remote spawning on this node.
    ///
    /// Registers the factory in both the struct-based SpawnManager and the
    /// native SpawnManagerActor (if started via `start_system_actors()`).
    pub fn register_factory(
        &mut self,
        type_name: impl Into<String>,
        factory: impl Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
            + Send
            + Sync
            + 'static,
    ) {
        let type_name = type_name.into();
        let factory = Arc::new(factory);

        // Register in struct-based manager
        self.spawn_manager
            .type_registry_mut()
            .register_factory(type_name.clone(), {
                let f = factory.clone();
                move |bytes: &[u8]| f(bytes)
            });

        // Forward to native actor if started
        if let Some(ref actors) = self.system_actors {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let f = factory;
            let _ = actors.spawn_manager.cast(
                crate::system_actors::SpawnManagerMsg::RegisterFactory {
                    type_name,
                    factory: Box::new(move |bytes: &[u8]| f(bytes)),
                    reply: tx,
                },
            );
        }
    }

    /// Process a remote spawn request.
    ///
    /// Looks up the actor type in the registry, deserializes Args from bytes,
    /// and returns the constructed actor along with its assigned [`ActorId`].
    ///
    /// The returned `ActorId` is pre-assigned for the remote spawn flow where
    /// the runtime controls ID assignment. The caller must use this ID when
    /// registering the spawned actor (not via the regular `spawn()` path,
    /// which assigns its own IDs).
    ///
    /// **Note:** Currently uses the simple factory API (`TypeRegistry::create_actor`).
    /// For actors with non-trivial `Deps`, use `spawn_manager_mut()` to access
    /// `TypeRegistry::create_actor_with_deps()` directly.
    ///
    /// Returns `Ok((actor_id, actor))` on success, or `Err(SpawnResponse::Failure)`
    /// if the type is not found or deserialization fails.
    pub fn handle_spawn_request(
        &mut self,
        request: &SpawnRequest,
    ) -> Result<(ActorId, Box<dyn std::any::Any + Send>), SpawnResponse> {
        match self.spawn_manager.create_actor(request) {
            Ok(actor) => {
                let local = self.next_local.fetch_add(1, Ordering::SeqCst);
                let actor_id = ActorId {
                    node: self.node_id.clone(),
                    local,
                };
                self.spawn_manager.record_spawn(actor_id.clone());
                Ok((actor_id, actor))
            }
            Err(e) => Err(SpawnResponse::Failure {
                request_id: request.request_id.clone(),
                error: e.to_string(),
            }),
        }
    }

    // -----------------------------------------------------------------------
    // SA2: WatchManager wiring
    // -----------------------------------------------------------------------

    /// Access the watch manager (for remote watch subscriptions).
    pub fn watch_manager(&self) -> &WatchManager {
        &self.watch_manager
    }

    /// Access the watch manager mutably.
    pub fn watch_manager_mut(&mut self) -> &mut WatchManager {
        &mut self.watch_manager
    }

    /// Register a remote watch: a remote watcher wants to know when a local
    /// actor terminates.
    pub fn remote_watch(&mut self, target: ActorId, watcher: ActorId) {
        self.watch_manager.watch(target, watcher);
    }

    /// Remove a remote watch subscription.
    pub fn remote_unwatch(&mut self, target: &ActorId, watcher: &ActorId) {
        self.watch_manager.unwatch(target, watcher);
    }

    /// Called when a local actor terminates. Returns notifications for all
    /// remote watchers that should be sent to their respective nodes.
    ///
    /// **Note:** This must be called explicitly by the integration layer.
    /// It is not yet automatically wired into ractor's actor stop lifecycle.
    pub fn notify_terminated(&mut self, terminated: &ActorId) -> Vec<WatchNotification> {
        self.watch_manager.on_terminated(terminated)
    }

    // -----------------------------------------------------------------------
    // SA3: CancelManager wiring
    // -----------------------------------------------------------------------

    /// Access the cancel manager.
    pub fn cancel_manager(&self) -> &CancelManager {
        &self.cancel_manager
    }

    /// Access the cancel manager mutably.
    pub fn cancel_manager_mut(&mut self) -> &mut CancelManager {
        &mut self.cancel_manager
    }

    /// Register a cancellation token for a request (for remote cancel support).
    pub fn register_cancel(&mut self, request_id: String, token: CancellationToken) {
        self.cancel_manager.register(request_id, token);
    }

    /// Process a remote cancellation request.
    pub fn cancel_request(&mut self, request_id: &str) -> CancelResponse {
        self.cancel_manager.cancel(request_id)
    }

    /// Clean up a cancellation token after its request completes normally.
    ///
    /// Should be called when a remote ask/stream/feed completes successfully
    /// to prevent stale tokens from accumulating.
    pub fn complete_request(&mut self, request_id: &str) {
        self.cancel_manager.remove(request_id);
    }

    // -----------------------------------------------------------------------
    // SA4: NodeDirectory wiring
    // -----------------------------------------------------------------------

    /// Access the node directory.
    pub fn node_directory(&self) -> &NodeDirectory {
        &self.node_directory
    }

    /// Access the node directory mutably.
    pub fn node_directory_mut(&mut self) -> &mut NodeDirectory {
        &mut self.node_directory
    }

    /// Register a peer node in the directory.
    ///
    /// If the peer already exists, updates its status to `Connected` and
    /// preserves the existing address when `address` is `None`.
    /// Emits a `ClusterEvent::NodeJoined` if the peer was not previously connected.
    pub fn connect_peer(&mut self, peer_id: NodeId, address: Option<String>) {
        let was_connected = self.node_directory.is_connected(&peer_id);
        if let Some(existing) = self.node_directory.get_peer(&peer_id) {
            // Preserve existing address if new address is None
            let resolved_address = address.or_else(|| existing.address.clone());
            self.node_directory.remove_peer(&peer_id);
            self.node_directory.add_peer(peer_id.clone(), resolved_address);
        } else {
            self.node_directory.add_peer(peer_id.clone(), address);
        }
        self.node_directory.set_status(&peer_id, PeerStatus::Connected);
        if !was_connected {
            self.cluster_events.emit(dactor::ClusterEvent::NodeJoined(peer_id));
        }
    }

    /// Mark a peer as disconnected.
    ///
    /// Emits a `ClusterEvent::NodeLeft` if the peer was previously connected.
    pub fn disconnect_peer(&mut self, peer_id: &NodeId) {
        let was_connected = self.node_directory.is_connected(peer_id);
        self.node_directory.set_status(peer_id, PeerStatus::Disconnected);
        if was_connected {
            self.cluster_events.emit(dactor::ClusterEvent::NodeLeft(peer_id.clone()));
        }
    }

    /// Check if a peer node is connected.
    pub fn is_peer_connected(&self, peer_id: &NodeId) -> bool {
        self.node_directory.is_connected(peer_id)
    }

    // -----------------------------------------------------------------------
    // JH1-JH2: Actor lifecycle handles
    // -----------------------------------------------------------------------

    /// Wait for an actor to stop.
    ///
    /// Returns `Ok(())` when the actor finishes cleanly, or `Err` if the
    /// actor panicked in `on_stop`. The stop receiver is consumed and removed
    /// from the map.
    ///
    /// Returns `Ok(())` immediately if no stop receiver is stored for this ID
    /// (e.g., actor was already awaited or was not spawned by this runtime).
    pub async fn await_stop(&self, actor_id: &ActorId) -> Result<(), String> {
        let rx = {
            let mut receivers = self.stop_receivers.lock().unwrap();
            receivers.remove(actor_id)
        };
        match rx {
            Some(rx) => rx
                .await
                .map_err(|_| "stop notifier dropped".to_string())
                .and_then(|r| r),
            None => Ok(()),
        }
    }

    /// Wait for all spawned actors to stop.
    ///
    /// Drains all stored stop receivers and awaits them all. Returns the first
    /// error encountered, but always waits for every actor to finish.
    pub async fn await_all(&self) -> Result<(), String> {
        let receivers: Vec<_> = {
            let mut map = self.stop_receivers.lock().unwrap();
            map.drain().collect()
        };
        let mut first_error = None;
        for (_, rx) in receivers {
            let result = rx.await.map_err(|e| format!("stop notifier dropped: {e}")).and_then(|r| r);
            if let Err(e) = result {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Remove completed stop receivers from the map.
    ///
    /// Call periodically to prevent stale entries from accumulating
    /// for actors that stopped without being awaited.
    pub fn cleanup_finished(&self) {
        let mut receivers = self.stop_receivers.lock().unwrap();
        receivers.retain(|_, rx| {
            matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty))
        });
    }

    /// Number of actors with stored stop receivers.
    ///
    /// Note: includes receivers for actors that have already stopped but
    /// haven't been awaited or cleaned up. Call `cleanup_finished()` first
    /// for an accurate count of running actors.
    pub fn active_handle_count(&self) -> usize {
        self.stop_receivers.lock().unwrap().len()
    }
}

impl Default for RactorRuntime {
    fn default() -> Self {
        Self::new()
    }
}
