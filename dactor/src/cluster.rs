use crate::errors::ClusterError;
use crate::node::NodeId;

/// Events emitted by the cluster membership system.
///
/// Subscribe via [`ClusterEvents::subscribe`] to react to topology changes
/// such as scaling, failover, or planned maintenance.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ClusterEvent {
    /// A new node has joined the cluster and is ready to receive messages.
    NodeJoined(NodeId),
    /// A node has left the cluster (gracefully or due to failure).
    NodeLeft(NodeId),
}

/// Opaque handle returned by [`ClusterEvents::subscribe`], used to cancel
/// a subscription via [`ClusterEvents::unsubscribe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubscriptionId(pub(crate) u64);

impl SubscriptionId {
    /// Create a new `SubscriptionId` from a raw integer.
    ///
    /// Intended for use by adapter crates implementing `ClusterEvents`.
    pub fn from_raw(id: u64) -> Self {
        Self(id)
    }
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
