use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::errors::{ActorSendError, ErrorAction, ErrorCode, RuntimeError};
use crate::interceptor::SendMode;
use crate::mailbox::MailboxConfig;
use crate::message::{Headers, Message};
use crate::node::ActorId;
use crate::stream::{BatchConfig, BoxStream, StreamReceiver, StreamSender};

/// An error originating from within an actor's handler or lifecycle hook.
///
/// Structured error type for actor handler failures.
/// Inspired by gRPC status codes — carries a code, message, optional details,
/// and an error chain for causal tracing.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ActorError {
    /// The error category (e.g., Internal, InvalidArgument, NotFound).
    pub code: ErrorCode,
    /// Human-readable error description.
    pub message: String,
    /// Optional structured details (JSON string or domain-specific data).
    pub details: Option<String>,
    /// Causal chain — the error that caused this one.
    pub cause: Option<Box<ActorError>>,
}

impl ActorError {
    /// Create a simple error with code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            cause: None,
        }
    }

    /// Create an internal error (most common for unexpected failures).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Add details to the error.
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Chain a cause to this error.
    pub fn with_cause(mut self, cause: ActorError) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }
}

impl fmt::Display for ActorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}", self.code, self.message)?;
        if let Some(ref details) = self.details {
            write!(f, " ({})", details)?;
        }
        if let Some(ref cause) = self.cause {
            write!(f, " caused by: {}", cause)?;
        }
        Ok(())
    }
}

impl std::error::Error for ActorError {}

/// Context passed to actor lifecycle hooks and handlers.
///
/// **Note:** `ActorContext` is not `Clone` because `Headers` contains type-erased
/// header values (`Box<dyn HeaderValue>`) which are not cloneable. This is an
/// intentional v0.2 API change — handlers receive `&mut ActorContext` and should
/// extract needed values rather than cloning the context.
#[derive(Debug)]
pub struct ActorContext {
    /// Unique identity of the actor.
    pub actor_id: ActorId,
    /// Human-readable name of the actor.
    pub actor_name: String,
    /// How the current message was sent (Tell, Ask, Expand, Reduce, Transform).
    /// `None` during on_start/on_stop (no message being processed).
    pub send_mode: Option<SendMode>,
    /// Headers attached to the current message.
    /// Empty during on_start/on_stop.
    pub headers: Headers,
    /// Cancellation token for the current request. `None` if no cancellation was requested.
    pub(crate) cancellation_token: Option<CancellationToken>,
}

impl ActorContext {
    /// Create a new `ActorContext` for an actor.
    ///
    /// Used by runtime adapters to construct the context before calling
    /// lifecycle hooks and handlers.
    pub fn new(actor_id: ActorId, actor_name: String) -> Self {
        Self {
            actor_id,
            actor_name,
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        }
    }

    /// Returns a future that completes when the current request is cancelled.
    /// Use in `tokio::select!` to cooperatively check for cancellation:
    ///
    /// ```ignore
    /// tokio::select! {
    ///     result = some_long_operation() => { /* use result */ }
    ///     _ = ctx.cancelled() => { return Err(ActorError::internal("cancelled")); }
    /// }
    /// ```
    ///
    /// Returns `futures::future::pending()` if no cancellation token is set,
    /// meaning the branch will never trigger.
    pub async fn cancelled(&self) {
        match &self.cancellation_token {
            Some(token) => token.cancelled().await,
            None => futures::future::pending().await,
        }
    }

    /// Set the cancellation token for the current message.
    /// Used by runtime adapters to propagate cancellation into handlers.
    pub fn set_cancellation_token(&mut self, token: Option<CancellationToken>) {
        self.cancellation_token = token;
    }
}

/// The core actor trait. Implemented by the user's actor struct.
/// State lives in `self`. Lifecycle hooks have default no-ops.
#[async_trait]
pub trait Actor: Send + 'static {
    /// Serializable construction arguments for creating this actor.
    type Args: Send + 'static;

    /// Non-serializable local dependencies, resolved at the target node.
    type Deps: Send + 'static;

    /// Construct the actor from serializable args and local deps.
    ///
    /// Called by the runtime during spawn. Perform only synchronous
    /// initialization here; use `on_start` for async setup.
    fn create(args: Self::Args, deps: Self::Deps) -> Self
    where
        Self: Sized;

    /// Called after spawn, before any messages. Default: no-op.
    /// Use for async initialization, resource acquisition, subscriptions.
    async fn on_start(&mut self, _ctx: &mut ActorContext) {}

    /// Called when the actor is stopping. Default: no-op.
    /// Use for cleanup, resource release, flushing buffers.
    async fn on_stop(&mut self) {}

    /// Called on handler error/panic. Returns what to do next.
    fn on_error(&mut self, _error: &ActorError) -> ErrorAction {
        ErrorAction::Stop
    }
}

/// Implemented by an actor for each message type it can handle.
/// One impl per (Actor, Message) pair. Sequential execution guaranteed.
#[async_trait]
pub trait Handler<M: Message>: Actor {
    /// Handle the message and return a reply.
    async fn handle(&mut self, msg: M, ctx: &mut ActorContext) -> M::Reply;
}

/// Implemented by actors that handle expand (server-streaming) requests.
/// The handler receives the request and a `StreamSender` to push items into.
/// When this method returns, the stream closes on the caller side.
#[async_trait]
pub trait ExpandHandler<M: Message>: Actor {
    /// Handle an expand request. Push items into `sender`.
    /// When this method returns, the stream closes.
    async fn handle_expand(
        &mut self,
        msg: M,
        sender: StreamSender<M::Reply>,
        ctx: &mut ActorContext,
    );
}

/// Implemented by actors that handle reduce (client-streaming) requests.
///
/// The handler receives a [`StreamReceiver`] from which it pulls
/// caller-provided items. When the stream ends, the handler returns a
/// final reply.
///
/// Generic parameters:
/// - `Item` — the type of items the caller streams to the actor.
/// - `Reply` — the type returned after the actor consumes all items.
#[async_trait]
pub trait ReduceHandler<Item: Send + 'static, Reply: Send + 'static>: Actor {
    /// Handle a reduce request. Pull items from `receiver` and return a reply.
    async fn handle_reduce(
        &mut self,
        receiver: StreamReceiver<Item>,
        ctx: &mut ActorContext,
    ) -> Reply;
}

/// Implemented by actors that handle transform (N→M) requests.
///
/// Receives items from an input stream and produces items to an output stream.
/// For each input item, the handler may push zero or more output items via the
/// [`StreamSender`]. When the input stream ends, [`on_transform_complete`] is
/// called to allow final items to be emitted.
///
/// Generic parameters:
/// - `Item` — the type of items the caller streams to the actor.
/// - `Output` — the type of items the actor pushes to the output stream.
#[async_trait]
pub trait TransformHandler<Item: Send + 'static, Output: Send + 'static>: Actor {
    /// Handle one input item. Push zero or more output items via `sender`.
    async fn handle_transform(
        &mut self,
        item: Item,
        sender: &StreamSender<Output>,
        ctx: &mut ActorContext,
    );

    /// Called when the input stream ends. Optionally push final items.
    async fn on_transform_complete(
        &mut self,
        sender: &StreamSender<Output>,
        ctx: &mut ActorContext,
    ) {
        let _ = (sender, ctx);
    }
}

/// A future that resolves to the reply from an `ask()` call.
///
/// Wraps a `oneshot::Receiver` and implements `Future` so that callers can
/// `.await` the reply directly: `let count = actor.ask(GetCount, None)?.await?;`
pub struct AskReply<R> {
    rx: oneshot::Receiver<Result<R, RuntimeError>>,
}

impl<R> AskReply<R> {
    /// Wrap a oneshot receiver into an `AskReply` future.
    ///
    /// Typically called by the runtime, not by user code.
    pub fn new(rx: oneshot::Receiver<Result<R, RuntimeError>>) -> Self {
        Self { rx }
    }
}

impl<R> Future for AskReply<R> {
    type Output = Result<R, RuntimeError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(Ok(reply))) => Poll::Ready(Ok(reply)),
            Poll::Ready(Ok(Err(error))) => Poll::Ready(Err(error)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(RuntimeError::ActorNotFound(
                "reply channel closed — actor may have stopped, panicked, or the request was cancelled".into(),
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A reference to a running actor of type `A`.
///
/// `ActorRef<A>` is typed to the actor struct. This enables
/// sending any message type `M` where `A: Handler<M>`.
pub trait ActorRef<A: Actor>: Clone + Send + Sync + 'static {
    /// The actor's unique identity.
    fn id(&self) -> ActorId;

    /// The actor's name (as given to spawn).
    fn name(&self) -> String;

    /// Check if the actor is still alive.
    fn is_alive(&self) -> bool;

    /// Gracefully stop the actor. Closes the mailbox and triggers on_stop.
    fn stop(&self);

    /// Fire-and-forget: deliver a message to the actor.
    /// The message must have `Reply = ()` (no reply expected).
    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>;

    /// Request-reply: send a message and await the reply.
    ///
    /// Returns an [`AskReply`] future that resolves to the handler's reply.
    /// Usage: `let reply = actor.ask(msg, None)?.await?;`
    ///
    /// Pass a [`CancellationToken`] to cooperatively cancel the operation.
    fn ask<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message;

    /// Request-stream: send a request and receive a stream of responses.
    /// `buffer` controls the channel capacity (backpressure).
    ///
    /// Pass `batch_config` to enable batching (reduces per-item overhead
    /// for remote actors). `None` means unbatched per-item delivery.
    ///
    /// Pass a [`CancellationToken`] to cooperatively cancel the stream.
    fn expand<M>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: ExpandHandler<M>,
        M: Message;

    /// Client-streaming (feed): stream items to the actor and receive a reply.
    ///
    /// The caller provides items via `input`. The actor consumes them via
    /// [`StreamReceiver`] and returns a single reply when the stream ends.
    /// `buffer` controls the internal channel capacity (backpressure).
    ///
    /// Pass `batch_config` to enable batching (reduces per-item overhead
    /// for remote actors). `None` means unbatched per-item delivery.
    ///
    /// Pass a [`CancellationToken`] to cooperatively cancel the feed.
    ///
    /// Usage: `let reply = actor.reduce::<u64, u64>(input, 8, None, None)?.await?;`
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
        Reply: Send + 'static;

    /// Transform: stream items to the actor and receive a stream of outputs.
    ///
    /// The caller provides items via `input`. For each input item the actor's
    /// [`TransformHandler::handle_transform`] may push zero or more output items.
    /// When the input stream ends, [`TransformHandler::on_transform_complete`]
    /// is called to allow final items to be emitted.
    ///
    /// `buffer` controls the internal channel capacity (backpressure).
    ///
    /// Pass a [`CancellationToken`] to cooperatively cancel the transform.
    ///
    /// Usage:
    /// ```ignore
    /// let output: BoxStream<String> = actor.transform::<i32, String>(input, 8, None)?;
    /// ```
    fn transform<Item, Output>(
        &self,
        input: BoxStream<Item>,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<Output>, ActorSendError>
    where
        A: TransformHandler<Item, Output>,
        Item: Send + 'static,
        Output: Send + 'static;
}

/// Create a [`CancellationToken`] that automatically cancels after the given duration.
pub fn cancel_after(duration: Duration) -> CancellationToken {
    let token = CancellationToken::new();
    let child = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(duration).await;
        child.cancel();
    });
    token
}

/// Configuration for spawning an actor.
///
/// Default: unbounded mailbox, local spawn. Set `target_node` to spawn
/// on a remote node — the runtime serializes `Args` and sends a
/// `SpawnRequest` to the target's `SpawnManager`.
///
/// If the target node is unreachable, `spawn_with_config()` returns
/// `Err(RuntimeError::Send(...))`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SpawnConfig {
    /// Mailbox configuration (unbounded by default).
    pub mailbox: MailboxConfig,
    /// Target node for remote spawn. `None` = spawn on local node (default).
    /// When set, the runtime serializes the actor's `Args` and sends a
    /// `SpawnRequest` to the target node's `SpawnManager`.
    pub target_node: Option<crate::node::NodeId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorAction;
    use crate::message::Message;
    use crate::node::{ActorId, NodeId};

    // Test: Actor trait compiles with a simple Counter
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

    // ── Message definitions ───────────────────────────
    struct Increment(u64);
    impl Message for Increment {
        type Reply = ();
    }

    struct GetCount;
    impl Message for GetCount {
        type Reply = u64;
    }

    struct Reset;
    impl Message for Reset {
        type Reply = u64;
    }

    // ── Handler implementations ───────────────────────
    #[async_trait]
    impl Handler<Increment> for Counter {
        async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
            self.count += msg.0;
        }
    }

    #[async_trait]
    impl Handler<GetCount> for Counter {
        async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
            self.count
        }
    }

    #[async_trait]
    impl Handler<Reset> for Counter {
        async fn handle(&mut self, _msg: Reset, _ctx: &mut ActorContext) -> u64 {
            let old = self.count;
            self.count = 0;
            old
        }
    }

    #[test]
    fn test_counter_actor_compiles() {
        let counter = Counter::create(Counter { count: 0 }, ());
        assert_eq!(counter.count, 0);
    }

    #[test]
    fn test_actor_default_on_error_returns_stop() {
        let mut counter = Counter { count: 0 };
        let action = counter.on_error(&ActorError::internal("test error"));
        assert_eq!(action, ErrorAction::Stop);
    }

    // Test: Actor with custom Args and Deps
    struct WorkerArgs {
        name: String,
    }
    struct WorkerDeps {
        multiplier: u64,
    }
    struct Worker {
        name: String,
        multiplier: u64,
    }

    impl Actor for Worker {
        type Args = WorkerArgs;
        type Deps = WorkerDeps;

        fn create(args: WorkerArgs, deps: WorkerDeps) -> Self {
            Worker {
                name: args.name,
                multiplier: deps.multiplier,
            }
        }
    }

    #[test]
    fn test_worker_actor_with_deps() {
        let worker = Worker::create(
            WorkerArgs { name: "w1".into() },
            WorkerDeps { multiplier: 10 },
        );
        assert_eq!(worker.name, "w1");
        assert_eq!(worker.multiplier, 10);
    }

    // Test: ActorId
    #[test]
    fn test_actor_id_display() {
        let id = ActorId {
            node: NodeId("node-1".into()),
            local: 42,
        };
        assert_eq!(format!("{}", id), "Actor(node-1/42)");
    }

    #[test]
    fn test_actor_id_equality() {
        let id1 = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        let id2 = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        let id3 = ActorId {
            node: NodeId("n1".into()),
            local: 2,
        };
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_actor_id_clone() {
        let id = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    // Test: ErrorAction variants
    #[test]
    fn test_error_action_variants() {
        assert_eq!(ErrorAction::Resume, ErrorAction::Resume);
        assert_eq!(ErrorAction::Restart, ErrorAction::Restart);
        assert_eq!(ErrorAction::Stop, ErrorAction::Stop);
        assert_eq!(ErrorAction::Escalate, ErrorAction::Escalate);
        assert_ne!(ErrorAction::Resume, ErrorAction::Stop);
    }

    // Test: SpawnConfig default
    #[test]
    fn test_spawn_config_default() {
        let config = SpawnConfig::default();
        assert!(config.target_node.is_none());
    }

    #[test]
    fn test_spawn_config_with_target_node() {
        let config = SpawnConfig {
            target_node: Some(NodeId("node-3".into())),
            ..Default::default()
        };
        assert_eq!(config.target_node.unwrap().0, "node-3");
    }

    // Test: ActorContext
    #[test]
    fn test_actor_context_fields() {
        let ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "test-actor".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };
        assert_eq!(ctx.actor_name, "test-actor");
        assert_eq!(ctx.actor_id.local, 1);
    }

    // Test: on_start / on_stop defaults are no-ops
    #[tokio::test]
    async fn test_lifecycle_defaults_are_noop() {
        let mut counter = Counter { count: 42 };
        let mut ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };
        counter.on_start(&mut ctx).await;
        counter.on_stop().await;
        assert_eq!(counter.count, 42);
    }

    // ── Handler tests ─────────────────────────────────

    #[tokio::test]
    async fn test_handler_increment() {
        let mut counter = Counter { count: 0 };
        let mut ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };
        counter.handle(Increment(5), &mut ctx).await;
        assert_eq!(counter.count, 5);
        counter.handle(Increment(3), &mut ctx).await;
        assert_eq!(counter.count, 8);
    }

    #[tokio::test]
    async fn test_handler_get_count() {
        let mut counter = Counter { count: 42 };
        let mut ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };
        let count = counter.handle(GetCount, &mut ctx).await;
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn test_handler_reset() {
        let mut counter = Counter { count: 100 };
        let mut ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };
        let old = counter.handle(Reset, &mut ctx).await;
        assert_eq!(old, 100);
        assert_eq!(counter.count, 0);
    }

    #[tokio::test]
    async fn test_multiple_handlers_on_same_actor() {
        let mut counter = Counter { count: 0 };
        let mut ctx = ActorContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
            cancellation_token: None,
        };

        counter.handle(Increment(10), &mut ctx).await;
        counter.handle(Increment(20), &mut ctx).await;

        let count = counter.handle(GetCount, &mut ctx).await;
        assert_eq!(count, 30);

        let old = counter.handle(Reset, &mut ctx).await;
        assert_eq!(old, 30);
        assert_eq!(counter.count, 0);
    }

    #[test]
    fn test_handler_requires_actor_bound() {
        fn assert_handler<A: Handler<M>, M: Message>() {}
        assert_handler::<Counter, Increment>();
        assert_handler::<Counter, GetCount>();
        assert_handler::<Counter, Reset>();
    }

    #[test]
    fn test_actor_error_construction() {
        let err = ActorError::new(ErrorCode::InvalidArgument, "bad input");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(err.message, "bad input");
        assert!(err.details.is_none());
        assert!(err.cause.is_none());
    }

    #[test]
    fn test_actor_error_with_details() {
        let err = ActorError::new(ErrorCode::NotFound, "user not found").with_details("user_id=42");
        assert_eq!(err.details.as_deref(), Some("user_id=42"));
    }

    #[test]
    fn test_actor_error_chain() {
        let root = ActorError::new(ErrorCode::Unavailable, "db connection failed");
        let err = ActorError::new(ErrorCode::Internal, "query failed").with_cause(root);
        assert!(err.cause.is_some());
        assert_eq!(err.cause.as_ref().unwrap().code, ErrorCode::Unavailable);
    }

    #[test]
    fn test_actor_error_display() {
        let err = ActorError::new(ErrorCode::Internal, "something broke")
            .with_details("stack: foo.rs:42");
        let display = format!("{}", err);
        assert!(display.contains("Internal"));
        assert!(display.contains("something broke"));
        assert!(display.contains("stack: foo.rs:42"));
    }

    #[test]
    fn test_actor_error_display_with_chain() {
        let root = ActorError::new(ErrorCode::Unavailable, "db down");
        let err = ActorError::new(ErrorCode::Internal, "query failed").with_cause(root);
        let display = format!("{}", err);
        assert!(display.contains("caused by"));
        assert!(display.contains("db down"));
    }

    #[test]
    fn test_error_code_variants() {
        let codes = vec![
            ErrorCode::Internal,
            ErrorCode::InvalidArgument,
            ErrorCode::NotFound,
            ErrorCode::Unavailable,
            ErrorCode::Timeout,
            ErrorCode::PermissionDenied,
            ErrorCode::FailedPrecondition,
            ErrorCode::ResourceExhausted,
            ErrorCode::Unimplemented,
            ErrorCode::Unknown,
            ErrorCode::Cancelled,
        ];
        assert_eq!(codes.len(), 11);
        // All distinct
        for (i, a) in codes.iter().enumerate() {
            for (j, b) in codes.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn test_actor_error_internal_helper() {
        let err = ActorError::internal("oops");
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(err.message, "oops");
    }

    #[test]
    fn test_not_supported_error() {
        use crate::errors::NotSupportedError;
        let err = NotSupportedError {
            capability: "BoundedMailbox".into(),
            message: "ractor does not support bounded mailboxes".into(),
        };
        assert!(format!("{}", err).contains("BoundedMailbox"));
    }

    #[test]
    fn test_runtime_error_not_supported() {
        use crate::errors::NotSupportedError;
        let err = RuntimeError::NotSupported(NotSupportedError {
            capability: "PriorityMailbox".into(),
            message: "not available".into(),
        });
        assert!(format!("{}", err).contains("PriorityMailbox"));
    }
}
