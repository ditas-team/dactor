//! Abstract transport for remote actor communication.
//!
//! The [`Transport`] trait defines how nodes send and receive [`WireEnvelope`]s
//! over the network. It is adapter-agnostic — implementations can be backed by
//! gRPC, TCP, QUIC, or any other protocol.
//!
//! An [`InMemoryTransport`] is provided for testing without real networking.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex};
use uuid::Uuid;

use crate::node::NodeId;
use crate::remote::WireEnvelope;
use crate::system_actors::{HandshakeRequest, HandshakeResponse};

// ---------------------------------------------------------------------------
// TransportError
// ---------------------------------------------------------------------------

/// Error from transport operations.
#[derive(Debug, Clone)]
pub struct TransportError {
    /// Description of the transport failure.
    pub message: String,
}

impl TransportError {
    /// Create a new transport error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transport error: {}", self.message)
    }
}

impl std::error::Error for TransportError {}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Abstract transport for sending [`WireEnvelope`]s between nodes.
///
/// Implementations bridge the dactor framework to a network protocol
/// (gRPC, TCP, QUIC, etc.). Each adapter provides its own `Transport`
/// based on the underlying provider's networking.
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Send a wire envelope to a remote node (fire-and-forget).
    async fn send(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<(), TransportError>;

    /// Send a wire envelope and wait for a reply envelope (for ask / stream).
    async fn send_request(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<WireEnvelope, TransportError>;

    /// Check if a node is reachable.
    async fn is_reachable(&self, node: &NodeId) -> bool;

    /// Establish a connection to a remote node.
    ///
    /// The `address` parameter provides the network endpoint to dial
    /// (e.g., "10.0.0.1:9000"). If `None`, the transport uses whatever
    /// endpoint is already associated with the `node` (e.g., a previously
    /// registered route). After a successful connect, the transport must
    /// retain the `NodeId → connection` association so that subsequent
    /// `send()` and `handshake()` calls can reach the node by `NodeId`.
    async fn connect(
        &self,
        node: &NodeId,
        address: Option<&str>,
    ) -> Result<(), TransportError>;

    /// Disconnect from a remote node.
    async fn disconnect(&self, node: &NodeId) -> Result<(), TransportError>;

    /// Perform a version handshake with a remote node.
    ///
    /// Called by the runtime after [`connect`](Self::connect) to verify wire
    /// protocol compatibility before any application messages are exchanged.
    /// The implementation should exchange [`HandshakeRequest`] /
    /// [`HandshakeResponse`] with the remote node and return the response.
    ///
    /// The default implementation returns an error indicating that the
    /// transport does not support handshakes. Adapters should override this
    /// once they implement the handshake protocol.
    async fn handshake(
        &self,
        _node: &NodeId,
        _request: HandshakeRequest,
    ) -> Result<HandshakeResponse, TransportError> {
        Err(TransportError::new("transport does not support handshake"))
    }
}

// ---------------------------------------------------------------------------
// InMemoryTransport
// ---------------------------------------------------------------------------

/// In-memory transport for testing. Routes envelopes between nodes within the
/// same process via channels.
pub struct InMemoryTransport {
    /// Map of NodeId → channel sender for delivering envelopes.
    routes: Arc<Mutex<HashMap<NodeId, mpsc::Sender<WireEnvelope>>>>,
    /// Pending request-reply pairs for `send_request`.
    pending: Arc<Mutex<HashMap<Uuid, oneshot::Sender<WireEnvelope>>>>,
    /// The identity of the local node.
    local_node: NodeId,
    /// Set of nodes we are "connected" to.
    connected: Arc<Mutex<std::collections::HashSet<NodeId>>>,
    /// Handshake info for each node, shared between linked transports.
    /// Populated via [`set_handshake_info`](Self::set_handshake_info).
    handshake_info: Arc<Mutex<HashMap<NodeId, HandshakeRequest>>>,
}

impl InMemoryTransport {
    /// Create a new in-memory transport for the given local node.
    pub fn new(local_node: NodeId) -> Self {
        Self {
            routes: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            local_node,
            connected: Arc::new(Mutex::new(std::collections::HashSet::new())),
            handshake_info: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a node and return a receiver for its incoming envelopes.
    pub async fn register_node(&self, node: NodeId) -> mpsc::Receiver<WireEnvelope> {
        let (tx, rx) = mpsc::channel(256);
        self.routes.lock().await.insert(node, tx);
        rx
    }

    /// Link two transports bidirectionally so each can send to the other's
    /// registered nodes. Both transports share route tables after linking.
    ///
    /// Note: The `pending` request-reply map is NOT shared — `complete_request`
    /// must be called on the same transport that called `send_request`.
    pub async fn link(&self, other: &InMemoryTransport) {
        // Collect routes from both transports without holding both locks
        // simultaneously (avoids deadlock if link() is called concurrently
        // in opposite direction).
        let self_entries: Vec<_> = {
            let routes = self.routes.lock().await;
            routes.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        let other_entries: Vec<_> = {
            let routes = other.routes.lock().await;
            routes.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };

        // Merge: self gets other's routes, other gets self's routes.
        {
            let mut routes = self.routes.lock().await;
            for (node, sender) in other_entries {
                routes.insert(node, sender);
            }
        }
        {
            let mut routes = other.routes.lock().await;
            for (node, sender) in self_entries {
                routes.insert(node, sender);
            }
        }

        // Mark each other as connected.
        self.connected.lock().await.insert(other.local_node.clone());
        other.connected.lock().await.insert(self.local_node.clone());

        // Share handshake info between linked transports.
        let self_info: Vec<_> = {
            let info = self.handshake_info.lock().await;
            info.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        let other_info: Vec<_> = {
            let info = other.handshake_info.lock().await;
            info.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        {
            let mut info = self.handshake_info.lock().await;
            for (node, req) in other_info {
                info.insert(node, req);
            }
        }
        {
            let mut info = other.handshake_info.lock().await;
            for (node, req) in self_info {
                info.insert(node, req);
            }
        }
    }

    /// Register this node's handshake information for version negotiation.
    ///
    /// Must be called before [`handshake`](Transport::handshake) can validate
    /// against a peer. Call this before [`link`](Self::link) — info is copied
    /// (not shared) during link, so post-link updates are not visible to
    /// already-linked transports.
    pub async fn set_handshake_info(&self, request: HandshakeRequest) {
        let node = request.node_id.clone();
        self.handshake_info.lock().await.insert(node, request);
    }

    /// Submit a reply for a pending `send_request` call identified by its
    /// request_id. This is used by test harnesses to complete request/reply
    /// flows.
    pub async fn complete_request(
        &self,
        request_id: Uuid,
        reply: WireEnvelope,
    ) -> Result<(), TransportError> {
        let sender = self
            .pending
            .lock()
            .await
            .remove(&request_id)
            .ok_or_else(|| TransportError::new(format!("no pending request for {request_id}")))?;

        sender
            .send(reply)
            .map_err(|_| TransportError::new("reply receiver dropped"))
    }
}

#[async_trait]
impl Transport for InMemoryTransport {
    async fn send(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<(), TransportError> {
        let routes = self.routes.lock().await;
        let sender = routes
            .get(target_node)
            .ok_or_else(|| TransportError::new(format!("no route to {target_node}")))?;

        sender
            .send(envelope)
            .await
            .map_err(|_| TransportError::new(format!("channel closed for {target_node}")))
    }

    async fn send_request(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<WireEnvelope, TransportError> {
        let request_id = envelope
            .request_id
            .ok_or_else(|| TransportError::new("send_request requires a request_id"))?;

        let (tx, rx) = oneshot::channel();

        // Register the pending reply.
        self.pending.lock().await.insert(request_id, tx);

        // Deliver the envelope to the target. Clean up pending on failure.
        if let Err(e) = self.send(target_node, envelope).await {
            self.pending.lock().await.remove(&request_id);
            return Err(e);
        }

        // Wait for the reply.
        rx.await
            .map_err(|_| TransportError::new("reply sender dropped"))
    }

    async fn is_reachable(&self, node: &NodeId) -> bool {
        self.connected.lock().await.contains(node)
    }

    async fn connect(&self, node: &NodeId, _address: Option<&str>) -> Result<(), TransportError> {
        // In-memory transport ignores address and connects by NodeId route.
        let routes = self.routes.lock().await;
        if routes.contains_key(node) {
            self.connected.lock().await.insert(node.clone());
            Ok(())
        } else {
            Err(TransportError::new(format!("no route to {node}")))
        }
    }

    async fn disconnect(&self, node: &NodeId) -> Result<(), TransportError> {
        self.connected.lock().await.remove(node);
        Ok(())
    }

    async fn handshake(
        &self,
        node: &NodeId,
        request: HandshakeRequest,
    ) -> Result<HandshakeResponse, TransportError> {
        let info = self.handshake_info.lock().await;
        let remote_info = info.get(node).ok_or_else(|| {
            TransportError::new(format!("no handshake info registered for {node}"))
        })?;
        Ok(crate::system_actors::validate_handshake(remote_info, &request))
    }
}

// ---------------------------------------------------------------------------
// TransportRegistry
// ---------------------------------------------------------------------------

/// Maps [`NodeId`]s to [`Transport`] instances for multi-node routing.
///
/// When a message needs to be sent to a remote node the registry is consulted
/// to find the appropriate transport for that node.
pub struct TransportRegistry {
    transports: Mutex<HashMap<NodeId, Arc<dyn Transport>>>,
}

impl TransportRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            transports: Mutex::new(HashMap::new()),
        }
    }

    /// Register a transport for a given node.
    pub async fn register(&self, node: NodeId, transport: Arc<dyn Transport>) {
        self.transports.lock().await.insert(node, transport);
    }

    /// Remove a transport mapping.
    pub async fn unregister(&self, node: &NodeId) {
        self.transports.lock().await.remove(node);
    }

    /// Look up the transport for a node.
    pub async fn get(&self, node: &NodeId) -> Option<Arc<dyn Transport>> {
        self.transports.lock().await.get(node).cloned()
    }

    /// Send an envelope to a node via its registered transport.
    pub async fn send(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<(), TransportError> {
        let transport = self
            .get(target_node)
            .await
            .ok_or_else(|| TransportError::new(format!("no transport for {target_node}")))?;
        transport.send(target_node, envelope).await
    }
}

impl Default for TransportRegistry {
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
    use crate::interceptor::SendMode;
    use crate::node::ActorId;
    use crate::remote::WireHeaders;

    /// Helper to create a simple WireEnvelope for testing.
    fn test_envelope(target_node: &str, body: &[u8]) -> WireEnvelope {
        WireEnvelope {
            target: ActorId {
                node: NodeId(target_node.into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Msg".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: body.to_vec(),
            request_id: None,
            version: None,
        }
    }

    fn test_envelope_with_request_id(target_node: &str, body: &[u8], id: Uuid) -> WireEnvelope {
        WireEnvelope {
            target: ActorId {
                node: NodeId(target_node.into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Ask".into(),
            send_mode: SendMode::Ask,
            headers: WireHeaders::new(),
            body: body.to_vec(),
            request_id: Some(id),
            version: None,
        }
    }

    #[tokio::test]
    async fn send_receive_roundtrip() {
        let transport = InMemoryTransport::new(NodeId("node-a".into()));
        let mut rx = transport.register_node(NodeId("node-b".into())).await;
        transport.connect(&NodeId("node-b".into()), None).await.unwrap();

        let envelope = test_envelope("node-b", b"hello");
        transport
            .send(&NodeId("node-b".into()), envelope)
            .await
            .unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.body, b"hello");
        assert_eq!(received.message_type, "test::Msg");
    }

    #[tokio::test]
    async fn send_request_with_reply() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("node-a".into())));
        let mut rx = transport.register_node(NodeId("node-b".into())).await;
        transport.connect(&NodeId("node-b".into()), None).await.unwrap();

        let request_id = Uuid::new_v4();
        let envelope = test_envelope_with_request_id("node-b", b"question", request_id);

        let transport_clone = Arc::clone(&transport);
        let handle = tokio::spawn(async move {
            transport_clone
                .send_request(&NodeId("node-b".into()), envelope)
                .await
        });

        // Simulate the remote side receiving and replying.
        let received = rx.recv().await.unwrap();
        assert_eq!(received.body, b"question");

        let reply = test_envelope_with_request_id("node-a", b"answer", request_id);
        transport.complete_request(request_id, reply).await.unwrap();

        let response = handle.await.unwrap().unwrap();
        assert_eq!(response.body, b"answer");
    }

    #[tokio::test]
    async fn is_reachable_false_for_unknown_true_after_connect() {
        let transport = InMemoryTransport::new(NodeId("node-a".into()));
        let _rx = transport.register_node(NodeId("node-b".into())).await;

        assert!(!transport.is_reachable(&NodeId("node-b".into())).await);
        assert!(!transport.is_reachable(&NodeId("node-c".into())).await);

        transport.connect(&NodeId("node-b".into()), None).await.unwrap();
        assert!(transport.is_reachable(&NodeId("node-b".into())).await);

        transport
            .disconnect(&NodeId("node-b".into()))
            .await
            .unwrap();
        assert!(!transport.is_reachable(&NodeId("node-b".into())).await);
    }

    #[tokio::test]
    async fn linked_transports_communicate() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        let mut rx1 = t1.register_node(NodeId("node-1".into())).await;
        let mut rx2 = t2.register_node(NodeId("node-2".into())).await;

        t1.link(&t2).await;

        // t1 can send to node-2 (registered in t2, now shared via link).
        let envelope = test_envelope("node-2", b"from-t1");
        t1.send(&NodeId("node-2".into()), envelope).await.unwrap();
        let received = rx2.recv().await.unwrap();
        assert_eq!(received.body, b"from-t1");

        // t2 can send to node-1 (registered in t1, now shared via link).
        let envelope = test_envelope("node-1", b"from-t2");
        t2.send(&NodeId("node-1".into()), envelope).await.unwrap();
        let received = rx1.recv().await.unwrap();
        assert_eq!(received.body, b"from-t2");
    }

    #[tokio::test]
    async fn connect_fails_without_route() {
        let transport = InMemoryTransport::new(NodeId("node-a".into()));
        let result = transport.connect(&NodeId("node-unknown".into()), None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no route"));
    }

    #[tokio::test]
    async fn transport_registry_send() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("node-a".into())));
        let mut rx = transport.register_node(NodeId("node-b".into())).await;

        let registry = TransportRegistry::new();
        registry
            .register(NodeId("node-b".into()), transport.clone())
            .await;

        let envelope = test_envelope("node-b", b"via-registry");
        registry
            .send(&NodeId("node-b".into()), envelope)
            .await
            .unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.body, b"via-registry");
    }

    #[tokio::test]
    async fn transport_registry_missing_node() {
        let registry = TransportRegistry::new();
        let envelope = test_envelope("node-x", b"lost");
        let result = registry.send(&NodeId("node-x".into()), envelope).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no transport"));
    }

    #[tokio::test]
    async fn transport_error_display() {
        let err = TransportError::new("connection refused");
        assert_eq!(format!("{err}"), "transport error: connection refused");
    }

    // -- Handshake tests --

    use crate::version::WireVersion;

    fn test_handshake_req(node: &str, wire: &str, adapter: &str) -> HandshakeRequest {
        HandshakeRequest {
            node_id: NodeId(node.into()),
            wire_version: WireVersion::parse(wire).unwrap(),
            app_version: None,
            adapter: adapter.into(),
        }
    }

    #[tokio::test]
    async fn handshake_compatible_accepted() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        t1.set_handshake_info(test_handshake_req("node-1", "0.2.0", "ractor"))
            .await;
        t2.set_handshake_info(test_handshake_req("node-2", "0.2.0", "ractor"))
            .await;

        let _rx1 = t1.register_node(NodeId("node-1".into())).await;
        let _rx2 = t2.register_node(NodeId("node-2".into())).await;
        t1.link(&t2).await;

        let req = test_handshake_req("node-1", "0.2.0", "ractor");
        let resp = t1.handshake(&NodeId("node-2".into()), req).await.unwrap();
        match resp {
            HandshakeResponse::Accepted { node_id, .. } => {
                // Response should come from the remote node (node-2), not the caller
                assert_eq!(node_id, NodeId("node-2".into()));
            }
            _ => panic!("expected Accepted"),
        }
    }

    #[tokio::test]
    async fn handshake_incompatible_protocol_rejected() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        t1.set_handshake_info(test_handshake_req("node-1", "0.2.0", "ractor"))
            .await;
        t2.set_handshake_info(test_handshake_req("node-2", "1.0.0", "ractor"))
            .await;

        let _rx1 = t1.register_node(NodeId("node-1".into())).await;
        let _rx2 = t2.register_node(NodeId("node-2".into())).await;
        t1.link(&t2).await;

        let req = test_handshake_req("node-1", "0.2.0", "ractor");
        let resp = t1.handshake(&NodeId("node-2".into()), req).await.unwrap();
        assert!(matches!(
            resp,
            HandshakeResponse::Rejected {
                reason: crate::system_actors::RejectionReason::IncompatibleProtocol,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn handshake_incompatible_adapter_rejected() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));
        let t2 = InMemoryTransport::new(NodeId("node-2".into()));

        t1.set_handshake_info(test_handshake_req("node-1", "0.2.0", "ractor"))
            .await;
        t2.set_handshake_info(test_handshake_req("node-2", "0.2.0", "kameo"))
            .await;

        let _rx1 = t1.register_node(NodeId("node-1".into())).await;
        let _rx2 = t2.register_node(NodeId("node-2".into())).await;
        t1.link(&t2).await;

        let req = test_handshake_req("node-1", "0.2.0", "ractor");
        let resp = t1.handshake(&NodeId("node-2".into()), req).await.unwrap();
        assert!(matches!(
            resp,
            HandshakeResponse::Rejected {
                reason: crate::system_actors::RejectionReason::IncompatibleAdapter,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn handshake_no_info_returns_error() {
        let t1 = InMemoryTransport::new(NodeId("node-1".into()));

        let req = test_handshake_req("node-1", "0.2.0", "ractor");
        let result = t1.handshake(&NodeId("node-unknown".into()), req).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no handshake info"));
    }

    #[tokio::test]
    async fn handshake_default_trait_returns_error() {
        // Verify the default Transport::handshake returns an error
        struct MinimalTransport;

        #[async_trait]
        impl Transport for MinimalTransport {
            async fn send(
                &self,
                _: &NodeId,
                _: WireEnvelope,
            ) -> Result<(), TransportError> {
                Ok(())
            }
            async fn send_request(
                &self,
                _: &NodeId,
                _: WireEnvelope,
            ) -> Result<WireEnvelope, TransportError> {
                Err(TransportError::new("not supported"))
            }
            async fn is_reachable(&self, _: &NodeId) -> bool {
                false
            }
            async fn connect(&self, _: &NodeId, _: Option<&str>) -> Result<(), TransportError> {
                Ok(())
            }
            async fn disconnect(&self, _: &NodeId) -> Result<(), TransportError> {
                Ok(())
            }
        }

        let t = MinimalTransport;
        let req = test_handshake_req("node-1", "0.2.0", "test");
        let result = t.handshake(&NodeId("node-2".into()), req).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .message
            .contains("does not support handshake"));
    }
}
