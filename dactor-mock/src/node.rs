use dactor::node::NodeId;
use dactor::system_actors::{CancelManager, NodeDirectory, PeerStatus, SpawnManager, WatchManager};
use dactor::test_support::test_runtime::TestRuntime;
use dactor::type_registry::TypeRegistry;

/// A simulated node in a MockCluster.
///
/// Each node has its own runtime, identity, and system actors for
/// simulated remote operations (spawn, watch, cancel).
pub struct MockNode {
    pub node_id: NodeId,
    pub runtime: TestRuntime,
    /// Manages remote actor spawn requests for this node.
    pub spawn_manager: SpawnManager,
    /// Manages remote watch/unwatch subscriptions for this node.
    pub watch_manager: WatchManager,
    /// Manages remote cancellation requests for this node.
    pub cancel_manager: CancelManager,
    /// Tracks peer node connection state.
    pub node_directory: NodeDirectory,
}

impl MockNode {
    pub fn new(node_id: NodeId) -> Self {
        let runtime = TestRuntime::with_node_id(node_id.clone());
        Self {
            node_id,
            runtime,
            spawn_manager: SpawnManager::new(TypeRegistry::new()),
            watch_manager: WatchManager::new(),
            cancel_manager: CancelManager::new(),
            node_directory: NodeDirectory::new(),
        }
    }

    /// Register an actor type for remote spawning on this node.
    pub fn register_factory(
        &mut self,
        type_name: impl Into<String>,
        factory: impl Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
            + Send
            + Sync
            + 'static,
    ) {
        self.spawn_manager
            .type_registry_mut()
            .register_factory(type_name, factory);
    }

    /// Connect this node to a peer (marks as Connected in directory).
    pub fn connect_peer(&mut self, peer: &NodeId) {
        self.node_directory.add_peer(peer.clone(), None);
        self.node_directory.set_status(peer, PeerStatus::Connected);
    }

    /// Disconnect a peer node.
    pub fn disconnect_peer(&mut self, peer: &NodeId) {
        self.node_directory
            .set_status(peer, PeerStatus::Disconnected);
    }
}
