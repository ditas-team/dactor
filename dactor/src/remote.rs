//! Remote actor type definitions.
//!
//! Contains marker traits, wire-format types, and cluster discovery stubs
//! for future remote actor communication. No actual networking is implemented.

use crate::interceptor::SendMode;
use crate::node::{ActorId, NodeId};
use uuid::Uuid;

/// Marker trait for messages that can be sent to remote actors.
/// Messages must be serializable to cross the network boundary.
pub trait RemoteMessage: crate::message::Message + Send + 'static {}

/// Trait for serializing and deserializing messages for wire transport.
///
/// The runtime uses this for all remote communication: tell, ask, stream, feed,
/// remote spawn (actor Args), error payloads, and headers.
pub trait MessageSerializer: Send + Sync + 'static {
    /// Human-readable name (for logging and diagnostics).
    fn name(&self) -> &'static str;

    /// Serialize a value to bytes.
    fn serialize(&self, value: &dyn std::any::Any) -> Result<Vec<u8>, SerializationError>;

    /// Deserialize bytes back to a typed value.
    fn deserialize(
        &self,
        bytes: &[u8],
        type_name: &str,
    ) -> Result<Box<dyn std::any::Any + Send>, SerializationError>;
}

/// Error during serialization/deserialization.
#[derive(Debug, Clone)]
pub struct SerializationError {
    /// Description of the serialization failure.
    pub message: String,
}

impl SerializationError {
    /// Create a new serialization error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SerializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "serialization error: {}", self.message)
    }
}

impl std::error::Error for SerializationError {}

/// Wire-format envelope for remote messages.
/// All remote messages travel as WireEnvelopes over the network.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WireEnvelope {
    /// Target actor ID.
    pub target: ActorId,
    /// Target actor's human-readable name.
    pub target_name: String,
    /// Rust type name of the message (for deserialization dispatch).
    pub message_type: String,
    /// How the message was sent (Tell, Ask, Expand, Reduce).
    pub send_mode: SendMode,
    /// Serialized headers (string key → bytes).
    pub headers: WireHeaders,
    /// Serialized message body.
    pub body: Vec<u8>,
    /// Request ID for correlating ask replies (None for tell).
    pub request_id: Option<Uuid>,
    /// Message version for schema evolution (None = current version).
    pub version: Option<u32>,
}

/// Wire-format headers: string key → serialized bytes.
/// Used for cross-node header transport.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WireHeaders {
    /// Key-value pairs of serialized header data.
    pub entries: std::collections::HashMap<String, Vec<u8>>,
}

impl WireHeaders {
    /// Create an empty wire headers map.
    pub fn new() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    /// Insert a header entry.
    pub fn insert(&mut self, name: String, value: Vec<u8>) {
        self.entries.insert(name, value);
    }

    /// Get a header value by name.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).map(|v| v.as_slice())
    }

    /// Whether there are no headers.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Number of header entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reconstruct typed [`Headers`](crate::message::Headers) from wire format
    /// using a [`HeaderRegistry`] to look up deserializers by name.
    pub fn to_headers(&self, registry: &HeaderRegistry) -> crate::message::Headers {
        let mut headers = crate::message::Headers::new();
        for (name, bytes) in &self.entries {
            if let Some(header_value) = registry.deserialize(name, bytes) {
                headers.insert_boxed(header_value);
            }
        }
        headers
    }
}

/// A function that deserializes header bytes into a typed header value.
pub type HeaderDeserializerFn =
    Box<dyn Fn(&[u8]) -> Option<Box<dyn crate::message::HeaderValue>> + Send + Sync>;

/// Registry for deserializing wire header bytes back to typed
/// [`HeaderValue`](crate::message::HeaderValue) instances.
///
/// Populated at startup with one entry per header type that can arrive
/// from remote nodes.
pub struct HeaderRegistry {
    /// header_name → deserializer function.
    deserializers: std::collections::HashMap<String, HeaderDeserializerFn>,
}

impl HeaderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            deserializers: std::collections::HashMap::new(),
        }
    }

    /// Register a deserializer for a named header.
    pub fn register(
        &mut self,
        header_name: impl Into<String>,
        deserializer: impl Fn(&[u8]) -> Option<Box<dyn crate::message::HeaderValue>>
            + Send
            + Sync
            + 'static,
    ) {
        self.deserializers
            .insert(header_name.into(), Box::new(deserializer));
    }

    /// Deserialize header bytes using the registered deserializer.
    /// Returns `None` if no deserializer is registered or deserialization fails.
    pub fn deserialize(
        &self,
        header_name: &str,
        bytes: &[u8],
    ) -> Option<Box<dyn crate::message::HeaderValue>> {
        let deser = self.deserializers.get(header_name)?;
        deser(bytes)
    }

    /// Number of registered header deserializers.
    pub fn len(&self) -> usize {
        self.deserializers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.deserializers.is_empty()
    }
}

impl Default for HeaderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Handler for message version migration.
/// Registered per message type to handle schema evolution.
pub trait MessageVersionHandler: Send + Sync + 'static {
    /// The message type this handler manages.
    fn message_type(&self) -> &'static str;

    /// Migrate a message payload from an older version to the current version.
    /// Returns the migrated bytes, or None if migration is not possible.
    fn migrate(&self, payload: &[u8], from_version: u32, to_version: u32) -> Option<Vec<u8>>;
}

/// Snapshot of the cluster state at a point in time.
///
/// Includes topology (which nodes are known), version information for
/// each peer, and the local node's own version metadata. This is the
/// primary type for operational visibility during rolling upgrades.
///
/// # Invariants
///
/// - `nodes` includes the local node.
/// - `peer_versions` excludes the local node (local version info is in
///   `wire_version` and `app_version`).
/// - Every connected remote node in `nodes` should have a corresponding
///   entry in `peer_versions`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClusterState {
    /// The local node's identity.
    pub local_node: NodeId,
    /// All known nodes in the cluster (including local).
    pub nodes: Vec<NodeId>,
    /// Whether this node considers itself the leader (if applicable).
    pub is_leader: bool,
    /// This node's wire protocol version.
    pub wire_version: crate::version::WireVersion,
    /// This node's application version, if configured. Purely
    /// informational — does not affect compatibility.
    pub app_version: Option<String>,
    /// Version metadata for each connected remote peer. Populated from
    /// successful handshake responses. Keyed by [`NodeId`]; does **not**
    /// include the local node.
    pub peer_versions: std::collections::HashMap<NodeId, PeerVersionInfo>,
}

impl ClusterState {
    /// Create a new ClusterState with the given local node and defaults.
    ///
    /// Sets `wire_version` to [`DACTOR_WIRE_VERSION`](crate::version::DACTOR_WIRE_VERSION),
    /// `app_version` to `None`, `is_leader` to `false`, and empty
    /// `peer_versions`.
    pub fn new(local_node: NodeId, nodes: Vec<NodeId>) -> Self {
        Self {
            local_node,
            nodes,
            is_leader: false,
            wire_version: crate::version::WireVersion::parse(
                crate::version::DACTOR_WIRE_VERSION,
            )
            .expect("DACTOR_WIRE_VERSION must be valid"),
            app_version: None,
            peer_versions: std::collections::HashMap::new(),
        }
    }

    /// Number of nodes in the cluster.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Check if a specific node is in the cluster.
    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.nodes.contains(node_id)
    }

    /// Look up version information for a remote peer.
    pub fn peer_version(&self, node_id: &NodeId) -> Option<&PeerVersionInfo> {
        self.peer_versions.get(node_id)
    }
}

/// Version metadata for a connected remote peer.
///
/// Populated from a successful [`HandshakeResponse::Accepted`](crate::system_actors::HandshakeResponse)
/// during connection setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerVersionInfo {
    /// The peer's wire protocol version.
    pub wire_version: crate::version::WireVersion,
    /// The peer's application version, if configured.
    pub app_version: Option<String>,
    /// The peer's actor framework adapter (e.g. "ractor", "kameo").
    pub adapter: String,
}

/// Error returned by [`ClusterDiscovery`] implementations.
#[derive(Debug, Clone)]
pub struct DiscoveryError {
    /// Human-readable description of what went wrong.
    pub message: String,
}

impl DiscoveryError {
    /// Create a new discovery error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DiscoveryError {}

/// A peer found by [`ClusterDiscovery`].
///
/// Combines a stable identity ([`NodeId`]) with a network address.
/// Discovery backends that only know addresses (e.g., DNS-based discovery)
/// can use the address as the node ID via [`DiscoveredPeer::from_address`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiscoveredPeer {
    /// The peer's stable identity in the cluster.
    pub node_id: NodeId,
    /// The peer's network address (e.g., "10.0.0.1:9000").
    pub address: String,
}

impl DiscoveredPeer {
    /// Create a discovered peer with an explicit node ID.
    pub fn new(node_id: NodeId, address: impl Into<String>) -> Self {
        Self {
            node_id,
            address: address.into(),
        }
    }

    /// Create a discovered peer using the address string as the node ID.
    ///
    /// Convenience for discovery backends that don't provide stable
    /// identities (e.g., DNS resolution). The address is used as both
    /// the identity and the endpoint.
    ///
    /// **Important:** When using this with [`try_connect_peer`](crate::cluster::perform_handshake),
    /// the remote node must be configured with a `node_id` that matches
    /// this address string exactly. If the remote node uses a different
    /// `node_id` (e.g., `"my-node"` instead of `"10.0.0.1:9000"`), the
    /// handshake will fail with a node identity mismatch. For such cases,
    /// use [`DiscoveredPeer::new`] with an explicit `node_id` instead.
    pub fn from_address(address: impl Into<String>) -> Self {
        let addr = address.into();
        Self {
            node_id: NodeId(addr.clone()),
            address: addr,
        }
    }
}

/// Trait for cluster discovery — how nodes find each other.
#[async_trait::async_trait]
pub trait ClusterDiscovery: Send + Sync + 'static {
    /// Discover peers to connect to.
    ///
    /// Returns a list of [`DiscoveredPeer`]s. The runtime will compare this
    /// against currently connected peers and attempt to connect new ones
    /// via [`try_connect_peer`](crate::cluster::perform_handshake).
    async fn discover(&self) -> Result<Vec<DiscoveredPeer>, DiscoveryError>;
}

/// Static list of seed peers (simplest discovery mechanism).
pub struct StaticSeeds {
    /// Pre-configured peers.
    pub peers: Vec<DiscoveredPeer>,
}

impl StaticSeeds {
    /// Create static seeds from a list of addresses.
    ///
    /// Each address is used as both the node ID and the endpoint
    /// (via [`DiscoveredPeer::from_address`]).
    pub fn new(addresses: Vec<String>) -> Self {
        Self {
            peers: addresses.into_iter().map(DiscoveredPeer::from_address).collect(),
        }
    }

    /// Create static seeds from a list of [`DiscoveredPeer`]s with
    /// explicit node IDs.
    pub fn from_peers(peers: Vec<DiscoveredPeer>) -> Self {
        Self { peers }
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for StaticSeeds {
    async fn discover(&self) -> Result<Vec<DiscoveredPeer>, DiscoveryError> {
        Ok(self.peers.clone())
    }
}

// ---------------------------------------------------------------------------
// JsonSerializer (serde feature)
// ---------------------------------------------------------------------------

/// JSON-based message serializer using `serde_json`.
///
/// Human-readable format, good for debugging and interop. Slightly slower
/// and larger than binary formats but excellent for development and
/// diagnostics.
#[cfg(feature = "serde")]
pub struct JsonSerializer;

#[cfg(feature = "serde")]
impl JsonSerializer {
    /// Serialize a value to JSON bytes.
    pub fn serialize_typed<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, SerializationError> {
        serde_json::to_vec(value)
            .map_err(|e| SerializationError::new(format!("json serialize: {e}")))
    }

    /// Deserialize JSON bytes to a typed value.
    pub fn deserialize_typed<T: serde::de::DeserializeOwned>(
        bytes: &[u8],
    ) -> Result<T, SerializationError> {
        serde_json::from_slice(bytes)
            .map_err(|e| SerializationError::new(format!("json deserialize: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Wire pipeline helpers
// ---------------------------------------------------------------------------

/// Build a [`WireEnvelope`] for a remote tell (fire-and-forget).
///
/// Serializes the message body to bytes and packages it with the target,
/// headers, and metadata into a wire-ready envelope.
#[cfg(feature = "serde")]
pub fn build_tell_envelope<M: serde::Serialize>(
    target: crate::node::ActorId,
    target_name: impl Into<String>,
    msg: &M,
    headers: WireHeaders,
) -> Result<WireEnvelope, SerializationError> {
    let body = JsonSerializer::serialize_typed(msg)?;
    Ok(WireEnvelope {
        target,
        target_name: target_name.into(),
        message_type: std::any::type_name::<M>().to_string(),
        send_mode: crate::interceptor::SendMode::Tell,
        headers,
        body,
        request_id: None,
        version: None,
    })
}

/// Build a [`WireEnvelope`] for a remote ask (request-reply).
///
/// Includes a `request_id` for correlating the reply.
#[cfg(feature = "serde")]
pub fn build_ask_envelope<M: serde::Serialize>(
    target: crate::node::ActorId,
    target_name: impl Into<String>,
    msg: &M,
    headers: WireHeaders,
    request_id: uuid::Uuid,
) -> Result<WireEnvelope, SerializationError> {
    let body = JsonSerializer::serialize_typed(msg)?;
    Ok(WireEnvelope {
        target,
        target_name: target_name.into(),
        message_type: std::any::type_name::<M>().to_string(),
        send_mode: crate::interceptor::SendMode::Ask,
        headers,
        body,
        request_id: Some(request_id),
        version: None,
    })
}

/// Build a [`WireEnvelope`] with an explicit send mode and optional request ID.
///
/// Lower-level builder for stream and feed modes.
#[cfg(feature = "serde")]
pub fn build_wire_envelope<M: serde::Serialize>(
    target: crate::node::ActorId,
    target_name: impl Into<String>,
    msg: &M,
    send_mode: crate::interceptor::SendMode,
    headers: WireHeaders,
    request_id: Option<uuid::Uuid>,
    version: Option<u32>,
) -> Result<WireEnvelope, SerializationError> {
    let body = JsonSerializer::serialize_typed(msg)?;
    Ok(WireEnvelope {
        target,
        target_name: target_name.into(),
        message_type: std::any::type_name::<M>().to_string(),
        send_mode,
        headers,
        body,
        request_id,
        version,
    })
}

/// Receive-side: deserialize a [`WireEnvelope`]'s body using a
/// [`TypeRegistry`](crate::type_registry::TypeRegistry).
///
/// Returns the type-erased deserialized message. The caller can downcast
/// to the concrete type using `Any::downcast::<M>()`.
pub fn receive_envelope_body(
    envelope: &WireEnvelope,
    registry: &crate::type_registry::TypeRegistry,
) -> Result<Box<dyn std::any::Any + Send>, SerializationError> {
    registry.deserialize(&envelope.message_type, &envelope.body)
}

/// Receive-side with version checking: deserialize a [`WireEnvelope`]'s body,
/// applying version migration if a [`MessageVersionHandler`] is registered.
pub fn receive_envelope_body_versioned(
    envelope: &WireEnvelope,
    registry: &crate::type_registry::TypeRegistry,
    version_handlers: &std::collections::HashMap<String, Box<dyn MessageVersionHandler>>,
    expected_version: Option<u32>,
) -> Result<Box<dyn std::any::Any + Send>, SerializationError> {
    let body = match (envelope.version, expected_version) {
        (Some(received), Some(expected)) if received != expected => {
            // Version mismatch — try migration
            if let Some(handler) = version_handlers.get(&envelope.message_type) {
                handler
                    .migrate(&envelope.body, received, expected)
                    .ok_or_else(|| {
                        SerializationError::new(format!(
                            "{}: cannot migrate from v{received} to v{expected}",
                            envelope.message_type
                        ))
                    })?
            } else {
                // No handler registered — use body as-is (rely on serde defaults)
                envelope.body.clone()
            }
        }
        _ => envelope.body.clone(),
    };

    registry.deserialize(&envelope.message_type, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interceptor::SendMode;
    use crate::node::NodeId;

    #[test]
    fn test_wire_envelope_construction() {
        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("node-1".into()),
                local: 42,
            },
            target_name: "test".into(),
            message_type: "my_crate::Increment".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![1, 2, 3],
            request_id: None,
            version: Some(1),
        };
        assert_eq!(envelope.message_type, "my_crate::Increment");
        assert_eq!(envelope.body, vec![1, 2, 3]);
        assert_eq!(envelope.version, Some(1));
    }

    #[test]
    fn test_wire_headers() {
        let mut headers = WireHeaders::new();
        assert!(headers.is_empty());
        headers.insert("trace-id".into(), b"abc-123".to_vec());
        headers.insert("priority".into(), vec![128]);
        assert_eq!(headers.len(), 2);
        assert_eq!(headers.get("trace-id").unwrap(), b"abc-123");
        assert_eq!(headers.get("priority").unwrap(), &[128]);
        assert!(headers.get("missing").is_none());
    }

    #[test]
    fn test_serialization_error() {
        let err = SerializationError::new("invalid format");
        assert!(format!("{}", err).contains("invalid format"));
    }

    #[test]
    fn test_cluster_state() {
        let mut state = ClusterState::new(
            NodeId("node-1".into()),
            vec![
                NodeId("node-1".into()),
                NodeId("node-2".into()),
                NodeId("node-3".into()),
            ],
        );
        state.is_leader = true;
        assert_eq!(state.node_count(), 3);
        assert!(state.contains(&NodeId("node-2".into())));
        assert!(!state.contains(&NodeId("node-99".into())));
        assert!(state.is_leader);
        assert!(state.app_version.is_none());
        assert_eq!(
            state.wire_version,
            crate::version::WireVersion::parse(crate::version::DACTOR_WIRE_VERSION).unwrap()
        );
        assert!(state.peer_versions.is_empty());
    }

    #[test]
    fn test_cluster_state_with_app_version() {
        let mut state = ClusterState::new(
            NodeId("node-1".into()),
            vec![NodeId("node-1".into()), NodeId("node-2".into())],
        );
        state.app_version = Some("2.3.1".into());
        assert_eq!(state.app_version.as_deref(), Some("2.3.1"));
    }

    #[test]
    fn test_cluster_state_peer_versions() {
        let mut state = ClusterState::new(
            NodeId("node-1".into()),
            vec![
                NodeId("node-1".into()),
                NodeId("node-2".into()),
                NodeId("node-3".into()),
            ],
        );
        state.peer_versions.insert(
            NodeId("node-2".into()),
            PeerVersionInfo {
                wire_version: crate::version::WireVersion::parse("0.2.0").unwrap(),
                app_version: Some("1.0.0".into()),
                adapter: "ractor".into(),
            },
        );
        state.peer_versions.insert(
            NodeId("node-3".into()),
            PeerVersionInfo {
                wire_version: crate::version::WireVersion::parse("0.2.0").unwrap(),
                app_version: Some("1.0.1".into()),
                adapter: "ractor".into(),
            },
        );

        // Lookup works
        let p2 = state.peer_version(&NodeId("node-2".into())).unwrap();
        assert_eq!(p2.app_version.as_deref(), Some("1.0.0"));
        assert_eq!(p2.adapter, "ractor");

        let p3 = state.peer_version(&NodeId("node-3".into())).unwrap();
        assert_eq!(p3.app_version.as_deref(), Some("1.0.1"));

        // Local node not in peer_versions
        assert!(state.peer_version(&NodeId("node-1".into())).is_none());

        // Unknown node not in peer_versions
        assert!(state.peer_version(&NodeId("node-99".into())).is_none());
    }

    #[test]
    fn test_cluster_state_mixed_app_versions() {
        let mut state = ClusterState::new(
            NodeId("node-1".into()),
            vec![
                NodeId("node-1".into()),
                NodeId("node-2".into()),
                NodeId("node-3".into()),
            ],
        );
        state.app_version = Some("2.3.1".into());
        state.peer_versions.insert(
            NodeId("node-2".into()),
            PeerVersionInfo {
                wire_version: crate::version::WireVersion::parse("0.2.0").unwrap(),
                app_version: Some("2.3.0".into()),
                adapter: "ractor".into(),
            },
        );
        state.peer_versions.insert(
            NodeId("node-3".into()),
            PeerVersionInfo {
                wire_version: crate::version::WireVersion::parse("0.2.0").unwrap(),
                app_version: Some("2.3.1".into()),
                adapter: "ractor".into(),
            },
        );

        // Can compute rollout progress
        let total = state.node_count();
        let on_latest = 1 // local
            + state.peer_versions.values()
                .filter(|p| p.app_version.as_deref() == Some("2.3.1"))
                .count();
        assert_eq!(total, 3);
        assert_eq!(on_latest, 2); // node-1 (local) + node-3
    }

    #[tokio::test]
    async fn test_static_seeds() {
        let seeds = StaticSeeds::new(vec!["node1:4697".into(), "node2:4697".into()]);
        let discovered = seeds.discover().await.unwrap();
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].address, "node1:4697");
        assert_eq!(discovered[0].node_id, NodeId("node1:4697".into()));
    }

    #[tokio::test]
    async fn test_static_seeds_from_peers() {
        let seeds = StaticSeeds::from_peers(vec![
            DiscoveredPeer::new(NodeId("node-a".into()), "10.0.0.1:9000"),
            DiscoveredPeer::new(NodeId("node-b".into()), "10.0.0.2:9000"),
        ]);
        let discovered = seeds.discover().await.unwrap();
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].node_id, NodeId("node-a".into()));
        assert_eq!(discovered[0].address, "10.0.0.1:9000");
        assert_eq!(discovered[1].node_id, NodeId("node-b".into()));
    }

    #[test]
    fn test_discovered_peer_from_address() {
        let peer = DiscoveredPeer::from_address("10.0.0.1:9000");
        assert_eq!(peer.node_id, NodeId("10.0.0.1:9000".into()));
        assert_eq!(peer.address, "10.0.0.1:9000");
    }

    #[test]
    fn test_wire_envelope_with_request_id() {
        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "Ask".into(),
            send_mode: SendMode::Ask,
            headers: WireHeaders::new(),
            body: vec![],
            request_id: Some(Uuid::new_v4()),
            version: None,
        };
        assert!(envelope.request_id.is_some());
        assert_eq!(envelope.send_mode, SendMode::Ask);
    }

    #[test]
    fn test_header_registry_roundtrip() {
        use crate::message::HeaderValue;
        use std::any::Any;

        #[derive(Debug, Clone)]
        struct TraceId(String);
        impl HeaderValue for TraceId {
            fn header_name(&self) -> &'static str {
                "trace-id"
            }
            fn to_bytes(&self) -> Option<Vec<u8>> {
                Some(self.0.as_bytes().to_vec())
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        // Set up header registry
        let mut registry = HeaderRegistry::new();
        registry.register("trace-id", |bytes: &[u8]| {
            let s = String::from_utf8(bytes.to_vec()).ok()?;
            Some(Box::new(TraceId(s)) as Box<dyn HeaderValue>)
        });

        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());

        // Create typed headers and convert to wire
        let mut headers = crate::message::Headers::new();
        headers.insert(TraceId("abc-123".into()));
        let wire = headers.to_wire();

        assert_eq!(wire.len(), 1);
        assert_eq!(wire.get("trace-id").unwrap(), b"abc-123");

        // Convert wire back to typed headers via registry
        let restored = wire.to_headers(&registry);
        let trace = restored.get::<TraceId>().unwrap();
        assert_eq!(trace.0, "abc-123");
    }

    #[test]
    fn test_header_registry_missing_deserializer() {
        let registry = HeaderRegistry::new();
        assert!(registry.deserialize("unknown", &[]).is_none());
    }

    #[test]
    fn test_headers_to_wire_skips_local_only() {
        use crate::message::HeaderValue;
        use std::any::Any;

        #[derive(Debug)]
        struct LocalOnlyHeader;
        impl HeaderValue for LocalOnlyHeader {
            fn header_name(&self) -> &'static str {
                "local-only"
            }
            fn to_bytes(&self) -> Option<Vec<u8>> {
                None
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut headers = crate::message::Headers::new();
        headers.insert(LocalOnlyHeader);
        let wire = headers.to_wire();
        assert!(wire.is_empty());
    }

    #[test]
    fn test_receive_envelope_body() {
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::Amount", |bytes: &[u8]| {
            if bytes.len() != 8 {
                return Err(SerializationError::new("expected 8 bytes"));
            }
            let val = u64::from_be_bytes(bytes.try_into().unwrap());
            Ok(Box::new(val))
        });

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Amount".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: 42u64.to_be_bytes().to_vec(),
            request_id: None,
            version: None,
        };

        let any = receive_envelope_body(&envelope, &registry).unwrap();
        let val = any.downcast::<u64>().unwrap();
        assert_eq!(*val, 42);
    }

    #[test]
    fn test_receive_envelope_body_unknown_type() {
        let registry = crate::type_registry::TypeRegistry::new();
        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "unknown::Type".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![],
            request_id: None,
            version: None,
        };

        let result = receive_envelope_body(&envelope, &registry);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no deserializer"));
    }

    #[test]
    fn test_version_mismatch_with_handler() {
        let mut registry = crate::type_registry::TypeRegistry::new();
        // Register v2 deserializer (u64 big-endian)
        registry.register("test::Versioned", |bytes: &[u8]| {
            if bytes.len() != 8 {
                return Err(SerializationError::new("expected 8 bytes"));
            }
            let val = u64::from_be_bytes(bytes.try_into().unwrap());
            Ok(Box::new(val))
        });

        // Version handler that doubles the value during migration
        struct DoubleMigrator;
        impl MessageVersionHandler for DoubleMigrator {
            fn message_type(&self) -> &'static str {
                "test::Versioned"
            }
            fn migrate(&self, payload: &[u8], _from: u32, _to: u32) -> Option<Vec<u8>> {
                if payload.len() != 8 {
                    return None;
                }
                let val = u64::from_be_bytes(payload.try_into().unwrap());
                Some((val * 2).to_be_bytes().to_vec())
            }
        }

        let mut version_handlers: std::collections::HashMap<
            String,
            Box<dyn MessageVersionHandler>,
        > = std::collections::HashMap::new();
        version_handlers.insert("test::Versioned".into(), Box::new(DoubleMigrator));

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Versioned".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: 21u64.to_be_bytes().to_vec(),
            request_id: None,
            version: Some(1), // sender is v1
        };

        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            Some(2), // we expect v2
        )
        .unwrap();
        let val = any.downcast::<u64>().unwrap();
        assert_eq!(*val, 42); // 21 * 2 = 42 (migrated)
    }

    #[test]
    fn test_version_match_skips_migration() {
        // When versions match, no migration is attempted — even if a handler exists.
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::Same", |bytes: &[u8]| Ok(Box::new(bytes.to_vec())));

        struct PanicMigrator;
        impl MessageVersionHandler for PanicMigrator {
            fn message_type(&self) -> &'static str {
                "test::Same"
            }
            fn migrate(&self, _payload: &[u8], _from: u32, _to: u32) -> Option<Vec<u8>> {
                panic!("migrate should not be called when versions match");
            }
        }

        let mut version_handlers: std::collections::HashMap<
            String,
            Box<dyn MessageVersionHandler>,
        > = std::collections::HashMap::new();
        version_handlers.insert("test::Same".into(), Box::new(PanicMigrator));

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Same".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![1, 2, 3],
            request_id: None,
            version: Some(2),
        };

        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            Some(2), // same version — handler must NOT be called
        )
        .unwrap();
        let val = any.downcast::<Vec<u8>>().unwrap();
        assert_eq!(*val, vec![1, 2, 3]);
    }

    // --- T11: Version breaking change — migration rejection scenarios ---

    #[test]
    fn test_version_mismatch_no_handler_falls_through() {
        // When no MessageVersionHandler is registered, version mismatch
        // falls through to deserialize the body as-is (relies on serde defaults).
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::NoHandler", |bytes: &[u8]| Ok(Box::new(bytes.to_vec())));

        let version_handlers: std::collections::HashMap<String, Box<dyn MessageVersionHandler>> =
            std::collections::HashMap::new();

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::NoHandler".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![10, 20],
            request_id: None,
            version: Some(1), // sender v1
        };

        // Receiver expects v2 but has no handler — body passes through
        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            Some(2),
        )
        .unwrap();
        let val = any.downcast::<Vec<u8>>().unwrap();
        assert_eq!(*val, vec![10, 20]);
    }

    #[test]
    fn test_version_mismatch_handler_returns_none_rejects() {
        // When the MessageVersionHandler cannot migrate (returns None),
        // the call should fail with a clear error.
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::FailMigrate", |bytes: &[u8]| Ok(Box::new(bytes.to_vec())));

        struct RejectingMigrator;
        impl MessageVersionHandler for RejectingMigrator {
            fn message_type(&self) -> &'static str {
                "test::FailMigrate"
            }
            fn migrate(&self, _payload: &[u8], _from: u32, _to: u32) -> Option<Vec<u8>> {
                None // migration not possible
            }
        }

        let mut version_handlers: std::collections::HashMap<
            String,
            Box<dyn MessageVersionHandler>,
        > = std::collections::HashMap::new();
        version_handlers.insert("test::FailMigrate".into(), Box::new(RejectingMigrator));

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::FailMigrate".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![1, 2, 3],
            request_id: None,
            version: Some(1), // sender v1
        };

        let result = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            Some(2), // receiver expects v2
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("cannot migrate from v1 to v2"),
            "expected migration rejection, got: {}",
            err.message
        );
    }

    #[test]
    fn test_version_none_on_sender_skips_migration() {
        // When the sender doesn't set a version (None), no migration is attempted
        // regardless of the receiver's expected version — even if a handler exists.
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::OptionalVersion", |bytes: &[u8]| Ok(Box::new(bytes.to_vec())));

        // Register a panicking handler to prove it's never called
        struct PanicMigrator;
        impl MessageVersionHandler for PanicMigrator {
            fn message_type(&self) -> &'static str {
                "test::OptionalVersion"
            }
            fn migrate(&self, _payload: &[u8], _from: u32, _to: u32) -> Option<Vec<u8>> {
                panic!("migrate should not be called when sender has no version");
            }
        }

        let mut version_handlers: std::collections::HashMap<
            String,
            Box<dyn MessageVersionHandler>,
        > = std::collections::HashMap::new();
        version_handlers.insert("test::OptionalVersion".into(), Box::new(PanicMigrator));

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::OptionalVersion".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![7, 8, 9],
            request_id: None,
            version: None, // sender has no version
        };

        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            Some(2), // receiver expects v2
        )
        .unwrap();
        let val = any.downcast::<Vec<u8>>().unwrap();
        assert_eq!(*val, vec![7, 8, 9]);
    }

    #[test]
    fn test_version_none_on_both_sides_skips_migration() {
        // When neither side specifies a version, no migration is attempted.
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::NoVersion", |bytes: &[u8]| Ok(Box::new(bytes.to_vec())));

        let version_handlers: std::collections::HashMap<String, Box<dyn MessageVersionHandler>> =
            std::collections::HashMap::new();

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::NoVersion".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![4, 5, 6],
            request_id: None,
            version: None,
        };

        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            None, // receiver also has no version expectation
        )
        .unwrap();
        let val = any.downcast::<Vec<u8>>().unwrap();
        assert_eq!(*val, vec![4, 5, 6]);
    }

    #[test]
    fn test_version_none_on_receiver_skips_migration() {
        // When receiver has no version expectation (None) but sender has a
        // version, no migration is attempted — even if a handler exists.
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::ReceiverNone", |bytes: &[u8]| Ok(Box::new(bytes.to_vec())));

        struct PanicMigrator;
        impl MessageVersionHandler for PanicMigrator {
            fn message_type(&self) -> &'static str {
                "test::ReceiverNone"
            }
            fn migrate(&self, _payload: &[u8], _from: u32, _to: u32) -> Option<Vec<u8>> {
                panic!("migrate should not be called when receiver has no version expectation");
            }
        }

        let mut version_handlers: std::collections::HashMap<
            String,
            Box<dyn MessageVersionHandler>,
        > = std::collections::HashMap::new();
        version_handlers.insert("test::ReceiverNone".into(), Box::new(PanicMigrator));

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::ReceiverNone".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![11, 22, 33],
            request_id: None,
            version: Some(3), // sender has v3
        };

        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            None, // receiver has no version expectation
        )
        .unwrap();
        let val = any.downcast::<Vec<u8>>().unwrap();
        assert_eq!(*val, vec![11, 22, 33]);
    }

    #[test]
    fn test_version_backward_migration_v2_to_v1() {
        // Verify migration works in reverse direction (newer sender, older receiver).
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register("test::Backward", |bytes: &[u8]| {
            if bytes.len() != 8 {
                return Err(SerializationError::new("expected 8 bytes"));
            }
            let val = u64::from_be_bytes(bytes.try_into().unwrap());
            Ok(Box::new(val))
        });

        struct HalveMigrator;
        impl MessageVersionHandler for HalveMigrator {
            fn message_type(&self) -> &'static str {
                "test::Backward"
            }
            fn migrate(&self, payload: &[u8], from: u32, to: u32) -> Option<Vec<u8>> {
                if from > to {
                    // Downgrade: halve the value
                    let val = u64::from_be_bytes(payload.try_into().ok()?);
                    Some((val / 2).to_be_bytes().to_vec())
                } else {
                    None
                }
            }
        }

        let mut version_handlers: std::collections::HashMap<
            String,
            Box<dyn MessageVersionHandler>,
        > = std::collections::HashMap::new();
        version_handlers.insert("test::Backward".into(), Box::new(HalveMigrator));

        let envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Backward".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: 100u64.to_be_bytes().to_vec(),
            request_id: None,
            version: Some(2), // sender is v2
        };

        let any = receive_envelope_body_versioned(
            &envelope,
            &registry,
            &version_handlers,
            Some(1), // receiver expects v1
        )
        .unwrap();
        let val = any.downcast::<u64>().unwrap();
        assert_eq!(*val, 50); // 100 / 2 = 50 (downgraded)
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Increment {
            amount: u64,
        }

        #[test]
        fn json_serializer_roundtrip() {
            let msg = Increment { amount: 42 };
            let bytes = JsonSerializer::serialize_typed(&msg).unwrap();
            let deserialized: Increment = JsonSerializer::deserialize_typed(&bytes).unwrap();
            assert_eq!(deserialized, msg);
        }

        #[test]
        fn json_serializer_invalid_bytes() {
            let result = JsonSerializer::deserialize_typed::<Increment>(b"not json");
            assert!(result.is_err());
            assert!(result.unwrap_err().message.contains("json deserialize"));
        }

        #[test]
        fn build_tell_envelope_roundtrip() {
            let target = ActorId {
                node: NodeId("node-2".into()),
                local: 7,
            };
            let msg = Increment { amount: 100 };
            let envelope =
                build_tell_envelope(target.clone(), "counter", &msg, WireHeaders::new()).unwrap();

            assert_eq!(envelope.target, target);
            assert_eq!(envelope.send_mode, SendMode::Tell);
            assert!(envelope.request_id.is_none());
            assert!(envelope.message_type.contains("Increment"));

            // Deserialize body back
            let deserialized: Increment =
                JsonSerializer::deserialize_typed(&envelope.body).unwrap();
            assert_eq!(deserialized.amount, 100);
        }

        #[test]
        fn build_ask_envelope_roundtrip() {
            let target = ActorId {
                node: NodeId("node-3".into()),
                local: 42,
            };
            let msg = Increment { amount: 5 };
            let request_id = Uuid::new_v4();
            let envelope = build_ask_envelope(
                target.clone(),
                "counter",
                &msg,
                WireHeaders::new(),
                request_id,
            )
            .unwrap();

            assert_eq!(envelope.target, target);
            assert_eq!(envelope.send_mode, SendMode::Ask);
            assert_eq!(envelope.request_id, Some(request_id));
        }

        #[test]
        fn full_pipeline_send_and_receive() {
            // 1. Build envelope (sender side)
            let target = ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            };
            let msg = Increment { amount: 77 };
            let envelope =
                build_tell_envelope(target, "counter", &msg, WireHeaders::new()).unwrap();

            // 2. Register type on receiver side
            let mut registry = crate::type_registry::TypeRegistry::new();
            registry.register_type::<Increment>();

            // 3. Receive and deserialize
            let any = receive_envelope_body(&envelope, &registry).unwrap();
            let received = any.downcast::<Increment>().unwrap();
            assert_eq!(received.amount, 77);
        }

        #[test]
        fn full_pipeline_with_headers() {
            use crate::message::HeaderValue;
            use std::any::Any;

            #[derive(Debug, Clone)]
            struct Priority(u8);
            impl HeaderValue for Priority {
                fn header_name(&self) -> &'static str {
                    "priority"
                }
                fn to_bytes(&self) -> Option<Vec<u8>> {
                    Some(vec![self.0])
                }
                fn as_any(&self) -> &dyn Any {
                    self
                }
            }

            // Sender: typed headers → wire
            let mut headers = crate::message::Headers::new();
            headers.insert(Priority(5));
            let wire_headers = headers.to_wire();

            let target = ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            };
            let msg = Increment { amount: 10 };
            let envelope = build_tell_envelope(target, "counter", &msg, wire_headers).unwrap();

            // Receiver: wire headers → typed (via registry)
            let mut header_registry = HeaderRegistry::new();
            header_registry.register("priority", |bytes: &[u8]| {
                if bytes.len() != 1 {
                    return None;
                }
                Some(Box::new(Priority(bytes[0])) as Box<dyn HeaderValue>)
            });

            let restored_headers = envelope.headers.to_headers(&header_registry);
            let priority = restored_headers.get::<Priority>().unwrap();
            assert_eq!(priority.0, 5);
        }
    }
}
