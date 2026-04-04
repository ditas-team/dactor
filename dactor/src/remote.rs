//! Remote actor type definitions.
//!
//! Contains marker traits, wire-format types, and cluster discovery stubs
//! for future remote actor communication. No actual networking is implemented.

use crate::node::{NodeId, ActorId};
use crate::interceptor::SendMode;
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
    fn deserialize(&self, bytes: &[u8], type_name: &str) -> Result<Box<dyn std::any::Any + Send>, SerializationError>;
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
        Self { message: message.into() }
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
    /// Rust type name of the message (for deserialization dispatch).
    pub message_type: String,
    /// How the message was sent (Tell, Ask, Stream, Feed).
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
    pub fn new() -> Self { Self { entries: std::collections::HashMap::new() } }

    /// Insert a header entry.
    pub fn insert(&mut self, name: String, value: Vec<u8>) {
        self.entries.insert(name, value);
    }

    /// Get a header value by name.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).map(|v| v.as_slice())
    }

    /// Whether there are no headers.
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
    /// Number of header entries.
    pub fn len(&self) -> usize { self.entries.len() }
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
#[derive(Debug, Clone)]
pub struct ClusterState {
    /// The local node's identity.
    pub local_node: NodeId,
    /// All known nodes in the cluster (including local).
    pub nodes: Vec<NodeId>,
    /// Whether this node considers itself the leader (if applicable).
    pub is_leader: bool,
}

impl ClusterState {
    /// Number of nodes in the cluster.
    pub fn node_count(&self) -> usize { self.nodes.len() }

    /// Check if a specific node is in the cluster.
    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.nodes.contains(node_id)
    }
}

/// Trait for cluster discovery — how nodes find each other.
pub trait ClusterDiscovery: Send + Sync + 'static {
    /// Discover seed nodes to connect to.
    fn discover(&self) -> Vec<String>;
}

/// Static list of seed nodes (simplest discovery mechanism).
pub struct StaticSeeds {
    /// List of seed node addresses.
    pub seeds: Vec<String>,
}

impl StaticSeeds {
    /// Create a new static seeds discovery with the given addresses.
    pub fn new(seeds: Vec<String>) -> Self { Self { seeds } }
}

impl ClusterDiscovery for StaticSeeds {
    fn discover(&self) -> Vec<String> { self.seeds.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;
    use crate::interceptor::SendMode;

    #[test]
    fn test_wire_envelope_construction() {
        let envelope = WireEnvelope {
            target: ActorId { node: NodeId("node-1".into()), local: 42 },
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
        let state = ClusterState {
            local_node: NodeId("node-1".into()),
            nodes: vec![
                NodeId("node-1".into()),
                NodeId("node-2".into()),
                NodeId("node-3".into()),
            ],
            is_leader: true,
        };
        assert_eq!(state.node_count(), 3);
        assert!(state.contains(&NodeId("node-2".into())));
        assert!(!state.contains(&NodeId("node-99".into())));
        assert!(state.is_leader);
    }

    #[test]
    fn test_static_seeds() {
        let seeds = StaticSeeds::new(vec![
            "node1:4697".into(),
            "node2:4697".into(),
        ]);
        let discovered = seeds.discover();
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0], "node1:4697");
    }

    #[test]
    fn test_wire_envelope_with_request_id() {
        let envelope = WireEnvelope {
            target: ActorId { node: NodeId("n".into()), local: 1 },
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
}
