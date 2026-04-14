//! System actors for remote operations.
//!
//! When a node needs to perform a remote operation (spawn an actor, watch a
//! remote actor, cancel a remote operation), it sends a message to a
//! **system actor** on the target node. System actors are automatically
//! started by the runtime and handle these requests.
//!
//! ## System Actor Types
//!
//! - [`SpawnManager`] — handles remote actor spawn requests
//! - [`WatchManager`] — handles remote watch/unwatch subscriptions
//! - [`CancelManager`] — handles remote cancellation requests
//! - [`NodeDirectory`] — maps NodeId → connection metadata
//!
//! ## System Message Type Constants
//!
//! Well-known [`WireEnvelope::message_type`](crate::remote::WireEnvelope::message_type)
//! values used by the transport router to dispatch incoming envelopes to the
//! correct system actor mailbox. See [`system_router`](crate::system_router).

use std::collections::{HashMap, HashSet};

use crate::node::{ActorId, NodeId};
use crate::remote::SerializationError;

// ---------------------------------------------------------------------------
// System message type constants (for WireEnvelope.message_type matching)
// ---------------------------------------------------------------------------
//
// ⚠️  WIRE PROTOCOL — DO NOT CHANGE THESE VALUES ⚠️
//
// These strings are **wire protocol identifiers** sent between nodes.
// They are NOT Rust type paths, even though they happen to resemble them.
// Changing any value will break compatibility with remote nodes running
// older code.  If you rename or refactor the corresponding Rust struct,
// the constant value here MUST stay the same.
//
// A `wire_protocol_constants_are_stable` unit test enforces this.
// ---------------------------------------------------------------------------

/// Wire protocol identifier for [`SpawnRequest`] messages.
///
/// **Do not change** — this is a frozen wire protocol value, not a Rust path.
pub const SYSTEM_MSG_TYPE_SPAWN: &str = "dactor::system_actors::SpawnRequest";

/// Wire protocol identifier for [`WatchRequest`] messages.
///
/// **Do not change** — this is a frozen wire protocol value, not a Rust path.
pub const SYSTEM_MSG_TYPE_WATCH: &str = "dactor::system_actors::WatchRequest";

/// Wire protocol identifier for [`UnwatchRequest`] messages.
///
/// **Do not change** — this is a frozen wire protocol value, not a Rust path.
pub const SYSTEM_MSG_TYPE_UNWATCH: &str = "dactor::system_actors::UnwatchRequest";

/// Wire protocol identifier for [`CancelRequest`] messages.
///
/// **Do not change** — this is a frozen wire protocol value, not a Rust path.
pub const SYSTEM_MSG_TYPE_CANCEL: &str = "dactor::system_actors::CancelRequest";

/// Wire protocol identifier for peer connect messages routed to [`NodeDirectory`].
///
/// **Do not change** — this is a frozen wire protocol value, not a Rust path.
pub const SYSTEM_MSG_TYPE_CONNECT_PEER: &str = "dactor::system_actors::ConnectPeer";

/// Wire protocol identifier for peer disconnect messages routed to [`NodeDirectory`].
///
/// **Do not change** — this is a frozen wire protocol value, not a Rust path.
pub const SYSTEM_MSG_TYPE_DISCONNECT_PEER: &str = "dactor::system_actors::DisconnectPeer";

/// Returns `true` if `message_type` is a well-known system message type.
pub fn is_system_message_type(message_type: &str) -> bool {
    matches!(
        message_type,
        SYSTEM_MSG_TYPE_SPAWN
            | SYSTEM_MSG_TYPE_WATCH
            | SYSTEM_MSG_TYPE_UNWATCH
            | SYSTEM_MSG_TYPE_CANCEL
            | SYSTEM_MSG_TYPE_CONNECT_PEER
            | SYSTEM_MSG_TYPE_DISCONNECT_PEER
    )
}

// ---------------------------------------------------------------------------
// SpawnManager
// ---------------------------------------------------------------------------

/// Message requesting a remote actor spawn.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpawnRequest {
    /// Fully-qualified Rust type name of the actor (e.g. "myapp::Counter").
    pub type_name: String,
    /// Serialized actor `Args` (construction arguments).
    pub args_bytes: Vec<u8>,
    /// Name for the spawned actor.
    pub name: String,
    /// Request ID for correlating the response.
    pub request_id: String,
}

/// Response to a spawn request.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpawnResponse {
    /// Actor spawned successfully. Contains the assigned ActorId.
    Success {
        /// Request ID from the original SpawnRequest.
        request_id: String,
        /// ActorId assigned to the newly spawned actor.
        actor_id: ActorId,
    },
    /// Spawn failed.
    Failure {
        /// Request ID from the original SpawnRequest.
        request_id: String,
        /// Error description.
        error: String,
    },
}

/// Manages remote actor spawn requests on a node.
///
/// When a remote node wants to spawn an actor on this node, it sends a
/// [`SpawnRequest`] to the SpawnManager. The SpawnManager looks up the
/// actor type in the [`TypeRegistry`](crate::type_registry::TypeRegistry),
/// deserializes the Args, creates the actor, and returns a [`SpawnResponse`].
pub struct SpawnManager {
    /// Type registry for looking up actor factories.
    type_registry: crate::type_registry::TypeRegistry,
    /// Actors spawned via remote requests.
    spawned: Vec<ActorId>,
}

impl SpawnManager {
    /// Create a new SpawnManager with the given type registry.
    pub fn new(type_registry: crate::type_registry::TypeRegistry) -> Self {
        Self {
            type_registry,
            spawned: Vec::new(),
        }
    }

    /// Process a spawn request.
    ///
    /// Looks up the actor type in the registry, deserializes Args from bytes,
    /// and returns the constructed actor as `Box<dyn Any + Send>`. The caller
    /// (runtime) is responsible for actually spawning the actor and assigning
    /// an ActorId.
    pub fn create_actor(
        &self,
        request: &SpawnRequest,
    ) -> Result<Box<dyn std::any::Any + Send>, SerializationError> {
        self.type_registry
            .create_actor(&request.type_name, &request.args_bytes)
    }

    /// Record that an actor was spawned via remote request.
    pub fn record_spawn(&mut self, id: ActorId) {
        self.spawned.push(id);
    }

    /// List all actors spawned via remote requests.
    pub fn spawned_actors(&self) -> &[ActorId] {
        &self.spawned
    }

    /// Access the type registry.
    pub fn type_registry(&self) -> &crate::type_registry::TypeRegistry {
        &self.type_registry
    }

    /// Access the type registry mutably (for registering new factories).
    pub fn type_registry_mut(&mut self) -> &mut crate::type_registry::TypeRegistry {
        &mut self.type_registry
    }
}

// ---------------------------------------------------------------------------
// WatchManager
// ---------------------------------------------------------------------------

/// Request to watch a remote actor for termination.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WatchRequest {
    /// The actor to watch.
    pub target: ActorId,
    /// The watcher (on the requesting node) to notify on termination.
    pub watcher: ActorId,
}

/// Request to stop watching a remote actor.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UnwatchRequest {
    /// The actor being watched.
    pub target: ActorId,
    /// The watcher to remove.
    pub watcher: ActorId,
}

/// Notification that a watched actor has terminated.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WatchNotification {
    /// The actor that terminated.
    pub terminated: ActorId,
    /// The watcher that should be notified.
    pub watcher: ActorId,
}

/// Manages remote watch/unwatch subscriptions.
///
/// Tracks which remote actors are watching which local actors. When a
/// local actor terminates, the WatchManager sends a [`WatchNotification`]
/// to the watcher's node.
pub struct WatchManager {
    /// target ActorId → set of watcher ActorIds (watchers from remote nodes).
    watchers: HashMap<ActorId, HashSet<ActorId>>,
}

impl WatchManager {
    /// Create a new empty WatchManager.
    pub fn new() -> Self {
        Self {
            watchers: HashMap::new(),
        }
    }

    /// Register a watch: `watcher` wants to know when `target` terminates.
    pub fn watch(&mut self, target: ActorId, watcher: ActorId) {
        self.watchers.entry(target).or_default().insert(watcher);
    }

    /// Remove a watch subscription.
    pub fn unwatch(&mut self, target: &ActorId, watcher: &ActorId) {
        if let Some(set) = self.watchers.get_mut(target) {
            set.remove(watcher);
            if set.is_empty() {
                self.watchers.remove(target);
            }
        }
    }

    /// Called when a local actor terminates. Returns notifications for all
    /// remote watchers of this actor.
    pub fn on_terminated(&mut self, terminated: &ActorId) -> Vec<WatchNotification> {
        self.watchers
            .remove(terminated)
            .unwrap_or_default()
            .into_iter()
            .map(|watcher| WatchNotification {
                terminated: terminated.clone(),
                watcher,
            })
            .collect()
    }

    /// Get all watchers for a given actor.
    pub fn watchers_of(&self, target: &ActorId) -> Vec<ActorId> {
        self.watchers
            .get(target)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of watched actors.
    pub fn watched_count(&self) -> usize {
        self.watchers.len()
    }
}

impl Default for WatchManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CancelManager
// ---------------------------------------------------------------------------

/// Request to cancel a remote operation.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CancelRequest {
    /// The actor whose operation should be cancelled.
    pub target: ActorId,
    /// The request ID to cancel (for ask/stream/feed correlation).
    pub request_id: Option<String>,
}

/// Acknowledgement of a cancel request.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CancelResponse {
    /// Cancellation was delivered to the target.
    Acknowledged,
    /// The target actor or request was not found.
    NotFound {
        /// Description of what was not found.
        reason: String,
    },
}

/// Manages remote cancellation requests.
///
/// Tracks active cancellation tokens and delivers cancel signals to local
/// actors when requested by remote nodes.
pub struct CancelManager {
    /// Active cancellation tokens: request_id → CancellationToken.
    tokens: HashMap<String, tokio_util::sync::CancellationToken>,
}

impl CancelManager {
    /// Create a new empty CancelManager.
    pub fn new() -> Self {
        Self {
            tokens: HashMap::new(),
        }
    }

    /// Register a cancellation token for a request.
    pub fn register(&mut self, request_id: String, token: tokio_util::sync::CancellationToken) {
        self.tokens.insert(request_id, token);
    }

    /// Process a cancel request. Cancels the token if found.
    pub fn cancel(&mut self, request_id: &str) -> CancelResponse {
        if let Some(token) = self.tokens.remove(request_id) {
            token.cancel();
            CancelResponse::Acknowledged
        } else {
            CancelResponse::NotFound {
                reason: format!("no active request with id '{request_id}'"),
            }
        }
    }

    /// Remove a token after the operation completes (cleanup).
    pub fn remove(&mut self, request_id: &str) {
        self.tokens.remove(request_id);
    }

    /// Number of active cancellable operations.
    pub fn active_count(&self) -> usize {
        self.tokens.len()
    }
}

impl Default for CancelManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// NodeDirectory
// ---------------------------------------------------------------------------

/// Connection status of a peer node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PeerStatus {
    /// Connection is being established.
    Connecting,
    /// Node is connected and reachable.
    Connected,
    /// Node is suspected unreachable (health check failed).
    Unreachable,
    /// Node has been disconnected (gracefully or due to failure).
    Disconnected,
}

/// Metadata about a peer node.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// The peer's node ID.
    pub node_id: NodeId,
    /// Current connection status.
    pub status: PeerStatus,
    /// Optional endpoint address (e.g. "10.0.0.1:4697").
    pub address: Option<String>,
}

/// Maps [`NodeId`]s to peer connection information.
///
/// The NodeDirectory is the runtime's view of the cluster topology. It
/// tracks which nodes are known, their connection status, and optional
/// endpoint addresses.
pub struct NodeDirectory {
    peers: HashMap<NodeId, PeerInfo>,
}

impl NodeDirectory {
    /// Create a new empty directory.
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
        }
    }

    /// Register a peer node.
    pub fn add_peer(&mut self, node_id: NodeId, address: Option<String>) {
        self.peers.insert(
            node_id.clone(),
            PeerInfo {
                node_id,
                status: PeerStatus::Connecting,
                address,
            },
        );
    }

    /// Update the status of a peer.
    pub fn set_status(&mut self, node_id: &NodeId, status: PeerStatus) {
        if let Some(info) = self.peers.get_mut(node_id) {
            info.status = status;
        }
    }

    /// Remove a peer from the directory.
    pub fn remove_peer(&mut self, node_id: &NodeId) {
        self.peers.remove(node_id);
    }

    /// Look up a peer by node ID.
    pub fn get_peer(&self, node_id: &NodeId) -> Option<&PeerInfo> {
        self.peers.get(node_id)
    }

    /// Get all known peer node IDs.
    pub fn peer_nodes(&self) -> Vec<NodeId> {
        self.peers.keys().cloned().collect()
    }

    /// Get all peers with a specific status.
    pub fn peers_with_status(&self, status: PeerStatus) -> Vec<&PeerInfo> {
        self.peers.values().filter(|p| p.status == status).collect()
    }

    /// Number of known peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Number of connected peers.
    pub fn connected_count(&self) -> usize {
        self.peers
            .values()
            .filter(|p| p.status == PeerStatus::Connected)
            .count()
    }

    /// Check if a node is known and connected.
    pub fn is_connected(&self, node_id: &NodeId) -> bool {
        self.peers
            .get(node_id)
            .map(|p| p.status == PeerStatus::Connected)
            .unwrap_or(false)
    }
}

impl Default for NodeDirectory {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Handshake types (version compatibility)
// ---------------------------------------------------------------------------

/// Information sent by a node when initiating a version handshake.
///
/// Both nodes exchange a `HandshakeRequest` during connection setup.
/// The receiving node compares the request against its own configuration
/// and returns a [`HandshakeResponse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeRequest {
    /// The node's identity.
    pub node_id: NodeId,
    /// The wire protocol version this node speaks.
    pub wire_version: crate::version::WireVersion,
    /// The application version, if configured. Informational only — does
    /// not affect compatibility.
    pub app_version: Option<String>,
    /// The actor framework adapter (e.g. "ractor", "kameo", "coerce").
    pub adapter: String,
}

impl HandshakeRequest {
    /// Build a [`HandshakeRequest`] from runtime configuration.
    ///
    /// Uses [`DACTOR_WIRE_VERSION`](crate::version::DACTOR_WIRE_VERSION) as
    /// the wire version. This is the canonical way for adapters to construct
    /// a handshake request — avoids duplicating version/field logic across
    /// adapter crates.
    pub fn from_runtime(
        node_id: NodeId,
        app_version: Option<String>,
        adapter: impl Into<String>,
    ) -> Self {
        Self {
            node_id,
            wire_version: crate::version::WireVersion::parse(
                crate::version::DACTOR_WIRE_VERSION,
            )
            .expect("DACTOR_WIRE_VERSION must be valid"),
            app_version,
            adapter: adapter.into(),
        }
    }
}

/// The result of a version handshake.
///
/// Returned by the remote node after comparing its own configuration
/// against the incoming [`HandshakeRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeResponse {
    /// The remote node accepted the handshake. The cluster can proceed.
    Accepted {
        /// The remote node's identity.
        node_id: NodeId,
        /// The remote node's wire protocol version.
        wire_version: crate::version::WireVersion,
        /// The remote node's application version, if configured.
        app_version: Option<String>,
        /// The remote node's adapter name.
        adapter: String,
    },
    /// The remote node rejected the handshake.
    Rejected {
        /// The remote node's identity.
        node_id: NodeId,
        /// The remote node's wire protocol version.
        wire_version: crate::version::WireVersion,
        /// Why the handshake was rejected.
        reason: RejectionReason,
        /// Human-readable detail message.
        detail: String,
    },
}

/// Reason a version handshake was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    /// The remote node's wire protocol MAJOR version differs. Nodes cannot
    /// form a cluster (Category 1 — infrastructure-level change).
    IncompatibleProtocol,
    /// The remote node uses a different actor framework adapter. Mixed-adapter
    /// clusters are rejected by default.
    IncompatibleAdapter,
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectionReason::IncompatibleProtocol => write!(f, "incompatible wire protocol"),
            RejectionReason::IncompatibleAdapter => write!(f, "incompatible adapter"),
        }
    }
}

/// Validate a remote node's [`HandshakeRequest`] against local configuration.
///
/// Returns a [`HandshakeResponse`] indicating whether the connection is accepted.
///
/// The check order is:
/// 1. Wire protocol compatibility (same MAJOR version)
/// 2. Adapter match (must be the same adapter)
pub fn validate_handshake(
    local: &HandshakeRequest,
    remote: &HandshakeRequest,
) -> HandshakeResponse {
    // Check wire protocol compatibility first
    if !local.wire_version.is_compatible(&remote.wire_version) {
        return HandshakeResponse::Rejected {
            node_id: local.node_id.clone(),
            wire_version: local.wire_version,
            reason: RejectionReason::IncompatibleProtocol,
            detail: format!(
                "wire version {remote} is incompatible with local {local} \
                 (different MAJOR version)",
                remote = remote.wire_version,
                local = local.wire_version,
            ),
        };
    }

    // Check adapter match
    if local.adapter != remote.adapter {
        return HandshakeResponse::Rejected {
            node_id: local.node_id.clone(),
            wire_version: local.wire_version,
            reason: RejectionReason::IncompatibleAdapter,
            detail: format!(
                "adapter \"{remote}\" does not match local \"{local}\"",
                remote = remote.adapter,
                local = local.adapter,
            ),
        };
    }

    // All checks passed
    HandshakeResponse::Accepted {
        node_id: local.node_id.clone(),
        wire_version: local.wire_version,
        app_version: local.app_version.clone(),
        adapter: local.adapter.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- SpawnManager tests --

    #[test]
    fn spawn_manager_create_actor() {
        let mut registry = crate::type_registry::TypeRegistry::new();
        registry.register_factory("test::Worker", |bytes: &[u8]| {
            if bytes.len() != 8 {
                return Err(SerializationError::new("expected 8 bytes"));
            }
            let val = u64::from_be_bytes(bytes.try_into().unwrap());
            Ok(Box::new(val))
        });

        let manager = SpawnManager::new(registry);
        let request = SpawnRequest {
            type_name: "test::Worker".into(),
            args_bytes: 42u64.to_be_bytes().to_vec(),
            name: "worker-1".into(),
            request_id: "req-1".into(),
        };

        let actor = manager.create_actor(&request).unwrap();
        let val = actor.downcast::<u64>().unwrap();
        assert_eq!(*val, 42);
    }

    #[test]
    fn spawn_manager_unknown_type() {
        let registry = crate::type_registry::TypeRegistry::new();
        let manager = SpawnManager::new(registry);
        let request = SpawnRequest {
            type_name: "unknown::Type".into(),
            args_bytes: vec![],
            name: "x".into(),
            request_id: "req-2".into(),
        };

        let result = manager.create_actor(&request);
        assert!(result.is_err());
    }

    #[test]
    fn spawn_manager_records_spawned() {
        let registry = crate::type_registry::TypeRegistry::new();
        let mut manager = SpawnManager::new(registry);
        assert!(manager.spawned_actors().is_empty());

        let id = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        manager.record_spawn(id.clone());
        assert_eq!(manager.spawned_actors().len(), 1);
        assert_eq!(manager.spawned_actors()[0], id);
    }

    // -- WatchManager tests --

    #[test]
    fn watch_and_terminate() {
        let mut wm = WatchManager::new();
        let target = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        let watcher = ActorId {
            node: NodeId("n2".into()),
            local: 10,
        };

        wm.watch(target.clone(), watcher.clone());
        assert_eq!(wm.watched_count(), 1);
        assert_eq!(wm.watchers_of(&target).len(), 1);

        let notifications = wm.on_terminated(&target);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].terminated, target);
        assert_eq!(notifications[0].watcher, watcher);
        assert_eq!(wm.watched_count(), 0);
    }

    #[test]
    fn watch_multiple_watchers() {
        let mut wm = WatchManager::new();
        let target = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        let w1 = ActorId {
            node: NodeId("n2".into()),
            local: 10,
        };
        let w2 = ActorId {
            node: NodeId("n3".into()),
            local: 20,
        };

        wm.watch(target.clone(), w1.clone());
        wm.watch(target.clone(), w2.clone());
        assert_eq!(wm.watchers_of(&target).len(), 2);

        let notifications = wm.on_terminated(&target);
        assert_eq!(notifications.len(), 2);
    }

    #[test]
    fn unwatch_removes_subscription() {
        let mut wm = WatchManager::new();
        let target = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        let watcher = ActorId {
            node: NodeId("n2".into()),
            local: 10,
        };

        wm.watch(target.clone(), watcher.clone());
        wm.unwatch(&target, &watcher);
        assert_eq!(wm.watched_count(), 0);

        let notifications = wm.on_terminated(&target);
        assert!(notifications.is_empty());
    }

    #[test]
    fn terminate_unwatched_actor_returns_empty() {
        let mut wm = WatchManager::new();
        let target = ActorId {
            node: NodeId("n1".into()),
            local: 99,
        };
        let notifications = wm.on_terminated(&target);
        assert!(notifications.is_empty());
    }

    // -- CancelManager tests --

    #[test]
    fn cancel_registered_request() {
        let mut cm = CancelManager::new();
        let token = tokio_util::sync::CancellationToken::new();
        let token_check = token.clone();

        cm.register("req-1".into(), token);
        assert_eq!(cm.active_count(), 1);

        let response = cm.cancel("req-1");
        assert!(matches!(response, CancelResponse::Acknowledged));
        assert!(token_check.is_cancelled());
        assert_eq!(cm.active_count(), 0);
    }

    #[test]
    fn cancel_unknown_request_returns_not_found() {
        let mut cm = CancelManager::new();
        let response = cm.cancel("nonexistent");
        assert!(matches!(response, CancelResponse::NotFound { .. }));
    }

    #[test]
    fn remove_cleans_up_token() {
        let mut cm = CancelManager::new();
        let token = tokio_util::sync::CancellationToken::new();
        cm.register("req-1".into(), token);
        assert_eq!(cm.active_count(), 1);

        cm.remove("req-1");
        assert_eq!(cm.active_count(), 0);
    }

    // -- NodeDirectory tests --

    #[test]
    fn add_and_query_peers() {
        let mut dir = NodeDirectory::new();
        dir.add_peer(NodeId("n1".into()), Some("10.0.0.1:4697".into()));
        dir.add_peer(NodeId("n2".into()), None);

        assert_eq!(dir.peer_count(), 2);
        assert!(!dir.is_connected(&NodeId("n1".into())));

        let info = dir.get_peer(&NodeId("n1".into())).unwrap();
        assert_eq!(info.status, PeerStatus::Connecting);
        assert_eq!(info.address.as_deref(), Some("10.0.0.1:4697"));
    }

    #[test]
    fn set_status_and_filter() {
        let mut dir = NodeDirectory::new();
        dir.add_peer(NodeId("n1".into()), None);
        dir.add_peer(NodeId("n2".into()), None);
        dir.add_peer(NodeId("n3".into()), None);

        dir.set_status(&NodeId("n1".into()), PeerStatus::Connected);
        dir.set_status(&NodeId("n2".into()), PeerStatus::Connected);
        dir.set_status(&NodeId("n3".into()), PeerStatus::Unreachable);

        assert_eq!(dir.connected_count(), 2);
        assert!(dir.is_connected(&NodeId("n1".into())));
        assert!(!dir.is_connected(&NodeId("n3".into())));

        let connected = dir.peers_with_status(PeerStatus::Connected);
        assert_eq!(connected.len(), 2);

        let unreachable = dir.peers_with_status(PeerStatus::Unreachable);
        assert_eq!(unreachable.len(), 1);
    }

    #[test]
    fn remove_peer() {
        let mut dir = NodeDirectory::new();
        dir.add_peer(NodeId("n1".into()), None);
        assert_eq!(dir.peer_count(), 1);

        dir.remove_peer(&NodeId("n1".into()));
        assert_eq!(dir.peer_count(), 0);
        assert!(dir.get_peer(&NodeId("n1".into())).is_none());
    }

    #[test]
    fn peer_nodes_list() {
        let mut dir = NodeDirectory::new();
        dir.add_peer(NodeId("n1".into()), None);
        dir.add_peer(NodeId("n2".into()), None);

        let nodes = dir.peer_nodes();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn spawn_response_variants() {
        let success = SpawnResponse::Success {
            request_id: "r1".into(),
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 42,
            },
        };
        assert!(matches!(success, SpawnResponse::Success { .. }));

        let failure = SpawnResponse::Failure {
            request_id: "r2".into(),
            error: "type not found".into(),
        };
        assert!(matches!(failure, SpawnResponse::Failure { .. }));
    }

    #[test]
    fn watch_notification_fields() {
        let notif = WatchNotification {
            terminated: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            watcher: ActorId {
                node: NodeId("n2".into()),
                local: 2,
            },
        };
        assert_eq!(notif.terminated.local, 1);
        assert_eq!(notif.watcher.local, 2);
    }

    #[test]
    fn peer_status_transitions() {
        let mut dir = NodeDirectory::new();
        dir.add_peer(NodeId("n1".into()), None);

        // Connecting → Connected → Unreachable → Disconnected
        assert_eq!(
            dir.get_peer(&NodeId("n1".into())).unwrap().status,
            PeerStatus::Connecting
        );
        dir.set_status(&NodeId("n1".into()), PeerStatus::Connected);
        assert_eq!(
            dir.get_peer(&NodeId("n1".into())).unwrap().status,
            PeerStatus::Connected
        );
        dir.set_status(&NodeId("n1".into()), PeerStatus::Unreachable);
        assert_eq!(
            dir.get_peer(&NodeId("n1".into())).unwrap().status,
            PeerStatus::Unreachable
        );
        dir.set_status(&NodeId("n1".into()), PeerStatus::Disconnected);
        assert_eq!(
            dir.get_peer(&NodeId("n1".into())).unwrap().status,
            PeerStatus::Disconnected
        );
    }

    /// Guard against accidental changes to wire protocol constants.
    ///
    /// These strings are sent over the network between nodes. Changing them
    /// breaks cross-node compatibility. If this test fails, you almost
    /// certainly need to revert your change to the constant — the wire
    /// value must stay the same even if the Rust struct is renamed.
    #[test]
    fn wire_protocol_constants_are_stable() {
        assert_eq!(
            SYSTEM_MSG_TYPE_SPAWN,
            "dactor::system_actors::SpawnRequest",
            "SYSTEM_MSG_TYPE_SPAWN is a wire protocol value — do not change"
        );
        assert_eq!(
            SYSTEM_MSG_TYPE_WATCH,
            "dactor::system_actors::WatchRequest",
            "SYSTEM_MSG_TYPE_WATCH is a wire protocol value — do not change"
        );
        assert_eq!(
            SYSTEM_MSG_TYPE_UNWATCH,
            "dactor::system_actors::UnwatchRequest",
            "SYSTEM_MSG_TYPE_UNWATCH is a wire protocol value — do not change"
        );
        assert_eq!(
            SYSTEM_MSG_TYPE_CANCEL,
            "dactor::system_actors::CancelRequest",
            "SYSTEM_MSG_TYPE_CANCEL is a wire protocol value — do not change"
        );
        assert_eq!(
            SYSTEM_MSG_TYPE_CONNECT_PEER,
            "dactor::system_actors::ConnectPeer",
            "SYSTEM_MSG_TYPE_CONNECT_PEER is a wire protocol value — do not change"
        );
        assert_eq!(
            SYSTEM_MSG_TYPE_DISCONNECT_PEER,
            "dactor::system_actors::DisconnectPeer",
            "SYSTEM_MSG_TYPE_DISCONNECT_PEER is a wire protocol value — do not change"
        );
    }

    // -- Handshake validation tests --

    fn make_handshake_request(
        node_id: &str,
        wire: &str,
        adapter: &str,
        app_version: Option<&str>,
    ) -> HandshakeRequest {
        HandshakeRequest {
            node_id: NodeId(node_id.into()),
            wire_version: crate::version::WireVersion::parse(wire).unwrap(),
            app_version: app_version.map(|s| s.into()),
            adapter: adapter.into(),
        }
    }

    #[test]
    fn handshake_same_version_and_adapter_accepted() {
        let local = make_handshake_request("node-a", "0.2.0", "ractor", None);
        let remote = make_handshake_request("node-b", "0.2.0", "ractor", None);
        let resp = validate_handshake(&local, &remote);
        assert!(matches!(resp, HandshakeResponse::Accepted { .. }));
    }

    #[test]
    fn handshake_same_major_different_minor_accepted() {
        let local = make_handshake_request("node-a", "0.2.0", "ractor", None);
        let remote = make_handshake_request("node-b", "0.3.0", "ractor", None);
        let resp = validate_handshake(&local, &remote);
        assert!(matches!(resp, HandshakeResponse::Accepted { .. }));
    }

    #[test]
    fn handshake_different_major_rejected() {
        let local = make_handshake_request("node-a", "0.2.0", "ractor", None);
        let remote = make_handshake_request("node-b", "1.0.0", "ractor", None);
        let resp = validate_handshake(&local, &remote);
        match resp {
            HandshakeResponse::Rejected { reason, detail, .. } => {
                assert_eq!(reason, RejectionReason::IncompatibleProtocol);
                assert!(detail.contains("MAJOR"));
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn handshake_different_adapter_rejected() {
        let local = make_handshake_request("node-a", "0.2.0", "ractor", None);
        let remote = make_handshake_request("node-b", "0.2.0", "kameo", None);
        let resp = validate_handshake(&local, &remote);
        match resp {
            HandshakeResponse::Rejected { reason, detail, .. } => {
                assert_eq!(reason, RejectionReason::IncompatibleAdapter);
                assert!(detail.contains("kameo"));
                assert!(detail.contains("ractor"));
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn handshake_protocol_checked_before_adapter() {
        let local = make_handshake_request("node-a", "0.2.0", "ractor", None);
        let remote = make_handshake_request("node-b", "1.0.0", "kameo", None);
        let resp = validate_handshake(&local, &remote);
        // Protocol mismatch should be reported even though adapter also differs
        match resp {
            HandshakeResponse::Rejected { reason, .. } => {
                assert_eq!(reason, RejectionReason::IncompatibleProtocol);
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn handshake_accepted_carries_local_info() {
        let local = make_handshake_request("node-a", "0.2.0", "ractor", Some("1.0.0"));
        let remote = make_handshake_request("node-b", "0.2.0", "ractor", Some("2.0.0"));
        match validate_handshake(&local, &remote) {
            HandshakeResponse::Accepted {
                node_id,
                wire_version,
                app_version,
                adapter,
            } => {
                assert_eq!(node_id, NodeId("node-a".into()));
                assert_eq!(wire_version.to_string(), "0.2.0");
                assert_eq!(app_version.as_deref(), Some("1.0.0"));
                assert_eq!(adapter, "ractor");
            }
            _ => panic!("expected Accepted"),
        }
    }

    #[test]
    fn rejection_reason_display() {
        assert_eq!(
            RejectionReason::IncompatibleProtocol.to_string(),
            "incompatible wire protocol"
        );
        assert_eq!(
            RejectionReason::IncompatibleAdapter.to_string(),
            "incompatible adapter"
        );
    }
}
