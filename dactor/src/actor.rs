use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

use crate::cluster::ClusterEvents;
use crate::errors::{ActorSendError, ErrorAction, GroupError};
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
/// Will be expanded in later PRs with headers, send_mode, etc.
#[derive(Debug, Clone)]
pub struct ActorContext {
    pub actor_id: ActorId,
    pub actor_name: String,
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
    use crate::node::{NodeId, ActorId};
    use crate::errors::ErrorAction;

    // Test: Actor trait compiles with a simple Counter
    struct Counter { count: u64 }

    impl Actor for Counter {
        type Args = Self;
        type Deps = ();

        fn create(args: Self, _deps: ()) -> Self { args }
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
        };
        counter.on_start(&mut ctx).await;
        counter.on_stop().await;
        assert_eq!(counter.count, 42);
    }
}
