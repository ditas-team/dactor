use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::cluster::ClusterEvents;
use crate::errors::{ActorSendError, ErrorAction, GroupError, RuntimeError};
use crate::interceptor::SendMode;
use crate::message::{Headers, Message};
use crate::node::ActorId;
use crate::timer::TimerHandle;

/// A handle to a running actor that can receive messages of type `M`.
///
/// `ActorRef` is the primary communication mechanism between actors and between
/// external code and actors. Implementations must be cheaply cloneable and safe
/// to share across threads.
///
/// Only fire-and-forget delivery is part of this trait. Request-reply patterns
/// are framework-specific and should be handled at the adapter layer.
pub trait ActorRef<M: Send + 'static>: Clone + Send + Sync + 'static {
    /// Fire-and-forget: deliver `msg` to the actor's mailbox.
    fn send(&self, msg: M) -> Result<(), ActorSendError>;
}

/// The top-level actor runtime abstraction. One instance per node.
///
/// Provides actor spawning, timer scheduling, processing group management,
/// and access to the cluster event subsystem. Processing group methods are
/// part of this trait (rather than a separate trait) so that all group
/// operations use the same `Self::Ref<M>` type family without requiring
/// cross-trait GAT equality constraints.
pub trait ActorRuntime: Send + Sync + 'static {
    /// The concrete actor reference type.
    type Ref<M: Send + 'static>: ActorRef<M>;

    /// The cluster events implementation.
    type Events: ClusterEvents;

    /// The timer handle type.
    type Timer: TimerHandle;

    /// Spawn a new actor with the given name and message handler.
    ///
    /// The handler receives messages one at a time (mailbox serialization).
    fn spawn<M, H>(
        &self,
        name: &str,
        handler: H,
    ) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static;

    /// Schedule a recurring message to `target` at the given interval.
    fn send_interval<M: Clone + Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        interval: Duration,
        msg: M,
    ) -> Self::Timer;

    /// Schedule a one-shot message to `target` after the given delay.
    fn send_after<M: Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        delay: Duration,
        msg: M,
    ) -> Self::Timer;

    /// Add an actor to a named processing group.
    fn join_group<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError>;

    /// Remove an actor from a named processing group.
    fn leave_group<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError>;

    /// Broadcast a message to all members of a named processing group.
    fn broadcast_group<M: Clone + Send + 'static>(
        &self,
        group_name: &str,
        msg: M,
    ) -> Result<(), GroupError>;

    /// Get all members of a named processing group.
    fn get_group_members<M: Send + 'static>(
        &self,
        group_name: &str,
    ) -> Result<Vec<Self::Ref<M>>, GroupError>;

    /// Access the cluster event subsystem.
    fn cluster_events(&self) -> &Self::Events;
}

// ---------------------------------------------------------------------------
// v0.2 Actor API — coexists with v0.1 ActorRef / ActorRuntime above
// ---------------------------------------------------------------------------

/// Stub error type for actor errors — will be expanded in a later PR.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ActorError {
    pub message: String,
}

impl ActorError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for ActorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ActorError: {}", self.message)
    }
}

impl std::error::Error for ActorError {}

/// Context passed to actor lifecycle hooks and handlers.
#[derive(Debug)]
pub struct ActorContext {
    pub actor_id: ActorId,
    pub actor_name: String,
    /// How the current message was sent (Tell, Ask, Stream, Feed).
    /// `None` during on_start/on_stop (no message being processed).
    pub send_mode: Option<SendMode>,
    /// Headers attached to the current message.
    /// Empty during on_start/on_stop.
    pub headers: Headers,
}

/// The core actor trait. Implemented by the user's actor struct.
/// State lives in `self`. Lifecycle hooks have default no-ops.
///
/// This is the v0.2 Actor API — distinct from the v0.1 `ActorRef<M>` / `ActorRuntime`
/// traits which remain for backward compatibility.
#[async_trait]
pub trait Actor: Send + 'static {
    /// Serializable construction arguments.
    type Args: Send + 'static;

    /// Local dependencies — non-serializable, resolved at target node.
    type Deps: Send + 'static;

    /// Construct the actor from args and deps.
    fn create(args: Self::Args, deps: Self::Deps) -> Self where Self: Sized;

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

/// A future that resolves to the reply from an `ask()` call.
///
/// Wraps a `oneshot::Receiver` and implements `Future` so that callers can
/// `.await` the reply directly: `let count = actor.ask(GetCount)?.await?;`
pub struct AskReply<R> {
    rx: oneshot::Receiver<Result<R, RuntimeError>>,
}

impl<R> AskReply<R> {
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

/// A reference to a running actor of type `A` (v0.2 API).
///
/// Unlike v0.1 `ActorRef<M>` which is typed to the message,
/// `TypedActorRef<A>` is typed to the actor struct. This enables
/// sending any message type `M` where `A: Handler<M>`.
pub trait TypedActorRef<A: Actor>: Clone + Send + Sync + 'static {
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
    /// Usage: `let reply = actor.ask(msg)?.await?;`
    fn ask<M>(&self, msg: M) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message;
}

/// Configuration for spawning an actor. All fields have defaults
/// matching v0.1 behavior (unbounded mailbox, no interceptors, local spawn).
#[derive(Debug, Clone, Default)]
pub struct SpawnConfig {
    // Will be expanded in later PRs with:
    // - mailbox: MailboxConfig
    // - inbound_interceptors: Vec<...>
    // - comparer: Option<...>
    // - target_node: Option<NodeId>
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::node::{NodeId, ActorId};
    use crate::errors::ErrorAction;

    // Test: Actor trait compiles with a simple Counter
    struct Counter { count: u64 }

    impl Actor for Counter {
        type Args = Self;
        type Deps = ();

        fn create(args: Self, _deps: ()) -> Self { args }
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
        let action = counter.on_error(&ActorError::new("test error"));
        assert_eq!(action, ErrorAction::Stop);
    }

    // Test: Actor with custom Args and Deps
    struct WorkerArgs { name: String }
    struct WorkerDeps { multiplier: u64 }
    struct Worker { name: String, multiplier: u64 }

    impl Actor for Worker {
        type Args = WorkerArgs;
        type Deps = WorkerDeps;

        fn create(args: WorkerArgs, deps: WorkerDeps) -> Self {
            Worker { name: args.name, multiplier: deps.multiplier }
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
        let id1 = ActorId { node: NodeId("n1".into()), local: 1 };
        let id2 = ActorId { node: NodeId("n1".into()), local: 1 };
        let id3 = ActorId { node: NodeId("n1".into()), local: 2 };
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_actor_id_clone() {
        let id = ActorId { node: NodeId("n1".into()), local: 1 };
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
        let _config = SpawnConfig::default();
    }

    // Test: ActorContext
    #[test]
    fn test_actor_context_fields() {
        let ctx = ActorContext {
            actor_id: ActorId { node: NodeId("n1".into()), local: 1 },
            actor_name: "test-actor".into(),
            send_mode: None,
            headers: Headers::new(),
        };
        assert_eq!(ctx.actor_name, "test-actor");
        assert_eq!(ctx.actor_id.local, 1);
    }

    // Test: on_start / on_stop defaults are no-ops
    #[tokio::test]
    async fn test_lifecycle_defaults_are_noop() {
        let mut counter = Counter { count: 42 };
        let mut ctx = ActorContext {
            actor_id: ActorId { node: NodeId("n1".into()), local: 1 },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
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
            actor_id: ActorId { node: NodeId("n1".into()), local: 1 },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
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
            actor_id: ActorId { node: NodeId("n1".into()), local: 1 },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
        };
        let count = counter.handle(GetCount, &mut ctx).await;
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn test_handler_reset() {
        let mut counter = Counter { count: 100 };
        let mut ctx = ActorContext {
            actor_id: ActorId { node: NodeId("n1".into()), local: 1 },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
        };
        let old = counter.handle(Reset, &mut ctx).await;
        assert_eq!(old, 100);
        assert_eq!(counter.count, 0);
    }

    #[tokio::test]
    async fn test_multiple_handlers_on_same_actor() {
        let mut counter = Counter { count: 0 };
        let mut ctx = ActorContext {
            actor_id: ActorId { node: NodeId("n1".into()), local: 1 },
            actor_name: "counter".into(),
            send_mode: None,
            headers: Headers::new(),
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
}
