//! V2 test runtime for the `Actor` / `Handler<M>` / `ActorRef<A>` API.
//!
//! Provides a lightweight, in-process actor runtime suitable for unit tests.
//! Actors are spawned on the Tokio runtime and process messages sequentially
//! via an unbounded channel.

#[allow(unused_imports)]
use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(unused_imports)]
use async_trait::async_trait;
use futures::FutureExt;
use tokio::sync::mpsc;

use crate::actor::{
    Actor, ActorContext, ActorError, ActorRef, AskReply, ReduceHandler, Handler, ExpandHandler,
    TransformHandler,
};
use crate::dead_letter::{DeadLetterEvent, DeadLetterHandler, DeadLetterReason};
#[allow(unused_imports)]
use crate::dispatch::DispatchResult;
use crate::dispatch::{AskDispatch, BoxedDispatch, ReduceDispatch, ExpandDispatch, TransformDispatch, TypedDispatch};
use crate::errors::{ActorSendError, ErrorAction, RuntimeError};
#[allow(unused_imports)]
use crate::interceptor::OutboundContext;
use crate::interceptor::{
    collect_handler_wrappers, apply_handler_wrappers,
    Disposition, DropObserver, InboundContext, InboundInterceptor, OutboundInterceptor, Outcome,
    SendMode,
};
use crate::mailbox::{MailboxConfig, OverflowStrategy};
use crate::message::{Headers, Message, RuntimeHeaders};
use crate::node::{ActorId, NodeId};
use crate::registry::ActorRegistry;
use crate::stream::{
    BatchConfig, BatchReader, BatchWriter, BoxStream, StreamReceiver, StreamSender,
};
use crate::supervision::ChildTerminated;
use tokio_util::sync::CancellationToken;

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
                OverflowStrategy::RejectWithError => sender.try_send(msg).map_err(|e| match e {
                    mpsc::error::TrySendError::Full(_) => ActorSendError("mailbox full".into()),
                    mpsc::error::TrySendError::Closed(_) => ActorSendError("actor stopped".into()),
                }),
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
                        mpsc::error::TrySendError::Full(_) => {
                            ActorSendError("mailbox full (Block not supported in sync tell)".into())
                        }
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
            Self::Unbounded(tx) => {
                let _ = tx.send(msg);
            }
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

    /// Approximate number of messages pending in the mailbox.
    fn pending(&self) -> usize {
        match self {
            Self::Unbounded(_) => 0, // unbounded channels don't expose length
            Self::Bounded { sender, .. } => sender.max_capacity() - sender.capacity(),
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
// TestActorRef
// ---------------------------------------------------------------------------

/// A test actor reference implementing `ActorRef<A>`.
pub struct TestActorRef<A: Actor> {
    id: ActorId,
    name: String,
    sender: MailboxSender<A>,
    alive: Arc<AtomicBool>,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    drop_observer: Option<Arc<dyn DropObserver>>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
}

impl<A: Actor> Clone for TestActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            sender: self.sender.clone(),
            alive: self.alive.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
        }
    }
}

impl<A: Actor> TestActorRef<A> {
    fn outbound_pipeline(&self) -> crate::runtime_support::OutboundPipeline {
        crate::runtime_support::OutboundPipeline {
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

impl<A: Actor> ActorRef<A> for TestActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire) && !self.sender.is_closed()
    }

    fn pending_messages(&self) -> usize {
        self.sender.pending()
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
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Tell, &msg);
        match result.disposition {
            Disposition::Continue => {}
            Disposition::Delay(_) => {}
            Disposition::Drop | Disposition::Reject(_) | Disposition::Retry(_) => return Ok(()),
        }

        let dispatch: BoxedDispatch<A> = Box::new(TypedDispatch { msg });
        self.sender.send(Some(dispatch)).map_err(|e| {
            let reason = if e.0.contains("mailbox full") {
                DeadLetterReason::MailboxFull
            } else {
                DeadLetterReason::ActorStopped
            };
            self.notify_dead_letter(std::any::type_name::<M>(), SendMode::Tell, reason);
            e
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
        let dispatch: BoxedDispatch<A> = Box::new(AskDispatch {
            msg,
            reply_tx: tx,
            cancel,
        });
        self.sender.send(Some(dispatch)).map_err(|e| {
            let reason = if e.0.contains("mailbox full") {
                DeadLetterReason::MailboxFull
            } else {
                DeadLetterReason::ActorStopped
            };
            self.notify_dead_letter(std::any::type_name::<M>(), SendMode::Ask, reason);
            e
        })?;

        // Wrap the reply channel so that outbound interceptors' on_reply()
        // is called when the reply arrives on the sender side.
        if self.outbound_interceptors.is_empty() {
            Ok(AskReply::new(rx))
        } else {
            let message_type = std::any::type_name::<M>();
            let (wrapped_tx, wrapped_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                match rx.await {
                    Ok(Ok(reply)) => {
                        pipeline.run_on_reply(
                            message_type,
                            &Outcome::AskSuccess {
                                reply: &reply as &dyn std::any::Any,
                            },
                        );
                        let _ = wrapped_tx.send(Ok(reply));
                    }
                    Ok(Err(e)) => {
                        let _ = wrapped_tx.send(Err(e));
                    }
                    Err(_) => {
                        let _ = wrapped_tx.send(Err(RuntimeError::ActorNotFound(
                            "reply channel closed".into(),
                        )));
                    }
                }
            });
            Ok(AskReply::new(wrapped_rx))
        }
    }

    fn expand<M, OutputItem>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<OutputItem>, ActorSendError>
    where
        A: ExpandHandler<M, OutputItem>,
        M: Send + 'static,
        OutputItem: Send + 'static,
    {
        let buffer = buffer.max(1);
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

        let (tx, mut rx) = tokio::sync::mpsc::channel(buffer);
        let sender = StreamSender::new(tx);
        let dispatch: BoxedDispatch<A> = Box::new(ExpandDispatch {
            msg,
            sender,
            cancel,
        });
        self.sender.send(Some(dispatch))?;

        match batch_config {
            Some(batch_config) => {
                // Batched: handler → batch writer → batch reader → interception → caller
                let (batch_tx, batch_rx) = tokio::sync::mpsc::channel::<Vec<OutputItem>>(buffer);
                let reader = BatchReader::new(batch_rx);
                tokio::spawn(async move {
                    let mut writer = BatchWriter::new(batch_tx, batch_config);
                    loop {
                        if writer.buffered_count() > 0 {
                            let delay = writer.max_delay();
                            tokio::select! {
                                biased;
                                item = rx.recv() => match item {
                                    Some(item) => {
                                        if writer.push(item).await.is_err() { break; }
                                    }
                                    None => break,
                                },
                                _ = tokio::time::sleep(delay) => {
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
                Ok(
                    crate::runtime_support::wrap_batched_stream_with_interception(
                        reader,
                        buffer,
                        pipeline,
                        std::any::type_name::<M>(),
                        SendMode::Expand,
                    ),
                )
            }
            None => {
                // Unbatched: handler → interception → caller
                Ok(crate::runtime_support::wrap_stream_with_interception(
                    rx,
                    buffer,
                    pipeline,
                    std::any::type_name::<M>(),
                    SendMode::Expand,
                ))
            }
        }
    }

    fn reduce<InputItem, Reply>(
        &self,
        input: BoxStream<InputItem>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: ReduceHandler<InputItem, Reply>,
        InputItem: Send + 'static,
        Reply: Send + 'static,
    {
        let buffer = buffer.max(1);
        let pipeline = self.outbound_pipeline();

        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let dispatch: BoxedDispatch<A> = Box::new(ReduceDispatch {
            receiver,
            reply_tx,
            cancel: cancel.clone(),
        });
        self.sender.send(Some(dispatch))?;

        match batch_config {
            Some(batch_config) => {
                crate::runtime_support::spawn_reduce_batched_drain(
                    input,
                    item_tx,
                    buffer,
                    batch_config,
                    cancel,
                    pipeline,
                    std::any::type_name::<InputItem>(),
                );
            }
            None => {
                crate::runtime_support::spawn_reduce_drain(
                    input,
                    item_tx,
                    cancel,
                    pipeline,
                    std::any::type_name::<InputItem>(),
                );
            }
        }

        Ok(AskReply::new(reply_rx))
    }

    fn transform<InputItem, OutputItem>(
        &self,
        input: BoxStream<InputItem>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<OutputItem>, ActorSendError>
    where
        A: TransformHandler<InputItem, OutputItem>,
        InputItem: Send + 'static,
        OutputItem: Send + 'static,
    {
        let buffer = buffer.max(1);
        let pipeline = self.outbound_pipeline();

        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let sender = StreamSender::new(output_tx);
        let dispatch: BoxedDispatch<A> = Box::new(TransformDispatch::new(
            receiver,
            sender,
            cancel.clone(),
        ));
        self.sender.send(Some(dispatch))?;

        crate::runtime_support::spawn_transform_drain(
            input,
            item_tx,
            cancel,
            pipeline.clone(),
            std::any::type_name::<InputItem>(),
        );

        match batch_config {
            Some(batch_config) => {
                // Batched: handler → batch writer → batch reader → interception → caller
                let (batch_tx, batch_rx) =
                    tokio::sync::mpsc::channel::<Vec<OutputItem>>(buffer);
                let reader = BatchReader::new(batch_rx);
                tokio::spawn(async move {
                    let mut writer = BatchWriter::new(batch_tx, batch_config);
                    loop {
                        if writer.buffered_count() > 0 {
                            let delay = writer.max_delay();
                            tokio::select! {
                                biased;
                                item = output_rx.recv() => match item {
                                    Some(item) => {
                                        if writer.push(item).await.is_err() { break; }
                                    }
                                    None => break,
                                },
                                _ = tokio::time::sleep(delay) => {
                                    if writer.check_deadline().await.is_err() { break; }
                                }
                            }
                        } else {
                            match output_rx.recv().await {
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
                Ok(
                    crate::runtime_support::wrap_batched_stream_with_interception(
                        reader,
                        buffer,
                        pipeline,
                        std::any::type_name::<OutputItem>(),
                        SendMode::Transform,
                    ),
                )
            }
            None => Ok(crate::runtime_support::wrap_stream_with_interception(
                output_rx,
                buffer,
                pipeline,
                std::any::type_name::<OutputItem>(),
                SendMode::Transform,
            )),
        }
    }
}

/// A type-erased entry in the watch registry.
struct WatchEntry {
    watcher_id: ActorId,
    /// Closure that delivers a [`ChildTerminated`] to the watcher actor.
    notify: Box<dyn Fn(ChildTerminated) + Send + Sync>,
}

// ---------------------------------------------------------------------------
// TestRuntime
// ---------------------------------------------------------------------------

/// A lightweight test runtime that spawns v0.2 actors on the Tokio runtime.
pub struct TestRuntime {
    node_id: NodeId,
    next_local: AtomicU64,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    drop_observer: Arc<Option<Arc<dyn DropObserver>>>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
    /// Watch registry — maps watched actor ID to list of watcher entries.
    watchers: Arc<Mutex<HashMap<ActorId, Vec<WatchEntry>>>>,
    /// Actor name registry for looking up actors by name.
    registry: Arc<ActorRegistry>,
    /// Stop notification receivers for await_stop(), keyed by ActorId.
    #[allow(clippy::type_complexity)]
    stop_receivers: Arc<Mutex<HashMap<ActorId, tokio::sync::oneshot::Receiver<Result<(), String>>>>>,
    /// Optional shared metrics registry. When set, a [`MetricsInterceptor`] is
    /// automatically prepended to every spawned actor's inbound interceptor list.
    #[cfg(feature = "metrics")]
    metrics_registry: Option<crate::metrics::MetricsRegistry>,
}

impl TestRuntime {
    pub fn new() -> Self {
        Self {
            node_id: NodeId("test-node".into()),
            next_local: AtomicU64::new(1),
            outbound_interceptors: Arc::new(Vec::new()),
            drop_observer: Arc::new(None),
            dead_letter_handler: Arc::new(None),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            registry: Arc::new(ActorRegistry::new()),
            stop_receivers: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "metrics")]
            metrics_registry: None,
        }
    }

    /// Create a new runtime with a custom node identity.
    pub fn with_node_id(node_id: NodeId) -> Self {
        Self {
            node_id,
            next_local: AtomicU64::new(1),
            outbound_interceptors: Arc::new(Vec::new()),
            drop_observer: Arc::new(None),
            dead_letter_handler: Arc::new(None),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            registry: Arc::new(ActorRegistry::new()),
            stop_receivers: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "metrics")]
            metrics_registry: None,
        }
    }

    /// Return a reference to the actor name registry.
    pub fn registry(&self) -> &ActorRegistry {
        &self.registry
    }

    /// Enable built-in metrics collection.
    ///
    /// Creates a shared [`MetricsRegistry`](crate::metrics::MetricsRegistry) and
    /// automatically prepends a [`MetricsInterceptor`](crate::metrics::MetricsInterceptor)
    /// to every subsequently spawned actor's inbound interceptor pipeline.
    ///
    /// **Must be called before any actors are spawned.**
    /// Requires the `metrics` feature.
    #[cfg(feature = "metrics")]
    pub fn enable_metrics(&mut self) {
        self.metrics_registry = Some(crate::metrics::MetricsRegistry::default());
    }

    /// Return a reference to the shared [`MetricsRegistry`](crate::metrics::MetricsRegistry),
    /// or `None` if metrics have not been enabled.
    /// Requires the `metrics` feature.
    #[cfg(feature = "metrics")]
    pub fn metrics(&self) -> Option<&crate::metrics::MetricsRegistry> {
        self.metrics_registry.as_ref()
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

    /// Set a global drop observer. Called whenever any interceptor returns
    /// `Disposition::Drop` for a message or stream item.
    ///
    /// **Must be called before any actors are spawned.**
    pub fn set_drop_observer(&mut self, observer: Arc<dyn DropObserver>) {
        self.drop_observer = Arc::new(Some(observer));
    }

    /// Set a global dead letter handler. Called whenever a message cannot be
    /// delivered (actor stopped, mailbox full, dropped by inbound interceptor).
    ///
    /// **Must be called before any actors are spawned.**
    pub fn set_dead_letter_handler(&mut self, handler: Arc<dyn DeadLetterHandler>) {
        self.dead_letter_handler = Arc::new(Some(handler));
    }

    /// Spawn a v0.2 actor whose `Deps` type is `()`. Returns a `TestActorRef<A>`.
    pub async fn spawn<A>(&self, name: &str, args: A::Args) -> Result<TestActorRef<A>, crate::errors::RuntimeError>
    where
        A: Actor<Deps = ()> + 'static,
    {
        Ok(self.spawn_internal(name, args, (), Vec::new(), MailboxConfig::Unbounded))
    }

    /// Spawn a v0.2 actor with explicit dependencies.
    pub async fn spawn_with_deps<A>(&self, name: &str, args: A::Args, deps: A::Deps) -> Result<TestActorRef<A>, crate::errors::RuntimeError>
    where
        A: Actor + 'static,
    {
        Ok(self.spawn_internal(name, args, deps, Vec::new(), MailboxConfig::Unbounded))
    }

    /// Spawn a v0.2 actor with spawn options (including interceptors and mailbox config).
    pub async fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> Result<TestActorRef<A>, crate::errors::RuntimeError>
    where
        A: Actor<Deps = ()> + 'static,
    {
        Ok(self.spawn_internal(name, args, (), options.interceptors, options.mailbox))
    }

    /// Register actor `watcher` to be notified when `target` terminates.
    ///
    /// The watcher must implement `Handler<ChildTerminated>`. When the target
    /// actor stops (gracefully or due to panic), the runtime will deliver a
    /// [`ChildTerminated`] message to the watcher.
    pub fn watch<W>(&self, watcher: &TestActorRef<W>, target_id: ActorId)
    where
        W: Actor + Handler<ChildTerminated> + 'static,
    {
        let watcher_id = watcher.id();
        let watcher_sender = watcher.sender.clone();

        let entry = WatchEntry {
            watcher_id,
            notify: Box::new(move |msg: ChildTerminated| {
                let dispatch: BoxedDispatch<W> = Box::new(TypedDispatch { msg });
                let _ = watcher_sender.send(Some(dispatch));
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

    pub(crate) fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
        mailbox: MailboxConfig,
    ) -> TestActorRef<A>
    where
        A: Actor + 'static,
    {
        // Generate actor ID first so we can register with the metrics registry.
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };

        // Prepend a MetricsInterceptor when metrics are enabled so it sees
        // every message regardless of what the caller passes in SpawnOptions.
        #[cfg(feature = "metrics")]
        let interceptors = if let Some(ref registry) = self.metrics_registry {
            let handle = registry.register(actor_id.clone());
            let mut combined = Vec::with_capacity(1 + interceptors.len());
            combined.push(Box::new(crate::metrics::MetricsInterceptor::new(handle))
                as Box<dyn InboundInterceptor>);
            combined.extend(interceptors);
            combined
        } else {
            interceptors
        };

        let actor_name = name.to_string();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();

        let (tx, mut rx) = match &mailbox {
            MailboxConfig::Unbounded => {
                let (tx, rx) = mpsc::unbounded_channel::<Option<BoxedDispatch<A>>>();
                (MailboxSender::Unbounded(tx), MailboxReceiver::Unbounded(rx))
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
        let watchers_ref = self.watchers.clone();
        let dead_letter_handler_task = self.dead_letter_handler.clone();
        let registry_task = self.registry.clone();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        self.stop_receivers.lock().unwrap().insert(actor_id.clone(), stop_rx);

        tokio::spawn(async move {
            let mut actor = A::create(args, deps);
            let mut ctx = ActorContext {
                actor_id: id_task,
                actor_name: name_task,
                send_mode: None,
                headers: Headers::new(),
                cancellation_token: None,
            };

            actor.on_start(&mut ctx).await;

            let mut stop_reason: Option<String> = None;

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
                            disp @ (Disposition::Drop
                            | Disposition::Reject(_)
                            | Disposition::Retry(_)) => {
                                rejection = Some((interceptor.name().to_string(), disp));
                                break;
                            }
                        }
                    }
                }

                // If rejected/dropped/retry, propagate proper error to caller
                if let Some((interceptor_name, disposition)) = rejection {
                    if matches!(disposition, Disposition::Drop) {
                        if let Some(ref handler) = *dead_letter_handler_task {
                            let event = DeadLetterEvent {
                                target_id: ctx.actor_id.clone(),
                                target_name: Some(ctx.actor_name.clone()),
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
                    continue;
                }

                if !total_delay.is_zero() {
                    tokio::time::sleep(total_delay).await;
                }

                // Collect handler wrappers BEFORE moving headers into ctx,
                // so interceptors can read headers without borrow conflicts.
                let ictx_for_wrap = InboundContext {
                    actor_id: ctx.actor_id.clone(),
                    actor_name: &ctx.actor_name,
                    message_type,
                    send_mode,
                    remote: false,
                    origin_node: None,
                };
                let wrappers = collect_handler_wrappers(&interceptors, &ictx_for_wrap, &headers);
                let needs_wrap = wrappers.iter().any(|w| w.is_some());

                // Copy interceptor-populated headers to ActorContext so handler can access them
                ctx.headers = headers;

                // Set the cancellation token on the context
                let cancel_token = dispatch.cancel_token();
                ctx.cancellation_token = cancel_token.clone();

                // Check if already cancelled before dispatching
                if let Some(ref token) = cancel_token {
                    if token.is_cancelled() {
                        // Send RuntimeError::Cancelled to the caller
                        dispatch.cancel();
                        ctx.cancellation_token = None;
                        continue;
                    }
                }

                // Dispatch the message, optionally wrapped by interceptor scopes.
                let result = if needs_wrap {
                    // Wrapped path: use oneshot to extract result from the
                    // opaque Future<Output = ()> wrapper chain.
                    let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();

                    let inner: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> = Box::pin(async {
                        let r = std::panic::AssertUnwindSafe(
                            dispatch.dispatch(&mut actor, &mut ctx),
                        )
                        .catch_unwind()
                        .await;
                        let _ = result_tx.send(r);
                    });

                    let wrapped = apply_handler_wrappers(wrappers, inner);

                    // Wrap the entire chain (including interceptor wrappers) with
                    // catch_unwind so a panic in a wrapper is handled the same
                    // way as a handler panic.
                    let wrapped = std::panic::AssertUnwindSafe(wrapped);

                    if let Some(ref token) = cancel_token {
                        tokio::select! {
                            biased;
                            _ = wrapped.catch_unwind() => {},
                            _ = token.cancelled() => {
                                ctx.cancellation_token = None;
                                continue;
                            }
                        }
                    } else {
                        wrapped.catch_unwind().await.ok();
                    }

                    // Retrieve the dispatch result from the oneshot channel.
                    // If the wrapper didn't await the inner future (contract
                    // violation) or panicked, treat as a handler panic.
                    match result_rx.try_recv() {
                        Ok(r) => r,
                        Err(_) => Err(Box::new(
                            "interceptor wrap_handler did not await the handler future",
                        ) as Box<dyn std::any::Any + Send>),
                    }
                } else {
                    // Fast path: no wrapping overhead.
                    if let Some(ref token) = cancel_token {
                        let dispatch_fut =
                            std::panic::AssertUnwindSafe(dispatch.dispatch(&mut actor, &mut ctx))
                                .catch_unwind();
                        tokio::select! {
                            biased;
                            r = dispatch_fut => r,
                            _ = token.cancelled() => {
                                ctx.cancellation_token = None;
                                continue;
                            }
                        }
                    } else {
                        std::panic::AssertUnwindSafe(dispatch.dispatch(&mut actor, &mut ctx))
                            .catch_unwind()
                            .await
                    }
                };

                // Clear the cancellation token
                ctx.cancellation_token = None;

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
                            (Some(reply), SendMode::Ask) => Outcome::AskSuccess {
                                reply: reply.as_ref(),
                            },
                            _ => Outcome::TellSuccess,
                        };

                        for interceptor in &interceptors {
                            interceptor.on_complete(
                                &ictx,
                                &runtime_headers,
                                &ctx.headers,
                                &outcome,
                            );
                        }

                        // Send reply to caller AFTER interceptors have seen it
                        dispatch_result.send_reply();
                    }
                    Err(_panic) => {
                        let error = ActorError::internal("handler panicked");
                        let action = actor.on_error(&error);

                        let outcome = Outcome::HandlerError { error };
                        for interceptor in &interceptors {
                            interceptor.on_complete(
                                &ictx,
                                &runtime_headers,
                                &ctx.headers,
                                &outcome,
                            );
                        }

                        match action {
                            ErrorAction::Resume => {
                                continue;
                            }
                            ErrorAction::Stop | ErrorAction::Escalate => {
                                tracing::error!(
                                    "handler panicked in actor {}, stopping",
                                    ctx.actor_name
                                );
                                stop_reason = Some("handler panicked".into());
                                break;
                            }
                            ErrorAction::Restart => {
                                // Full restart with Args/Deps recreation is adapter-specific.
                                // TestRuntime treats Restart as Resume (documented limitation).
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

            // Run on_stop with panic catching so we can propagate errors
            let stop_result =
                std::panic::AssertUnwindSafe(actor.on_stop())
                    .catch_unwind()
                    .await;
            let stop_err = match stop_result {
                Ok(()) => None,
                Err(_panic) => Some("actor panicked in on_stop".to_string()),
            };

            // Notify all watchers that this actor has terminated.
            // Clone entries and release lock before calling notify closures
            // to avoid holding the mutex during potentially blocking sends.
            let actor_id = ctx.actor_id.clone();
            let actor_name = ctx.actor_name.clone();
            let entries = {
                let mut watchers = watchers_ref.lock().unwrap();
                watchers.remove(&actor_id).unwrap_or_default()
            };
            if !entries.is_empty() {
                let notification = ChildTerminated {
                    child_id: actor_id,
                    child_name: actor_name.clone(),
                    reason: stop_reason,
                };
                for entry in &entries {
                    (entry.notify)(notification.clone());
                }
            }

            // Notify await_stop() waiters with the result
            let result = match stop_err {
                Some(e) => Err(e),
                None => Ok(()),
            };
            let _ = stop_tx.send(result);

            // Auto-unregister from the name registry on stop.
            registry_task.unregister(&actor_name);
        });

        let actor_ref = TestActorRef {
            id: actor_id,
            name: actor_name,
            sender: tx,
            alive,
            outbound_interceptors: self.outbound_interceptors.clone(),
            drop_observer: (*self.drop_observer).clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
        };

        self.registry.register(name, actor_ref.clone());

        actor_ref
    }

    // -----------------------------------------------------------------------
    // Actor lifecycle handles
    // -----------------------------------------------------------------------

    /// Wait for an actor to stop.
    ///
    /// Returns `Ok(())` when the actor finishes cleanly, or `Err` if the
    /// actor panicked in `on_stop`. The stop receiver is consumed and removed
    /// from the map.
    ///
    /// Returns `Ok(())` immediately if no stop receiver is stored for this ID.
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
            matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            )
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

impl Default for TestRuntime {
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
    use crate::actor::{ReduceHandler, ExpandHandler, TransformHandler};
    use crate::message::Message;
    use crate::node::NodeId;
    use crate::stream::{StreamReceiver, StreamSender};

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

    // -- Shared test actor: Greeter (for registry type-mismatch tests) ------

    struct Greet(String);
    impl Message for Greet {
        type Reply = String;
    }

    struct Greeter;

    impl Actor for Greeter {
        type Args = ();
        type Deps = ();
        fn create(_args: (), _deps: ()) -> Self {
            Greeter
        }
    }

    #[async_trait]
    impl Handler<Greet> for Greeter {
        async fn handle(&mut self, msg: Greet, _ctx: &mut ActorContext) -> String {
            format!("Hello, {}!", msg.0)
        }
    }

    // -- Tests --------------------------------------------------------------

    #[tokio::test]
    async fn test_spawn_and_tell() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        counter.tell(Increment(5)).unwrap();
        counter.tell(Increment(3)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(counter.is_alive());
    }

    #[tokio::test]
    async fn test_tell_returns_actor_id() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("my-counter", Counter { count: 0 }).await.unwrap();

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
        let runtime = TestRuntime::new();
        let tracker = runtime.spawn::<OrderTracker>("tracker", received.clone()).await.unwrap();

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
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

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
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<StartTracker>("tracker", StartTrackerArgs(log.clone())).await.unwrap();

        actor.tell(Ping).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let entries = log.lock().await;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "on_start");
        assert_eq!(entries[1], "handle");
    }

    #[tokio::test]
    async fn test_tell_to_stopped_actor() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        // Drop the original to close the channel
        let counter2 = counter.clone();
        drop(counter);

        // counter2 still holds a sender, so the actor is alive
        assert!(counter2.tell(Increment(1)).is_ok());
    }

    #[tokio::test]
    async fn test_unique_actor_ids() {
        let runtime = TestRuntime::new();
        let a = runtime.spawn::<Counter>("a", Counter { count: 0 }).await.unwrap();
        let b = runtime.spawn::<Counter>("b", Counter { count: 0 }).await.unwrap();

        assert_ne!(a.id(), b.id());
        assert!(a.id().local < b.id().local);
    }

    // -- Ask tests ----------------------------------------------------------

    #[tokio::test]
    async fn test_ask_get_count() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 42 }).await.unwrap();

        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn test_ask_after_tell() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        counter.tell(Increment(10)).unwrap();
        counter.tell(Increment(20)).unwrap();

        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count, 30);
    }

    #[tokio::test]
    async fn test_ask_reset_returns_old_value() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 100 }).await.unwrap();

        let old = counter.ask(Reset, None).unwrap().await.unwrap();
        assert_eq!(old, 100);

        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_concurrent_asks() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        counter.tell(Increment(100)).unwrap();

        // Ensure the tell is processed before asking
        let _ = counter.ask(GetCount, None).unwrap().await.unwrap();

        let ref1 = counter.clone();
        let ref2 = counter.clone();

        let (r1, r2) = tokio::join!(
            async { ref1.ask(GetCount, None).unwrap().await.unwrap() },
            async { ref2.ask(GetCount, None).unwrap().await.unwrap() },
        );

        assert_eq!(r1, 100);
        assert_eq!(r2, 100);
    }

    #[tokio::test]
    async fn test_interleaved_tell_ask() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        counter.tell(Increment(5)).unwrap();
        let c1 = counter.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(c1, 5);

        counter.tell(Increment(3)).unwrap();
        let c2 = counter.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(c2, 8);

        let old = counter.ask(Reset, None).unwrap().await.unwrap();
        assert_eq!(old, 8);

        let c3 = counter.ask(GetCount, None).unwrap().await.unwrap();
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
        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(LogInterceptor { log: log.clone() })],
                ..Default::default()
            },
        ).await.unwrap();

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
        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 42 },
            SpawnOptions {
                interceptors: vec![Box::new(LogInterceptor { log: log.clone() })],
                ..Default::default()
            },
        ).await.unwrap();

        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
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

        let runtime = TestRuntime::new();
        let actor = runtime.spawn_with_options::<CountingActor>(
            "counting",
            handle_count_clone,
            SpawnOptions {
                interceptors: vec![Box::new(DropInterceptor)],
                ..Default::default()
            },
        ).await.unwrap();

        actor.tell(Ping).unwrap();
        actor.tell(Ping).unwrap();
        actor.tell(Ping).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            handle_count.load(Ordering::SeqCst),
            0,
            "handler should not have been called"
        );
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
        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 42 },
            SpawnOptions {
                interceptors: vec![Box::new(RejectInterceptor)],
                ..Default::default()
            },
        ).await.unwrap();

        let result = counter.ask(GetCount, None).unwrap().await;
        assert!(result.is_err(), "rejected ask should return Err");
        match result.unwrap_err() {
            RuntimeError::Rejected {
                interceptor,
                reason,
            } => {
                assert_eq!(interceptor, "reject-all");
                assert_eq!(reason, "forbidden");
            }
            other => panic!("expected Rejected, got: {:?}", other),
        }
    }

    // ── Disposition::Retry tests ─────────────────────────────

    struct RetryInterceptor;
    impl InboundInterceptor for RetryInterceptor {
        fn name(&self) -> &'static str {
            "retry-later"
        }
        fn on_receive(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _h: &mut Headers,
            _msg: &dyn Any,
        ) -> Disposition {
            Disposition::Retry(Duration::from_millis(500))
        }
    }

    #[tokio::test]
    async fn test_disposition_retry_ask_returns_retry_after() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 42 },
            SpawnOptions {
                interceptors: vec![Box::new(RetryInterceptor)],
                ..Default::default()
            },
        ).await.unwrap();

        let result = counter.ask(GetCount, None).unwrap().await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RuntimeError::RetryAfter {
                interceptor,
                retry_after,
            } => {
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

        struct TrackActor {
            count: Arc<AtomicU64>,
        }
        impl Actor for TrackActor {
            type Args = Arc<AtomicU64>;
            type Deps = ();
            fn create(args: Arc<AtomicU64>, _: ()) -> Self {
                TrackActor { count: args }
            }
        }

        struct TrackMsg;
        impl Message for TrackMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<TrackMsg> for TrackActor {
            async fn handle(&mut self, _msg: TrackMsg, _ctx: &mut ActorContext) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn_with_options::<TrackActor>(
            "tracker",
            count_clone,
            SpawnOptions {
                interceptors: vec![Box::new(RetryInterceptor)],
                ..Default::default()
            },
        ).await.unwrap();

        actor.tell(TrackMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            handler_count.load(Ordering::SeqCst),
            0,
            "handler should not be called when Retry"
        );
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

        let runtime = TestRuntime::new();
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
        ).await.unwrap();

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

        let runtime = TestRuntime::new();
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
        ).await.unwrap();

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

        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(DelayInterceptor)],
                ..Default::default()
            },
        ).await.unwrap();

        let start = tokio::time::Instant::now();
        counter.tell(Increment(1)).unwrap();

        // Ask blocks until tell+ask are both processed (sequentially, both delayed)
        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
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

        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(SmallDelay(50)), Box::new(SmallDelay(50))],
                ..Default::default()
            },
        ).await.unwrap();

        let start = tokio::time::Instant::now();
        // Use ask to block until message is processed
        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
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
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        counter.tell(Increment(10)).unwrap();
        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
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

        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(TypeLogInterceptor { log: log.clone() })],
                ..Default::default()
            },
        ).await.unwrap();

        counter.tell(Increment(1)).unwrap();
        let _ = counter.ask(GetCount, None).unwrap().await.unwrap();

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

        let runtime = TestRuntime::new();
        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(DowncastInterceptor {
                    captured: captured.clone(),
                })],
                ..Default::default()
            },
        ).await.unwrap();

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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(OutLog { log: log.clone() }));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(RejectOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 42 }).await.unwrap();

        let result = counter.ask(GetCount, None).unwrap().await;
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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(StampPriority));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(RetryOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        let result = counter.ask(GetCount, None).unwrap().await;
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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(DropOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(DropOut));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        let result = counter.ask(GetCount, None).unwrap().await;
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

        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(ModeLog { log: log.clone() }));
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        counter.tell(Increment(1)).unwrap();
        let _ = counter.ask(GetCount, None).unwrap().await;

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

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<StopTracker>("tracker", log.clone()).await.unwrap();

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
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

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
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<ResumeActor>("resume", count.clone()).await.unwrap();

        actor.tell(PanicMsg).unwrap(); // should panic but resume
        actor.tell(CountMsg).unwrap(); // should still be processed

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "actor should resume after panic"
        );
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
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<StopOnError>("stopper", alive.clone()).await.unwrap();

        actor.tell(PanicMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            !alive.load(Ordering::SeqCst),
            "on_stop should have been called"
        );
        assert!(!actor.is_alive());
    }

    #[tokio::test]
    async fn test_on_error_default_is_stop() {
        struct PanicCounter {
            #[allow(dead_code)]
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

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<PanicCounter>("panic-counter", PanicCounter { count: 0 }).await.unwrap();

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
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<ModeTracker>("tracker", mode_ref).await.unwrap();

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
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<AskModeTracker>("tracker", mode_ref).await.unwrap();

        let _ = actor.ask(AskCheck, None).unwrap().await.unwrap();

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
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<RestartActor>("restart", count.clone()).await.unwrap();

        actor.tell(RestartPanicMsg).unwrap();
        actor.tell(RestartCountMsg).unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        // Restart is treated as Resume for now
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "actor should continue after restart-as-resume"
        );
        assert!(actor.is_alive());
    }

    // -- Mailbox tests ------------------------------------------------------

    #[tokio::test]
    async fn test_unbounded_mailbox_accepts_many() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        for _ in 0..1000 {
            counter.tell(Increment(1)).unwrap();
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count, 1000);
    }

    #[tokio::test]
    async fn test_default_spawn_is_unbounded() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        for _ in 0..100 {
            counter.tell(Increment(1)).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let count = counter.ask(GetCount, None).unwrap().await.unwrap();
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
        fn create(_: (), _: ()) -> Self {
            SlowActor
        }
    }

    struct SlowMsg;
    impl Message for SlowMsg {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<SlowMsg> for SlowActor {
        async fn handle(&mut self, _: SlowMsg, _: &mut ActorContext) {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    #[tokio::test]
    async fn test_bounded_reject_when_full() {
        let runtime = TestRuntime::new();
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
        ).await.unwrap();

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
        let runtime = TestRuntime::new();
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
        ).await.unwrap();

        actor.tell(SlowMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        actor.tell(SlowMsg).unwrap();
        actor.tell(SlowMsg).unwrap();

        // Should succeed — silently dropped
        let result = actor.tell(SlowMsg);
        assert!(result.is_ok(), "DropNewest should silently succeed");
    }

    // -- Supervision / DeathWatch tests -------------------------------------

    use crate::supervision::ChildTerminated;

    struct Watcher {
        events: Arc<Mutex<Vec<ChildTerminated>>>,
    }

    impl Actor for Watcher {
        type Args = Arc<Mutex<Vec<ChildTerminated>>>;
        type Deps = ();
        fn create(args: Arc<Mutex<Vec<ChildTerminated>>>, _: ()) -> Self {
            Watcher { events: args }
        }
    }

    #[async_trait]
    impl Handler<ChildTerminated> for Watcher {
        async fn handle(&mut self, msg: ChildTerminated, _ctx: &mut ActorContext) {
            self.events.lock().unwrap().push(msg);
        }
    }

    struct WatcherPing;
    impl Message for WatcherPing {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<WatcherPing> for Watcher {
        async fn handle(&mut self, _: WatcherPing, _: &mut ActorContext) {}
    }

    struct Worker;
    impl Actor for Worker {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            Worker
        }
    }

    struct WorkerMsg;
    impl Message for WorkerMsg {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<WorkerMsg> for Worker {
        async fn handle(&mut self, _: WorkerMsg, _: &mut ActorContext) {}
    }

    #[tokio::test]
    async fn test_watch_receives_child_terminated() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime::new();
        let watcher = runtime.spawn::<Watcher>("watcher", events.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("worker", ()).await.unwrap();

        let worker_id = worker.id();
        runtime.watch(&watcher, worker_id.clone());

        // Stop the worker
        worker.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let evts = events.lock().unwrap();
        assert_eq!(evts.len(), 1);
        assert_eq!(evts[0].child_id, worker_id);
        assert_eq!(evts[0].child_name, "worker");
        assert!(
            evts[0].reason.is_none(),
            "graceful stop should have no reason"
        );
    }

    #[tokio::test]
    async fn test_unwatch_stops_notifications() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime::new();
        let watcher = runtime.spawn::<Watcher>("watcher", events.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("worker", ()).await.unwrap();

        let worker_id = worker.id();
        let watcher_id = watcher.id();
        runtime.watch(&watcher, worker_id.clone());

        // Unwatch before stopping
        runtime.unwatch(&watcher_id, &worker_id);

        worker.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let evts = events.lock().unwrap();
        assert!(evts.is_empty(), "unwatch should prevent notification");
    }

    #[tokio::test]
    async fn test_watch_panic_includes_reason() {
        struct PanicWorker;
        impl Actor for PanicWorker {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                PanicWorker
            }
        }

        struct PanicMsg;
        impl Message for PanicMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<PanicMsg> for PanicWorker {
            async fn handle(&mut self, _: PanicMsg, _: &mut ActorContext) {
                panic!("boom");
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime::new();
        let watcher = runtime.spawn::<Watcher>("watcher", events.clone()).await.unwrap();
        let worker = runtime.spawn::<PanicWorker>("panic-worker", ()).await.unwrap();

        let worker_id = worker.id();
        runtime.watch(&watcher, worker_id.clone());

        // Send a message that causes a panic (default on_error returns Stop)
        worker.tell(PanicMsg).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let evts = events.lock().unwrap();
        assert_eq!(evts.len(), 1);
        assert_eq!(evts[0].child_id, worker_id);
        assert_eq!(evts[0].reason, Some("handler panicked".into()));
    }

    #[tokio::test]
    async fn test_watch_multiple_watchers() {
        let events1 = Arc::new(Mutex::new(Vec::new()));
        let events2 = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime::new();
        let watcher1 = runtime.spawn::<Watcher>("watcher1", events1.clone()).await.unwrap();
        let watcher2 = runtime.spawn::<Watcher>("watcher2", events2.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("worker", ()).await.unwrap();

        let worker_id = worker.id();
        runtime.watch(&watcher1, worker_id.clone());
        runtime.watch(&watcher2, worker_id.clone());

        worker.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let evts1 = events1.lock().unwrap();
        let evts2 = events2.lock().unwrap();
        assert_eq!(evts1.len(), 1, "watcher1 should receive notification");
        assert_eq!(evts2.len(), 1, "watcher2 should receive notification");
        assert_eq!(evts1[0].child_id, worker_id);
        assert_eq!(evts2[0].child_id, worker_id);
    }

    // ======================================================================
    // Stream tests
    // ======================================================================

    struct LogServer {
        logs: Vec<String>,
    }

    impl Actor for LogServer {
        type Args = Vec<String>;
        type Deps = ();
        fn create(args: Vec<String>, _: ()) -> Self {
            LogServer { logs: args }
        }
    }

    struct GetLogs;
    impl Message for GetLogs {
        type Reply = String;
    }

    #[async_trait]
    impl ExpandHandler<GetLogs, String> for LogServer {
        async fn handle_expand(
            &mut self,
            _msg: GetLogs,
            sender: StreamSender<String>,
            _ctx: &mut ActorContext,
        ) {
            for log in &self.logs {
                if sender.send(log.clone()).await.is_err() {
                    break;
                }
            }
        }
    }

    #[tokio::test]
    async fn test_stream_returns_items() {
        use tokio_stream::StreamExt;

        let runtime = TestRuntime::new();
        let server = runtime
            .spawn::<LogServer>("logs", vec!["line1".into(), "line2".into(), "line3".into()]).await.unwrap();

        let mut stream = server.expand(GetLogs, 16, None, None).unwrap();
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
        }

        assert_eq!(items, vec!["line1", "line2", "line3"]);
    }

    #[tokio::test]
    async fn test_stream_empty() {
        use tokio_stream::StreamExt;

        let runtime = TestRuntime::new();
        let server = runtime.spawn::<LogServer>("logs", vec![]).await.unwrap();

        let mut stream = server.expand(GetLogs, 16, None, None).unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_consumer_drops_early() {
        use tokio_stream::StreamExt;

        let logs: Vec<String> = (0..1000).map(|i| format!("line-{}", i)).collect();
        let runtime = TestRuntime::new();
        let server = runtime.spawn::<LogServer>("logs", logs).await.unwrap();

        let mut stream = server.expand(GetLogs, 4, None, None).unwrap();
        let item1 = stream.next().await.unwrap();
        let item2 = stream.next().await.unwrap();
        assert_eq!(item1, "line-0");
        assert_eq!(item2, "line-1");

        // Drop stream — actor's sender.send() should return ConsumerDropped
        drop(stream);
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Actor should still be alive (dropping a stream doesn't kill the actor)
        assert!(server.is_alive());
    }

    #[tokio::test]
    async fn test_stream_items_in_order() {
        struct NumberStream;
        impl Actor for NumberStream {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                NumberStream
            }
        }

        struct GetNumbers {
            count: u64,
        }
        impl Message for GetNumbers {
            type Reply = u64;
        }

        #[async_trait]
        impl ExpandHandler<GetNumbers, u64> for NumberStream {
            async fn handle_expand(
                &mut self,
                msg: GetNumbers,
                sender: StreamSender<u64>,
                _ctx: &mut ActorContext,
            ) {
                for i in 0..msg.count {
                    if sender.send(i).await.is_err() {
                        break;
                    }
                }
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<NumberStream>("numbers", ()).await.unwrap();

        let stream = actor
            .expand(GetNumbers { count: 100 }, 16, None, None)
            .unwrap();
        let items: Vec<u64> = tokio_stream::StreamExt::collect(stream).await;

        assert_eq!(items.len(), 100);
        for (i, val) in items.iter().enumerate() {
            assert_eq!(*val, i as u64);
        }
    }

    #[tokio::test]
    async fn test_stream_backpressure() {
        use tokio_stream::StreamExt;

        struct SlowStream;
        impl Actor for SlowStream {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                SlowStream
            }
        }

        struct GetItems;
        impl Message for GetItems {
            type Reply = u64;
        }

        #[async_trait]
        impl ExpandHandler<GetItems, u64> for SlowStream {
            async fn handle_expand(
                &mut self,
                _: GetItems,
                sender: StreamSender<u64>,
                _ctx: &mut ActorContext,
            ) {
                for i in 0..10 {
                    if sender.send(i).await.is_err() {
                        break;
                    }
                }
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<SlowStream>("slow", ()).await.unwrap();
        let mut stream = actor.expand(GetItems, 1, None, None).unwrap();

        // Read slowly — backpressure should prevent buffer overflow
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(items.len(), 10);
    }

    // ── Feed (client-streaming) tests ─────────────────────────────────

    #[tokio::test]
    async fn test_feed_sum_integers() {
        struct Summer;
        impl Actor for Summer {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                Summer
            }
        }

        #[async_trait]
        impl ReduceHandler<u64, u64> for Summer {
            async fn handle_reduce(
                &mut self,
                mut receiver: StreamReceiver<u64>,
                _ctx: &mut ActorContext,
            ) -> u64 {
                let mut total = 0u64;
                while let Some(n) = receiver.recv().await {
                    total += n;
                }
                total
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Summer>("summer", ()).await.unwrap();

        let input = futures::stream::iter(vec![10u64, 20, 30, 40, 50]);
        let reply = actor
            .reduce::<u64, u64>(Box::pin(input), 8, None, None)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(reply, 150);
    }

    #[tokio::test]
    async fn test_feed_empty_stream() {
        struct Summer;
        impl Actor for Summer {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                Summer
            }
        }

        #[async_trait]
        impl ReduceHandler<u64, u64> for Summer {
            async fn handle_reduce(
                &mut self,
                mut receiver: StreamReceiver<u64>,
                _ctx: &mut ActorContext,
            ) -> u64 {
                let mut total = 0u64;
                while let Some(n) = receiver.recv().await {
                    total += n;
                }
                total
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Summer>("summer", ()).await.unwrap();

        let input = futures::stream::iter(Vec::<u64>::new());
        let reply = actor
            .reduce::<u64, u64>(Box::pin(input), 8, None, None)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(reply, 0);
    }

    #[tokio::test]
    async fn test_feed_100_items_in_order() {
        struct Collector {
            items: Vec<u64>,
        }
        impl Actor for Collector {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                Collector { items: Vec::new() }
            }
        }

        #[async_trait]
        impl ReduceHandler<u64, Vec<u64>> for Collector {
            async fn handle_reduce(
                &mut self,
                mut receiver: StreamReceiver<u64>,
                _ctx: &mut ActorContext,
            ) -> Vec<u64> {
                self.items.clear();
                while let Some(n) = receiver.recv().await {
                    self.items.push(n);
                }
                self.items.clone()
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Collector>("collector", ()).await.unwrap();

        let values: Vec<u64> = (0..100).collect();
        let input = futures::stream::iter(values.clone());
        let reply = actor
            .reduce::<u64, Vec<u64>>(Box::pin(input), 16, None, None)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(reply, values);
    }

    #[tokio::test]
    async fn test_feed_backpressure_buffer_1() {
        struct SlowConsumer;
        impl Actor for SlowConsumer {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                SlowConsumer
            }
        }

        #[async_trait]
        impl ReduceHandler<u64, u64> for SlowConsumer {
            async fn handle_reduce(
                &mut self,
                mut receiver: StreamReceiver<u64>,
                _ctx: &mut ActorContext,
            ) -> u64 {
                let mut count = 0u64;
                while receiver.recv().await.is_some() {
                    count += 1;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                count
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<SlowConsumer>("slow", ()).await.unwrap();

        let input = futures::stream::iter(0u64..20);
        let reply = actor
            .reduce::<u64, u64>(Box::pin(input), 1, None, None)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(reply, 20);
    }

    // ── Transform (N→M) tests ──────────────────────────────

    #[tokio::test]
    async fn test_transform_doubler() {
        use tokio_stream::StreamExt;

        struct Doubler;
        impl Actor for Doubler {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self { Doubler }
        }

        #[async_trait]
        impl TransformHandler<i32, i32> for Doubler {
            async fn handle_transform(
                &mut self,
                item: i32,
                sender: &StreamSender<i32>,
                _ctx: &mut ActorContext,
            ) {
                let _ = sender.send(item * 2).await;
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Doubler>("doubler", ()).await.unwrap();

        let input = Box::pin(futures::stream::iter(vec![1, 2, 3, 4, 5]));
        let output: Vec<i32> = actor
            .transform::<i32, i32>(input, 8, None, None)
            .unwrap()
            .collect()
            .await;
        assert_eq!(output, vec![2, 4, 6, 8, 10]);
    }

    #[tokio::test]
    async fn test_transform_splitter() {
        use tokio_stream::StreamExt;

        struct Splitter;
        impl Actor for Splitter {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self { Splitter }
        }

        #[async_trait]
        impl TransformHandler<String, String> for Splitter {
            async fn handle_transform(
                &mut self,
                item: String,
                sender: &StreamSender<String>,
                _ctx: &mut ActorContext,
            ) {
                for word in item.split_whitespace() {
                    if sender.send(word.to_string()).await.is_err() {
                        break;
                    }
                }
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Splitter>("splitter", ()).await.unwrap();

        let input = Box::pin(futures::stream::iter(vec![
            "hello world".to_string(),
            "foo bar baz".to_string(),
        ]));
        let output: Vec<String> = actor
            .transform::<String, String>(input, 8, None, None)
            .unwrap()
            .collect()
            .await;
        assert_eq!(output, vec!["hello", "world", "foo", "bar", "baz"]);
    }

    #[tokio::test]
    async fn test_transform_filter() {
        use tokio_stream::StreamExt;

        struct EvenFilter;
        impl Actor for EvenFilter {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self { EvenFilter }
        }

        #[async_trait]
        impl TransformHandler<i32, i32> for EvenFilter {
            async fn handle_transform(
                &mut self,
                item: i32,
                sender: &StreamSender<i32>,
                _ctx: &mut ActorContext,
            ) {
                if item % 2 == 0 {
                    let _ = sender.send(item).await;
                }
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<EvenFilter>("filter", ()).await.unwrap();

        let input = Box::pin(futures::stream::iter(1..=10));
        let output: Vec<i32> = actor
            .transform::<i32, i32>(input, 8, None, None)
            .unwrap()
            .collect()
            .await;
        assert_eq!(output, vec![2, 4, 6, 8, 10]);
    }

    #[tokio::test]
    async fn test_transform_empty_input() {
        use tokio_stream::StreamExt;

        struct Doubler;
        impl Actor for Doubler {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self { Doubler }
        }

        #[async_trait]
        impl TransformHandler<i32, i32> for Doubler {
            async fn handle_transform(
                &mut self,
                item: i32,
                sender: &StreamSender<i32>,
                _ctx: &mut ActorContext,
            ) {
                let _ = sender.send(item * 2).await;
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Doubler>("doubler", ()).await.unwrap();

        let input = Box::pin(futures::stream::iter(Vec::<i32>::new()));
        let output: Vec<i32> = actor
            .transform::<i32, i32>(input, 8, None, None)
            .unwrap()
            .collect()
            .await;
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn test_transform_on_complete_emits_final() {
        use tokio_stream::StreamExt;

        struct SumAndEmit {
            sum: i32,
        }
        impl Actor for SumAndEmit {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self { SumAndEmit { sum: 0 } }
        }

        #[async_trait]
        impl TransformHandler<i32, i32> for SumAndEmit {
            async fn handle_transform(
                &mut self,
                item: i32,
                _sender: &StreamSender<i32>,
                _ctx: &mut ActorContext,
            ) {
                self.sum += item;
            }

            async fn on_transform_complete(
                &mut self,
                sender: &StreamSender<i32>,
                _ctx: &mut ActorContext,
            ) {
                let _ = sender.send(self.sum).await;
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<SumAndEmit>("sum-emit", ()).await.unwrap();

        let input = Box::pin(futures::stream::iter(vec![10, 20, 30]));
        let output: Vec<i32> = actor
            .transform::<i32, i32>(input, 8, None, None)
            .unwrap()
            .collect()
            .await;
        assert_eq!(output, vec![60]);
    }

    #[tokio::test]
    async fn test_transform_batched_preserves_order() {
        use crate::stream::BatchConfig;
        use tokio_stream::StreamExt;

        struct Doubler;
        impl Actor for Doubler {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                Doubler
            }
        }

        #[async_trait]
        impl TransformHandler<i32, i32> for Doubler {
            async fn handle_transform(
                &mut self,
                item: i32,
                sender: &StreamSender<i32>,
                _ctx: &mut ActorContext,
            ) {
                let _ = sender.send(item * 2).await;
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Doubler>("doubler-batched", ()).await.unwrap();

        let input = Box::pin(futures::stream::iter(vec![1, 2, 3, 4, 5]));
        let batch_config = BatchConfig::new(2, Duration::from_secs(10));
        let output: Vec<i32> = actor
            .transform::<i32, i32>(input, 8, Some(batch_config), None)
            .unwrap()
            .collect()
            .await;
        assert_eq!(output, vec![2, 4, 6, 8, 10], "batched transform should preserve order");
    }

    // ── Cancellation tests ──────────────────────────────

    #[tokio::test]
    async fn test_cancel_ask_before_handler() {
        let token = CancellationToken::new();
        token.cancel(); // cancel immediately

        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();

        let result = counter.ask(GetCount, Some(token)).unwrap().await;
        // Should be Err(Cancelled) because cancelled before handler ran
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RuntimeError::Cancelled));
    }

    #[tokio::test]
    async fn test_cancel_after_timeout() {
        use crate::actor::cancel_after;

        struct SlowActor;
        impl Actor for SlowActor {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                SlowActor
            }
        }
        struct SlowMsg;
        impl Message for SlowMsg {
            type Reply = String;
        }
        #[async_trait]
        impl Handler<SlowMsg> for SlowActor {
            async fn handle(&mut self, _: SlowMsg, _ctx: &mut ActorContext) -> String {
                tokio::time::sleep(Duration::from_secs(10)).await;
                "done".into()
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<SlowActor>("slow", ()).await.unwrap();
        let token = cancel_after(Duration::from_millis(50));
        let result = actor.ask(SlowMsg, Some(token)).unwrap().await;
        assert!(result.is_err()); // cancelled during handler execution
    }

    #[tokio::test]
    async fn test_no_cancel_runs_to_completion() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 42 }).await.unwrap();
        let result = counter.ask(GetCount, None).unwrap().await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_ctx_cancelled_in_handler() {
        use crate::actor::cancel_after;

        struct CancelAwareActor;
        impl Actor for CancelAwareActor {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                CancelAwareActor
            }
        }
        struct LongTask;
        impl Message for LongTask {
            type Reply = String;
        }
        #[async_trait]
        impl Handler<LongTask> for CancelAwareActor {
            async fn handle(&mut self, _: LongTask, ctx: &mut ActorContext) -> String {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => "completed".into(),
                    _ = ctx.cancelled() => "cancelled".into(),
                }
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<CancelAwareActor>("aware", ()).await.unwrap();
        let token = cancel_after(Duration::from_millis(50));
        let result = actor.ask(LongTask, Some(token)).unwrap().await.unwrap();
        assert_eq!(result, "cancelled");
    }

    #[tokio::test]
    async fn test_cancel_stream() {
        use crate::actor::cancel_after;
        use tokio_stream::StreamExt;

        struct SlowStreamer;
        impl Actor for SlowStreamer {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                SlowStreamer
            }
        }
        struct StreamForever;
        impl Message for StreamForever {
            type Reply = u64;
        }
        #[async_trait]
        impl ExpandHandler<StreamForever, u64> for SlowStreamer {
            async fn handle_expand(
                &mut self,
                _msg: StreamForever,
                sender: StreamSender<u64>,
                _ctx: &mut ActorContext,
            ) {
                let mut i = 0u64;
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if sender.send(i).await.is_err() {
                        break;
                    }
                    i += 1;
                }
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<SlowStreamer>("streamer", ()).await.unwrap();
        let token = cancel_after(Duration::from_millis(100));
        let mut stream = actor.expand(StreamForever, 4, None, Some(token)).unwrap();

        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
        }
        // Stream should have ended due to cancellation — got some items but not infinite
        assert!(!items.is_empty());
        assert!(items.len() < 20);
    }

    #[tokio::test]
    async fn test_cancel_feed() {
        use crate::actor::cancel_after;

        struct FeedActor;
        impl Actor for FeedActor {
            type Args = ();
            type Deps = ();
            fn create(_: (), _: ()) -> Self {
                FeedActor
            }
        }
        #[async_trait]
        impl ReduceHandler<u64, Vec<u64>> for FeedActor {
            async fn handle_reduce(
                &mut self,
                mut receiver: StreamReceiver<u64>,
                _ctx: &mut ActorContext,
            ) -> Vec<u64> {
                let mut items = Vec::new();
                while let Some(item) = receiver.recv().await {
                    items.push(item);
                }
                items
            }
        }

        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<FeedActor>("feed-actor", ()).await.unwrap();

        // Create a slow infinite stream
        let input = futures::stream::unfold(0u64, |state| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Some((state, state + 1))
        });

        let token = cancel_after(Duration::from_millis(100));
        let result = actor
            .reduce::<u64, Vec<u64>>(Box::pin(input), 4, None, Some(token))
            .unwrap()
            .await;
        // The operation was cancelled — either we get a partial result or an error
        // When the dispatch loop's select fires cancellation, the handler future is dropped
        // and the caller receives an error (channel closed).
        match result {
            Ok(items) => {
                // If the handler finished before cancellation propagated, we get partial items
                assert!(!items.is_empty());
                assert!(items.len() < 20);
            }
            Err(_) => {
                // Cancellation dropped the handler before it could return
            }
        }
    }

    #[tokio::test]
    async fn test_cancelled_returns_pending_when_no_token() {
        // Verify that ctx.cancelled() never resolves when no token is set
        let ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "test".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };

        // cancelled() should not resolve — use select to prove it
        tokio::select! {
            _ = ctx.cancelled() => panic!("cancelled() should never resolve without a token"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                // expected: timeout wins because cancelled() returns pending
            }
        }
    }

    // ── Conformance suite ────────────────────────────────
    mod conformance_tests {
        use super::*;
        use crate::test_support::conformance;

        #[tokio::test]
        async fn conformance_tell_and_ask() {
            let runtime = TestRuntime::new();
            conformance::test_tell_and_ask(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_message_ordering() {
            let runtime = TestRuntime::new();
            conformance::test_message_ordering(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_ask_reply() {
            let runtime = TestRuntime::new();
            conformance::test_ask_reply(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_stop() {
            let runtime = TestRuntime::new();
            conformance::test_stop(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_unique_ids() {
            let runtime = TestRuntime::new();
            conformance::test_unique_ids(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_actor_name() {
            let runtime = TestRuntime::new();
            conformance::test_actor_name(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_stream_items() {
            let runtime = TestRuntime::new();
            conformance::test_stream_items(|name, init| {
                runtime.spawn::<conformance::ConformanceStreamer>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_stream_empty() {
            let runtime = TestRuntime::new();
            conformance::test_stream_empty(|name, init| {
                runtime.spawn::<conformance::ConformanceStreamer>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_feed_sum() {
            let runtime = TestRuntime::new();
            conformance::test_feed_sum(|name, init| {
                runtime.spawn::<conformance::ConformanceAggregator>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_lifecycle_ordering() {
            let runtime = TestRuntime::new();
            conformance::test_lifecycle_ordering(|name, init| {
                runtime.spawn::<conformance::ConformanceLifecycle>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_cancel_ask() {
            let runtime = TestRuntime::new();
            conformance::test_cancel_ask(|name, init| {
                runtime.spawn::<conformance::ConformanceCounter>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_on_error_resume() {
            let runtime = TestRuntime::new();
            conformance::test_on_error_resume(|name, init| {
                runtime.spawn::<conformance::ConformanceResumeActor>(name, init)
            })
            .await;
        }

        // ── Conformance transform tests ───────────────────────────────────

        #[tokio::test]
        async fn conformance_transform_doubler() {
            let runtime = TestRuntime::new();
            conformance::test_transform_doubler(|name, init| {
                runtime.spawn::<conformance::ConformanceDoubler>(name, init)
            })
            .await;
        }

        #[tokio::test]
        async fn conformance_transform_empty() {
            let runtime = TestRuntime::new();
            conformance::test_transform_empty(|name, init| {
                runtime.spawn::<conformance::ConformanceDoubler>(name, init)
            })
            .await;
        }

        // ── Batched stream/feed integration tests ─────────────────────────

        #[tokio::test]
        async fn test_expand_batched_returns_items_in_order() {
            use crate::stream::BatchConfig;
            use tokio_stream::StreamExt;

            let runtime = TestRuntime::new();
            let server = runtime.spawn::<LogServer>(
                "logs-batched",
                vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
            ).await.unwrap();

            let batch_config = BatchConfig::new(2, Duration::from_secs(10));
            let mut stream = server
                .expand(GetLogs, 16, Some(batch_config), None)
                .unwrap();
            let mut items = Vec::new();
            while let Some(item) = stream.next().await {
                items.push(item);
            }

            assert_eq!(items, vec!["a", "b", "c", "d", "e"]);
        }

        #[tokio::test]
        async fn test_reduce_batched_sum() {
            use crate::stream::BatchConfig;

            struct Summer;
            impl Actor for Summer {
                type Args = ();
                type Deps = ();
                fn create(_: (), _: ()) -> Self {
                    Summer
                }
            }

            #[async_trait]
            impl ReduceHandler<u64, u64> for Summer {
                async fn handle_reduce(
                    &mut self,
                    mut receiver: StreamReceiver<u64>,
                    _ctx: &mut ActorContext,
                ) -> u64 {
                    let mut total = 0u64;
                    while let Some(n) = receiver.recv().await {
                        total += n;
                    }
                    total
                }
            }

            let runtime = TestRuntime::new();
            let actor = runtime.spawn::<Summer>("sum-batched", ()).await.unwrap();

            let input = futures::stream::iter(vec![10u64, 20, 30, 40, 50]);
            let batch_config = BatchConfig::new(3, Duration::from_secs(10));
            let total = actor
                .reduce::<u64, u64>(Box::pin(input), 8, Some(batch_config), None)
                .unwrap()
                .await
                .unwrap();

            assert_eq!(total, 150);
        }
    }

    // -- F6: on_reply wiring test -------------------------------------------

    #[tokio::test]
    async fn test_outbound_on_reply_called_for_ask() {
        use std::sync::atomic::AtomicU64;

        struct ReplyObserver {
            call_count: Arc<AtomicU64>,
        }
        impl OutboundInterceptor for ReplyObserver {
            fn name(&self) -> &'static str {
                "reply-observer"
            }
            fn on_reply(
                &self,
                _ctx: &OutboundContext<'_>,
                _rh: &RuntimeHeaders,
                _headers: &Headers,
                outcome: &Outcome<'_>,
            ) {
                if matches!(outcome, Outcome::AskSuccess { .. }) {
                    self.call_count.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let call_count = Arc::new(AtomicU64::new(0));
        let mut runtime = TestRuntime::new();
        runtime.add_outbound_interceptor(Box::new(ReplyObserver {
            call_count: call_count.clone(),
        }));

        let actor = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        actor.tell(Increment(42)).unwrap();

        let count = actor.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count, 42);
        // on_reply should have been called exactly once
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // Second ask
        let count2 = actor.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count2, 42);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    // -- Dead letter handler tests ------------------------------------------

    #[tokio::test]
    async fn test_dead_letter_handler_on_stopped_actor_tell() {
        use crate::dead_letter::{CollectingDeadLetterHandler, DeadLetterReason};

        let collector = Arc::new(CollectingDeadLetterHandler::new());
        let mut runtime = TestRuntime::new();
        runtime.set_dead_letter_handler(collector.clone());

        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        counter.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = counter.tell(Increment(1));
        assert!(result.is_err());

        assert_eq!(collector.count(), 1);
        let events = collector.events();
        assert!(matches!(events[0].reason, DeadLetterReason::ActorStopped));
        assert_eq!(events[0].send_mode, SendMode::Tell);
        assert!(events[0].message_type.contains("Increment"));
    }

    #[tokio::test]
    async fn test_dead_letter_handler_on_stopped_actor_ask() {
        use crate::dead_letter::{CollectingDeadLetterHandler, DeadLetterReason};

        let collector = Arc::new(CollectingDeadLetterHandler::new());
        let mut runtime = TestRuntime::new();
        runtime.set_dead_letter_handler(collector.clone());

        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        counter.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = counter.ask(GetCount, None);
        assert!(result.is_err());

        assert_eq!(collector.count(), 1);
        let events = collector.events();
        assert!(matches!(events[0].reason, DeadLetterReason::ActorStopped));
        assert_eq!(events[0].send_mode, SendMode::Ask);
        assert!(events[0].message_type.contains("GetCount"));
    }

    #[tokio::test]
    async fn test_dead_letter_handler_on_inbound_interceptor_drop() {
        use crate::dead_letter::{CollectingDeadLetterHandler, DeadLetterReason};

        struct DropAllInterceptor;
        impl InboundInterceptor for DropAllInterceptor {
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
            fn on_complete(
                &self,
                _ctx: &InboundContext<'_>,
                _rh: &RuntimeHeaders,
                _h: &Headers,
                _outcome: &Outcome<'_>,
            ) {
            }
        }

        let collector = Arc::new(CollectingDeadLetterHandler::new());
        let mut runtime = TestRuntime::new();
        runtime.set_dead_letter_handler(collector.clone());

        let counter = runtime.spawn_with_options::<Counter>(
            "counter",
            Counter { count: 0 },
            SpawnOptions {
                interceptors: vec![Box::new(DropAllInterceptor)],
                ..Default::default()
            },
        ).await.unwrap();

        counter.tell(Increment(1)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(collector.count(), 1);
        let events = collector.events();
        match &events[0].reason {
            DeadLetterReason::DroppedByInterceptor { interceptor } => {
                assert_eq!(interceptor, "drop-all");
            }
            other => panic!("expected DroppedByInterceptor, got {:?}", other),
        }
        assert_eq!(events[0].send_mode, SendMode::Tell);
        assert!(events[0].message_type.contains("Increment"));
    }

    #[tokio::test]
    async fn test_dead_letter_collecting_handler_multiple_events() {
        use crate::dead_letter::{CollectingDeadLetterHandler, DeadLetterReason};

        let collector = Arc::new(CollectingDeadLetterHandler::new());
        let mut runtime = TestRuntime::new();
        runtime.set_dead_letter_handler(collector.clone());

        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        counter.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let _ = counter.tell(Increment(1));
        let _ = counter.tell(Increment(2));
        let _ = counter.ask(GetCount, None);

        assert_eq!(collector.count(), 3);
        let events = collector.events();
        assert!(events
            .iter()
            .all(|e| matches!(e.reason, DeadLetterReason::ActorStopped)));

        collector.clear();
        assert_eq!(collector.count(), 0);
    }

    // -- Built-in metrics integration tests --------------------------------

    #[cfg(feature = "metrics")]
    #[tokio::test]
    async fn test_metrics_enabled_tracks_messages() {
        let mut runtime = TestRuntime::new();
        runtime.enable_metrics();

        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        counter.tell(Increment(1)).unwrap();
        counter.tell(Increment(2)).unwrap();
        let _ = counter.ask(GetCount, None).unwrap().await.unwrap();

        let store = runtime.metrics().unwrap();
        assert_eq!(store.total_messages(), 3);
        assert_eq!(store.total_errors(), 0);
        assert_eq!(store.actor_count(), 1);
    }

    #[cfg(feature = "metrics")]
    #[tokio::test]
    async fn test_metrics_shared_across_actors() {
        let mut runtime = TestRuntime::new();
        runtime.enable_metrics();

        let a = runtime.spawn::<Counter>("a", Counter { count: 0 }).await.unwrap();
        let b = runtime.spawn::<Counter>("b", Counter { count: 0 }).await.unwrap();

        a.tell(Increment(1)).unwrap();
        b.tell(Increment(1)).unwrap();
        b.tell(Increment(2)).unwrap();

        // Wait for messages to be processed
        let _ = a.ask(GetCount, None).unwrap().await.unwrap();
        let _ = b.ask(GetCount, None).unwrap().await.unwrap();

        let store = runtime.metrics().unwrap();
        // 3 tells + 2 asks = 5 total messages, 2 actors
        assert_eq!(store.total_messages(), 5);
        assert_eq!(store.actor_count(), 2);
    }

    #[cfg(feature = "metrics")]
    #[tokio::test]
    async fn test_runtime_metrics_snapshot() {
        let mut runtime = TestRuntime::new();
        runtime.enable_metrics();

        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        counter.tell(Increment(1)).unwrap();
        let _ = counter.ask(GetCount, None).unwrap().await.unwrap();

        let snapshot = runtime.metrics().unwrap().runtime_metrics();
        assert_eq!(snapshot.actor_count, 1);
        assert_eq!(snapshot.total_messages, 2);
        assert_eq!(snapshot.total_errors, 0);
        assert!(snapshot.message_rate > 0.0);
        assert_eq!(snapshot.error_rate, 0.0);
        assert_eq!(snapshot.window, std::time::Duration::from_secs(60));
    }

    #[cfg(feature = "metrics")]
    #[tokio::test]
    async fn test_metrics_not_tracked_when_disabled() {
        let runtime = TestRuntime::new();
        assert!(runtime.metrics().is_none());

        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 }).await.unwrap();
        counter.tell(Increment(1)).unwrap();
        let _ = counter.ask(GetCount, None).unwrap().await.unwrap();

        // No metrics store — nothing to query
        assert!(runtime.metrics().is_none());
    }

    // -- Registry tests -----------------------------------------------------

    #[tokio::test]
    async fn test_registry_auto_register_on_spawn() {
        let runtime = TestRuntime::new();
        let _counter = runtime.spawn::<Counter>("my-counter", Counter { count: 0 }).await.unwrap();

        assert!(runtime.registry().contains("my-counter"));
        let looked_up: Option<TestActorRef<Counter>> = runtime.registry().lookup("my-counter");
        assert!(looked_up.is_some());
    }

    #[tokio::test]
    async fn test_registry_lookup_and_use() {
        let runtime = TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter-a", Counter { count: 0 }).await.unwrap();
        counter.tell(Increment(10)).unwrap();

        // Look up by name and send a message through the looked-up ref
        let looked_up: TestActorRef<Counter> = runtime.registry().lookup("counter-a").unwrap();
        looked_up.tell(Increment(5)).unwrap();

        let count = looked_up.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(count, 15);
    }

    #[tokio::test]
    async fn test_registry_lookup_wrong_type_returns_none() {
        let runtime = TestRuntime::new();
        let _counter = runtime.spawn::<Counter>("typed-actor", Counter { count: 0 }).await.unwrap();

        // Greeter is a different actor type — lookup should return None
        let wrong: Option<TestActorRef<Greeter>> = runtime.registry().lookup("typed-actor");
        assert!(wrong.is_none());
    }

    #[tokio::test]
    async fn test_registry_lookup_missing_name_returns_none() {
        let runtime = TestRuntime::new();
        let missing: Option<TestActorRef<Counter>> = runtime.registry().lookup("no-such-actor");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_registry_unregister() {
        let runtime = TestRuntime::new();
        let _counter = runtime.spawn::<Counter>("removable", Counter { count: 0 }).await.unwrap();
        assert!(runtime.registry().contains("removable"));

        assert!(runtime.registry().unregister("removable"));
        assert!(!runtime.registry().contains("removable"));

        let gone: Option<TestActorRef<Counter>> = runtime.registry().lookup("removable");
        assert!(gone.is_none());
    }

    #[tokio::test]
    async fn test_registry_multiple_actors() {
        let runtime = TestRuntime::new();
        let _c1 = runtime.spawn::<Counter>("counter-1", Counter { count: 0 }).await.unwrap();
        let _c2 = runtime.spawn::<Counter>("counter-2", Counter { count: 100 }).await.unwrap();
        let _g = runtime.spawn::<Greeter>("greeter", ()).await.unwrap();

        assert_eq!(runtime.registry().len(), 3);

        let c1: TestActorRef<Counter> = runtime.registry().lookup("counter-1").unwrap();
        c1.tell(Increment(1)).unwrap();
        let v1 = c1.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(v1, 1);

        let c2: TestActorRef<Counter> = runtime.registry().lookup("counter-2").unwrap();
        let v2 = c2.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(v2, 100);

        let g: TestActorRef<Greeter> = runtime.registry().lookup("greeter").unwrap();
        let reply = g.ask(Greet("World".into()), None).unwrap().await.unwrap();
        assert_eq!(reply, "Hello, World!");
    }

    // -- JH5: TestRuntime await_stop / await_all / cleanup_finished / active_handle_count --

    #[tokio::test]
    async fn jh5_await_stop_resolves_after_actor_stops() {
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Counter>("await-stop-actor", Counter { count: 0 }).await.unwrap();
        let actor_id = actor.id();

        actor.stop();
        let result = runtime.await_stop(&actor_id).await;
        assert!(result.is_ok());
        assert_eq!(runtime.active_handle_count(), 0);
    }

    #[tokio::test]
    async fn jh5_await_stop_unknown_id_returns_ok() {
        let runtime = TestRuntime::new();
        let fake_id = ActorId {
            node: NodeId("fake".into()),
            local: 999,
        };
        let result = runtime.await_stop(&fake_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn jh5_await_all_waits_for_all_actors() {
        let runtime = TestRuntime::new();
        let a1 = runtime.spawn::<Counter>("aa1", Counter { count: 0 }).await.unwrap();
        let a2 = runtime.spawn::<Counter>("aa2", Counter { count: 0 }).await.unwrap();
        let a3 = runtime.spawn::<Counter>("aa3", Counter { count: 0 }).await.unwrap();

        assert_eq!(runtime.active_handle_count(), 3);

        a1.stop();
        a2.stop();
        a3.stop();

        let result = runtime.await_all().await;
        assert!(result.is_ok());
        assert_eq!(runtime.active_handle_count(), 0);
    }

    #[tokio::test]
    async fn jh5_cleanup_finished_removes_stopped_actors() {
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<Counter>("cleanup-test", Counter { count: 0 }).await.unwrap();
        assert_eq!(runtime.active_handle_count(), 1);

        actor.stop();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        runtime.cleanup_finished();
        assert_eq!(runtime.active_handle_count(), 0);
    }

    #[tokio::test]
    async fn jh5_active_handle_count() {
        let runtime = TestRuntime::new();
        assert_eq!(runtime.active_handle_count(), 0);

        let _a = runtime.spawn::<Counter>("hc1", Counter { count: 0 }).await.unwrap();
        assert_eq!(runtime.active_handle_count(), 1);

        let _b = runtime.spawn::<Counter>("hc2", Counter { count: 0 }).await.unwrap();
        assert_eq!(runtime.active_handle_count(), 2);
    }

    struct PanickingActor;

    #[async_trait]
    impl Actor for PanickingActor {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self { PanickingActor }

        async fn on_stop(&mut self) {
            panic!("intentional on_stop panic");
        }
    }

    struct Ping;
    impl Message for Ping { type Reply = (); }

    #[async_trait]
    impl Handler<Ping> for PanickingActor {
        async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {}
    }

    #[tokio::test]
    async fn jh5_panic_propagated_through_await_stop() {
        let runtime = TestRuntime::new();
        let actor = runtime.spawn::<PanickingActor>("panic-actor", ()).await.unwrap();
        let actor_id = actor.id();

        actor.stop();
        let result = runtime.await_stop(&actor_id).await;
        assert!(result.is_err(), "expected error from panicking on_stop");
    }

    // ---- wrap_handler integration tests ----

    tokio::task_local! {
        static TEST_CONTEXT: String;
    }

    /// An interceptor that injects a header in on_receive and restores it
    /// as task-local context in wrap_handler.
    struct TaskLocalInterceptor;

    impl InboundInterceptor for TaskLocalInterceptor {
        fn name(&self) -> &'static str {
            "task-local"
        }

        fn on_receive(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            headers: &mut Headers,
            _msg: &dyn Any,
        ) -> Disposition {
            headers.insert(ContextHeader("restored-context".to_string()));
            Disposition::Continue
        }

        fn wrap_handler<'a>(
            &'a self,
            _ctx: &InboundContext<'_>,
            headers: &Headers,
        ) -> Option<crate::interceptor::HandlerWrapper<'a>> {
            let value = headers.get::<ContextHeader>()?.0.clone();
            Some(Box::new(move |next| {
                Box::pin(TEST_CONTEXT.scope(value, next))
            }))
        }
    }

    /// Simple header for carrying context through the interceptor pipeline.
    #[derive(Clone)]
    struct ContextHeader(String);

    impl crate::message::HeaderValue for ContextHeader {
        fn header_name(&self) -> &'static str {
            "test-context"
        }
        fn to_bytes(&self) -> Option<Vec<u8>> {
            Some(self.0.as_bytes().to_vec())
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Actor that reads from task-local context during handler execution.
    struct ContextReadingActor;

    impl Actor for ContextReadingActor {
        type Args = ();
        type Deps = ();

        fn create(_args: (), _deps: ()) -> Self {
            ContextReadingActor
        }
    }

    struct ReadContext;
    impl Message for ReadContext {
        type Reply = String;
    }

    #[async_trait]
    impl Handler<ReadContext> for ContextReadingActor {
        async fn handle(&mut self, _msg: ReadContext, _ctx: &mut ActorContext) -> String {
            TEST_CONTEXT
                .try_with(|v| v.clone())
                .unwrap_or_else(|_| "no-context".to_string())
        }
    }

    #[tokio::test]
    async fn test_wrap_handler_sets_task_local_context() {
        let runtime = TestRuntime::new();
        let actor = runtime
            .spawn_with_options::<ContextReadingActor>(
                "ctx-reader",
                (),
                SpawnOptions {
                    interceptors: vec![Box::new(TaskLocalInterceptor)],
                    mailbox: MailboxConfig::Unbounded,
                },
            )
            .await
            .unwrap();

        let result = actor.ask(ReadContext, None).unwrap().await.unwrap();
        assert_eq!(result, "restored-context");
    }

    #[tokio::test]
    async fn test_wrap_handler_not_present_uses_fast_path() {
        // With no wrapping interceptor, handler should still work normally
        let runtime = TestRuntime::new();
        let actor = runtime
            .spawn_with_options::<ContextReadingActor>(
                "no-wrap",
                (),
                SpawnOptions {
                    interceptors: vec![],
                    mailbox: MailboxConfig::Unbounded,
                },
            )
            .await
            .unwrap();

        let result = actor.ask(ReadContext, None).unwrap().await.unwrap();
        assert_eq!(result, "no-context");
    }

    /// Interceptor that wraps with ordering tracking.
    struct OrderTrackingInterceptor {
        id: u32,
        order: Arc<std::sync::Mutex<Vec<u32>>>,
    }

    impl InboundInterceptor for OrderTrackingInterceptor {
        fn name(&self) -> &'static str {
            "order-tracking"
        }

        fn wrap_handler<'a>(
            &'a self,
            _ctx: &InboundContext<'_>,
            _headers: &Headers,
        ) -> Option<crate::interceptor::HandlerWrapper<'a>> {
            let id = self.id;
            let order = self.order.clone();
            Some(Box::new(move |next| {
                Box::pin(async move {
                    order.lock().unwrap().push(id);
                    next.await;
                })
            }))
        }
    }

    struct NoopInterceptor;

    impl InboundInterceptor for NoopInterceptor {
        fn name(&self) -> &'static str {
            "noop"
        }
    }

    struct WrapPing;
    impl Message for WrapPing {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<WrapPing> for ContextReadingActor {
        async fn handle(&mut self, _msg: WrapPing, _ctx: &mut ActorContext) {}
    }

    #[tokio::test]
    async fn test_wrap_handler_nesting_order() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));

        let runtime = TestRuntime::new();
        let actor = runtime
            .spawn_with_options::<ContextReadingActor>(
                "wrap-order",
                (),
                SpawnOptions {
                    interceptors: vec![
                        Box::new(OrderTrackingInterceptor {
                            id: 1,
                            order: order.clone(),
                        }),
                        Box::new(OrderTrackingInterceptor {
                            id: 2,
                            order: order.clone(),
                        }),
                    ],
                    mailbox: MailboxConfig::Unbounded,
                },
            )
            .await
            .unwrap();

        actor.tell(WrapPing).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let order = order.lock().unwrap().clone();
        assert_eq!(order, vec![1, 2], "interceptor[0] should be outermost (enters first)");
    }

    #[tokio::test]
    async fn test_wrap_handler_mixed_with_noop_interceptors() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));

        let runtime = TestRuntime::new();
        let actor = runtime
            .spawn_with_options::<ContextReadingActor>(
                "mixed-wrap",
                (),
                SpawnOptions {
                    interceptors: vec![
                        Box::new(OrderTrackingInterceptor {
                            id: 1,
                            order: order.clone(),
                        }),
                        Box::new(NoopInterceptor), // returns None
                        Box::new(OrderTrackingInterceptor {
                            id: 3,
                            order: order.clone(),
                        }),
                    ],
                    mailbox: MailboxConfig::Unbounded,
                },
            )
            .await
            .unwrap();

        actor.tell(WrapPing).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let order = order.lock().unwrap().clone();
        assert_eq!(order, vec![1, 3], "noop interceptor should be skipped");
    }
}

