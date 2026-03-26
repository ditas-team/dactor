use std::fmt;
use std::time::Duration;

use crate::types::node::NodeId;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Error returned by `ActorRef::send()`.
#[derive(Debug, Clone)]
pub struct ActorSendError(pub String);

impl fmt::Display for ActorSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "actor send failed: {}", self.0)
    }
}

impl std::error::Error for ActorSendError {}

/// Error returned by processing group operations.
#[derive(Debug, Clone)]
pub struct GroupError(pub String);

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "group error: {}", self.0)
    }
}

impl std::error::Error for GroupError {}

/// Error returned by `ClusterEvents` operations.
#[derive(Debug, Clone)]
pub struct ClusterError(pub String);

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cluster error: {}", self.0)
    }
}

impl std::error::Error for ClusterError {}

// ---------------------------------------------------------------------------
// Cluster events
// ---------------------------------------------------------------------------

/// Events emitted by the cluster membership system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterEvent {
    NodeJoined(NodeId),
    NodeLeft(NodeId),
}

/// Opaque handle returned by [`ClusterEvents::subscribe`], used to cancel
/// a subscription via [`ClusterEvents::unsubscribe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub(crate) u64);

impl SubscriptionId {
    /// Create a new `SubscriptionId` from a raw integer.
    ///
    /// Intended for use by adapter crates implementing `ClusterEvents`.
    pub fn from_raw(id: u64) -> Self {
        Self(id)
    }
}

// ---------------------------------------------------------------------------
// Abstract traits
// ---------------------------------------------------------------------------

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

/// Subscription to cluster membership events.
pub trait ClusterEvents: Send + Sync + 'static {
    /// Subscribe to cluster membership changes. The callback is invoked for
    /// each `NodeJoined` / `NodeLeft` event. Returns a [`SubscriptionId`]
    /// that can be used to cancel the subscription.
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<SubscriptionId, ClusterError>;

    /// Remove a previously registered subscription. Idempotent.
    fn unsubscribe(&self, id: SubscriptionId) -> Result<(), ClusterError>;
}

/// A handle to a scheduled timer that can be cancelled.
pub trait TimerHandle: Send + 'static {
    /// Cancel the timer. Idempotent — calling cancel on an already-cancelled
    /// or fired timer is a no-op.
    fn cancel(self);
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
