use crate::errors::ClusterError;
use crate::node::NodeId;

/// Events emitted by the cluster membership system.
///
/// Subscribe via [`ClusterEvents::subscribe`] to react to topology changes
/// such as scaling, failover, or planned maintenance.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ClusterEvent {
    /// A new node has joined the cluster and is ready to receive messages.
    NodeJoined(NodeId),
    /// A node has left the cluster (gracefully or due to failure).
    NodeLeft(NodeId),
    /// A node attempted to join but was rejected during the version handshake.
    ///
    /// This event is emitted when a connecting node fails the compatibility
    /// check (different wire protocol version or adapter) or when the
    /// handshake transport call itself fails. The rejected node does **not**
    /// appear in the cluster's node list.
    NodeRejected {
        /// The node that was rejected.
        node_id: NodeId,
        /// Why the node was rejected.
        reason: NodeRejectionReason,
        /// Human-readable detail message.
        detail: String,
    },
}

/// Reason a node was rejected during cluster join.
///
/// This is the **cluster-level** rejection reason, used in
/// [`ClusterEvent::NodeRejected`]. It is distinct from
/// [`RejectionReason`](crate::system_actors::RejectionReason), which is
/// the handshake-level (wire protocol) reason. Use the [`From`]
/// implementation to convert handshake rejections into cluster events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum NodeRejectionReason {
    /// The remote node's wire protocol MAJOR version differs (Category 1).
    IncompatibleProtocol,
    /// The remote node uses a different actor framework adapter.
    IncompatibleAdapter,
    /// The transport-level handshake call failed (timeout, network error,
    /// or the remote node did not respond).
    ConnectionFailed,
}

impl std::fmt::Display for NodeRejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeRejectionReason::IncompatibleProtocol => {
                write!(f, "incompatible wire protocol")
            }
            NodeRejectionReason::IncompatibleAdapter => {
                write!(f, "incompatible adapter")
            }
            NodeRejectionReason::ConnectionFailed => {
                write!(f, "connection failed")
            }
        }
    }
}

impl From<crate::system_actors::RejectionReason> for NodeRejectionReason {
    fn from(reason: crate::system_actors::RejectionReason) -> Self {
        match reason {
            crate::system_actors::RejectionReason::IncompatibleProtocol => {
                NodeRejectionReason::IncompatibleProtocol
            }
            crate::system_actors::RejectionReason::IncompatibleAdapter => {
                NodeRejectionReason::IncompatibleAdapter
            }
        }
    }
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
    /// each [`ClusterEvent`] (`NodeJoined`, `NodeLeft`, `NodeRejected`).
    /// Returns a [`SubscriptionId`] that can be used to cancel the
    /// subscription.
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<SubscriptionId, ClusterError>;

    /// Remove a previously registered subscription. Idempotent.
    fn unsubscribe(&self, id: SubscriptionId) -> Result<(), ClusterError>;
}

// ---------------------------------------------------------------------------
// ClusterEventEmitter
// ---------------------------------------------------------------------------

/// In-process event emitter for cluster membership changes.
///
/// Manages subscriber callbacks and dispatches [`ClusterEvent`]s to all
/// active subscribers. Used by adapter runtimes to notify actors of
/// topology changes.
pub struct ClusterEventEmitter {
    next_id: u64,
    subscribers: std::collections::HashMap<SubscriptionId, Box<dyn Fn(ClusterEvent) + Send + Sync>>,
}

impl ClusterEventEmitter {
    /// Create a new emitter with no subscribers.
    pub fn new() -> Self {
        Self {
            next_id: 1,
            subscribers: std::collections::HashMap::new(),
        }
    }

    /// Subscribe to cluster events. Returns a subscription ID.
    pub fn subscribe(
        &mut self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> SubscriptionId {
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.subscribers.insert(id, on_event);
        id
    }

    /// Remove a subscription. Idempotent.
    pub fn unsubscribe(&mut self, id: SubscriptionId) {
        self.subscribers.remove(&id);
    }

    /// Emit an event to all subscribers.
    pub fn emit(&self, event: ClusterEvent) {
        for callback in self.subscribers.values() {
            callback(event.clone());
        }
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }
}

impl Default for ClusterEventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AdapterCluster trait (R4: Connection management)
// ---------------------------------------------------------------------------

/// Connection management for adapter runtimes.
///
/// Adapters implement this trait to wire cluster discovery, node connections,
/// and health monitoring into their runtime. The dactor framework calls
/// these methods during startup and when topology changes are detected.
#[async_trait::async_trait]
pub trait AdapterCluster: Send + Sync + 'static {
    /// Connect to a remote node. Called when discovery finds a new peer
    /// or when reconnecting after failure.
    async fn connect(&self, node: &NodeId) -> Result<(), ClusterError>;

    /// Disconnect from a remote node. Called on graceful shutdown or when
    /// removing a node from the cluster.
    async fn disconnect(&self, node: &NodeId) -> Result<(), ClusterError>;

    /// Reconnect to a node (disconnect + connect). Used for recovery after
    /// transient failures.
    async fn reconnect(&self, node: &NodeId) -> Result<(), ClusterError> {
        self.disconnect(node).await?;
        self.connect(node).await
    }

    /// Check if a node is currently reachable.
    async fn is_reachable(&self, node: &NodeId) -> bool;

    /// Get the list of currently connected nodes.
    async fn connected_nodes(&self) -> Vec<NodeId>;
}

// ---------------------------------------------------------------------------
// HealthChecker trait (C5: Health delegation)
// ---------------------------------------------------------------------------

/// Result of a node health check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Node is healthy and responsive.
    Healthy,
    /// Node is unhealthy or unresponsive.
    Unhealthy {
        /// Description of the health issue.
        reason: String,
    },
    /// Health check timed out.
    Timeout,
}

/// Trait for checking the health of a remote node.
///
/// Adapters delegate health checks to their underlying provider's
/// mechanism (e.g., gRPC health check, TCP ping, heartbeat).
#[async_trait::async_trait]
pub trait HealthChecker: Send + Sync + 'static {
    /// Check the health of a remote node.
    async fn check(&self, node: &NodeId) -> HealthStatus;
}

/// Called when a node becomes unreachable. Adapters implement this to
/// handle the failure (e.g., mark actors as stopped, notify watchers).
#[async_trait::async_trait]
pub trait UnreachableHandler: Send + Sync + 'static {
    /// Called when a node is determined to be unreachable.
    async fn on_node_unreachable(&self, node: &NodeId);
}

// ---------------------------------------------------------------------------
// Handshake enforcement helper
// ---------------------------------------------------------------------------

/// Outcome of a validated peer connection attempt.
#[derive(Debug, Clone)]
pub enum HandshakeOutcome {
    /// Handshake succeeded. Contains the peer's version metadata.
    Accepted(crate::remote::PeerVersionInfo),
    /// Handshake was rejected by the remote node.
    Rejected {
        reason: NodeRejectionReason,
        detail: String,
    },
    /// Transport-level failure during connect or handshake exchange.
    ConnectionFailed {
        detail: String,
    },
}

/// Perform a validated connection attempt: transport connect → handshake → cleanup on failure.
///
/// This is the core handshake enforcement logic shared by all adapters.
/// It handles the full sequence:
/// 1. `transport.connect(node, address)` — dial the network endpoint
/// 2. `transport.handshake(node, request)` — exchange version info
/// 3. On success: return `Accepted` with peer version metadata
/// 4. On reject/error: `transport.disconnect(node)` and return failure
///
/// The `address` parameter is the network endpoint to dial (from
/// [`DiscoveredPeer::address`](crate::remote::DiscoveredPeer)). If `None`,
/// the transport uses whatever endpoint is already associated with the node.
///
/// Adapter runtimes call this from their `try_connect_peer()` method,
/// then update their own state (NodeDirectory, events) based on the outcome.
pub async fn perform_handshake(
    transport: &dyn crate::transport::Transport,
    node: &NodeId,
    address: Option<&str>,
    request: crate::system_actors::HandshakeRequest,
) -> HandshakeOutcome {
    // Step 1: Establish transport connectivity
    if let Err(e) = transport.connect(node, address).await {
        return HandshakeOutcome::ConnectionFailed {
            detail: format!("transport connect failed: {}", e.message),
        };
    }

    // Step 2: Exchange handshake
    match transport.handshake(node, request).await {
        Ok(crate::system_actors::HandshakeResponse::Accepted {
            node_id: remote_node_id,
            wire_version,
            app_version,
            adapter,
        }) => {
            // Verify the peer's claimed identity matches who we connected to
            if &remote_node_id != node {
                let _ = transport.disconnect(node).await;
                return HandshakeOutcome::Rejected {
                    reason: NodeRejectionReason::ConnectionFailed,
                    detail: format!(
                        "peer identity mismatch: expected {}, got {}",
                        node, remote_node_id
                    ),
                };
            }
            HandshakeOutcome::Accepted(crate::remote::PeerVersionInfo {
                wire_version,
                app_version,
                adapter,
            })
        }
        Ok(crate::system_actors::HandshakeResponse::Rejected {
            reason,
            detail,
            ..
        }) => {
            // Disconnect on rejection
            let _ = transport.disconnect(node).await;
            HandshakeOutcome::Rejected {
                reason: reason.into(),
                detail,
            }
        }
        Err(e) => {
            // Disconnect on transport failure
            let _ = transport.disconnect(node).await;
            HandshakeOutcome::ConnectionFailed {
                detail: format!("handshake exchange failed: {}", e.message),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[test]
    fn cluster_event_emitter_subscribe_and_emit() {
        let mut emitter = ClusterEventEmitter::new();
        let count = Arc::new(AtomicU64::new(0));
        let count_clone = Arc::clone(&count);

        let _id = emitter.subscribe(Box::new(move |_event| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        assert_eq!(emitter.subscriber_count(), 1);

        emitter.emit(ClusterEvent::NodeJoined(NodeId("n1".into())));
        emitter.emit(ClusterEvent::NodeLeft(NodeId("n1".into())));

        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cluster_event_emitter_unsubscribe() {
        let mut emitter = ClusterEventEmitter::new();
        let count = Arc::new(AtomicU64::new(0));
        let count_clone = Arc::clone(&count);

        let id = emitter.subscribe(Box::new(move |_event| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        emitter.emit(ClusterEvent::NodeJoined(NodeId("n1".into())));
        assert_eq!(count.load(Ordering::SeqCst), 1);

        emitter.unsubscribe(id);
        assert_eq!(emitter.subscriber_count(), 0);

        emitter.emit(ClusterEvent::NodeJoined(NodeId("n2".into())));
        assert_eq!(count.load(Ordering::SeqCst), 1); // no change
    }

    #[test]
    fn cluster_event_emitter_multiple_subscribers() {
        let mut emitter = ClusterEventEmitter::new();
        let count1 = Arc::new(AtomicU64::new(0));
        let count2 = Arc::new(AtomicU64::new(0));
        let c1 = Arc::clone(&count1);
        let c2 = Arc::clone(&count2);

        emitter.subscribe(Box::new(move |_| {
            c1.fetch_add(1, Ordering::SeqCst);
        }));
        emitter.subscribe(Box::new(move |_| {
            c2.fetch_add(10, Ordering::SeqCst);
        }));

        emitter.emit(ClusterEvent::NodeJoined(NodeId("n1".into())));

        assert_eq!(count1.load(Ordering::SeqCst), 1);
        assert_eq!(count2.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn cluster_event_emitter_captures_event_data() {
        let mut emitter = ClusterEventEmitter::new();
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);

        emitter.subscribe(Box::new(move |event| {
            captured_clone.lock().unwrap().push(event);
        }));

        emitter.emit(ClusterEvent::NodeJoined(NodeId("alpha".into())));
        emitter.emit(ClusterEvent::NodeLeft(NodeId("beta".into())));

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], ClusterEvent::NodeJoined(NodeId("alpha".into())));
        assert_eq!(events[1], ClusterEvent::NodeLeft(NodeId("beta".into())));
    }

    #[test]
    fn health_status_variants() {
        let healthy = HealthStatus::Healthy;
        assert_eq!(healthy, HealthStatus::Healthy);

        let unhealthy = HealthStatus::Unhealthy {
            reason: "connection refused".into(),
        };
        assert!(matches!(unhealthy, HealthStatus::Unhealthy { .. }));

        let timeout = HealthStatus::Timeout;
        assert_eq!(timeout, HealthStatus::Timeout);
    }

    #[test]
    fn subscription_id_from_raw() {
        let id = SubscriptionId::from_raw(42);
        assert_eq!(id, SubscriptionId(42));
    }

    // -- NodeRejected / NodeRejectionReason tests --

    #[test]
    fn node_rejected_event_construction() {
        let event = ClusterEvent::NodeRejected {
            node_id: NodeId("bad-node".into()),
            reason: NodeRejectionReason::IncompatibleProtocol,
            detail: "wire 1.0 vs 0.2".into(),
        };
        match &event {
            ClusterEvent::NodeRejected {
                node_id,
                reason,
                detail,
            } => {
                assert_eq!(node_id, &NodeId("bad-node".into()));
                assert_eq!(*reason, NodeRejectionReason::IncompatibleProtocol);
                assert!(detail.contains("1.0"));
            }
            _ => panic!("expected NodeRejected"),
        }
    }

    #[test]
    fn node_rejected_emitted_to_subscribers() {
        let mut emitter = ClusterEventEmitter::new();
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);

        emitter.subscribe(Box::new(move |event| {
            captured_clone.lock().unwrap().push(event);
        }));

        emitter.emit(ClusterEvent::NodeRejected {
            node_id: NodeId("rejected-node".into()),
            reason: NodeRejectionReason::IncompatibleAdapter,
            detail: "kameo vs ractor".into(),
        });

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ClusterEvent::NodeRejected {
                reason: NodeRejectionReason::IncompatibleAdapter,
                ..
            }
        ));
    }

    #[test]
    fn node_rejection_reason_from_handshake_rejection() {
        use crate::system_actors::RejectionReason;

        let protocol: NodeRejectionReason = RejectionReason::IncompatibleProtocol.into();
        assert_eq!(protocol, NodeRejectionReason::IncompatibleProtocol);

        let adapter: NodeRejectionReason = RejectionReason::IncompatibleAdapter.into();
        assert_eq!(adapter, NodeRejectionReason::IncompatibleAdapter);
    }

    #[test]
    fn node_rejection_reason_display() {
        assert_eq!(
            NodeRejectionReason::IncompatibleProtocol.to_string(),
            "incompatible wire protocol"
        );
        assert_eq!(
            NodeRejectionReason::IncompatibleAdapter.to_string(),
            "incompatible adapter"
        );
        assert_eq!(
            NodeRejectionReason::ConnectionFailed.to_string(),
            "connection failed"
        );
    }

    #[test]
    fn node_rejection_reason_connection_failed() {
        let event = ClusterEvent::NodeRejected {
            node_id: NodeId("unreachable".into()),
            reason: NodeRejectionReason::ConnectionFailed,
            detail: "transport error: connection refused".into(),
        };
        assert!(matches!(
            event,
            ClusterEvent::NodeRejected {
                reason: NodeRejectionReason::ConnectionFailed,
                ..
            }
        ));
    }

    #[test]
    fn node_rejected_equality() {
        let a = ClusterEvent::NodeRejected {
            node_id: NodeId("n1".into()),
            reason: NodeRejectionReason::IncompatibleProtocol,
            detail: "test".into(),
        };
        let b = ClusterEvent::NodeRejected {
            node_id: NodeId("n1".into()),
            reason: NodeRejectionReason::IncompatibleProtocol,
            detail: "test".into(),
        };
        assert_eq!(a, b);
    }

    // -- perform_handshake tests --

    use crate::system_actors::HandshakeRequest;
    use crate::transport::{InMemoryTransport, Transport};
    use crate::version::WireVersion;

    fn test_req(node: &str, wire: &str, adapter: &str) -> HandshakeRequest {
        HandshakeRequest {
            node_id: NodeId(node.into()),
            wire_version: WireVersion::parse(wire).unwrap(),
            app_version: None,
            adapter: adapter.into(),
        }
    }

    #[tokio::test]
    async fn perform_handshake_accepted() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        t1.set_handshake_info(test_req("node-1", "0.2.0", "ractor")).await;
        t2.set_handshake_info(test_req("node-2", "0.2.0", "ractor")).await;

        let _rx1 = t1.register_node(NodeId("node-1".into())).await;
        let _rx2 = t2.register_node(NodeId("node-2".into())).await;
        t1.link(&t2).await;

        let req = test_req("node-1", "0.2.0", "ractor");
        let outcome = perform_handshake(&t1, &NodeId("node-2".into()), None, req).await;
        match outcome {
            HandshakeOutcome::Accepted(info) => {
                assert_eq!(info.adapter, "ractor");
            }
            other => panic!("expected Accepted, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn perform_handshake_rejected_protocol() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        t1.set_handshake_info(test_req("node-1", "0.2.0", "ractor")).await;
        t2.set_handshake_info(test_req("node-2", "1.0.0", "ractor")).await;

        let _rx1 = t1.register_node(NodeId("node-1".into())).await;
        let _rx2 = t2.register_node(NodeId("node-2".into())).await;
        t1.link(&t2).await;

        let req = test_req("node-1", "0.2.0", "ractor");
        let outcome = perform_handshake(&t1, &NodeId("node-2".into()), None, req).await;
        match outcome {
            HandshakeOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, NodeRejectionReason::IncompatibleProtocol);
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
        // Transport should be disconnected after rejection
        assert!(!t1.is_reachable(&NodeId("node-2".into())).await);
    }

    #[tokio::test]
    async fn perform_handshake_rejected_adapter() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        t1.set_handshake_info(test_req("node-1", "0.2.0", "ractor")).await;
        t2.set_handshake_info(test_req("node-2", "0.2.0", "kameo")).await;

        let _rx1 = t1.register_node(NodeId("node-1".into())).await;
        let _rx2 = t2.register_node(NodeId("node-2".into())).await;
        t1.link(&t2).await;

        let req = test_req("node-1", "0.2.0", "ractor");
        let outcome = perform_handshake(&t1, &NodeId("node-2".into()), None, req).await;
        match outcome {
            HandshakeOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, NodeRejectionReason::IncompatibleAdapter);
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
        // Transport should be disconnected after rejection
        assert!(!t1.is_reachable(&NodeId("node-2".into())).await);
    }

    #[tokio::test]
    async fn perform_handshake_connection_failed() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        // No route to node-2 — transport.connect() will fail

        let req = test_req("node-1", "0.2.0", "ractor");
        let outcome = perform_handshake(&t1, &NodeId("node-2".into()), None, req).await;
        match outcome {
            HandshakeOutcome::ConnectionFailed { detail } => {
                assert!(detail.contains("connect failed"));
            }
            other => panic!("expected ConnectionFailed, got {:?}", other),
        }
    }
}
