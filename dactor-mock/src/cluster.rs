use std::collections::HashMap;
use std::sync::Arc;

use dactor::actor::{Actor, Handler};
use dactor::node::{ActorId, NodeId};
use dactor::remote::ClusterState;
use dactor::supervision::ChildTerminated;
use dactor::test_support::test_runtime::TestActorRef;

use crate::network::MockNetwork;
use crate::node::MockNode;

/// A simulated multi-node cluster for testing.
///
/// Each node has its own runtime and system actors (SpawnManager,
/// WatchManager, CancelManager, NodeDirectory). Cross-node messaging
/// goes through the MockNetwork which can simulate failures.
pub struct MockCluster {
    nodes: HashMap<NodeId, MockNode>,
    network: Arc<MockNetwork>,
}

impl MockCluster {
    /// Create a cluster with the given node IDs.
    ///
    /// All nodes are connected to each other in the NodeDirectory.
    pub fn new(node_ids: &[&str]) -> Self {
        let ids: Vec<NodeId> = node_ids.iter().map(|id| NodeId(id.to_string())).collect();
        let mut nodes = HashMap::new();

        for id in &ids {
            let mut node = MockNode::new(id.clone());
            // Connect each node to all other nodes
            for peer in &ids {
                if peer != id {
                    node.connect_peer(peer);
                }
            }
            nodes.insert(id.clone(), node);
        }

        Self {
            nodes,
            network: Arc::new(MockNetwork::new()),
        }
    }

    /// Get a reference to a node.
    pub fn node(&self, id: &str) -> &MockNode {
        self.nodes
            .get(&NodeId(id.to_string()))
            .unwrap_or_else(|| panic!("node '{}' not found in cluster", id))
    }

    /// Get a mutable reference to a node.
    pub fn node_mut(&mut self, id: &str) -> &mut MockNode {
        self.nodes
            .get_mut(&NodeId(id.to_string()))
            .unwrap_or_else(|| panic!("node '{}' not found in cluster", id))
    }

    /// Get the network for fault injection.
    pub fn network(&self) -> &MockNetwork {
        &self.network
    }

    /// Number of nodes in the cluster.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// All node IDs.
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.nodes.keys().cloned().collect()
    }

    /// Remove a node from the cluster.
    ///
    /// Updates all surviving nodes' NodeDirectory to mark the crashed
    /// node as Disconnected.
    pub fn crash_node(&mut self, id: &str) {
        let crashed = NodeId(id.to_string());
        self.nodes.remove(&crashed);
        // Update surviving nodes
        for node in self.nodes.values_mut() {
            node.disconnect_peer(&crashed);
        }
    }

    /// Restart a node — creates a fresh node with same ID.
    /// Reconnects it to all existing peers and updates their directories.
    pub fn restart_node(&mut self, id: &str) {
        let node_id = NodeId(id.to_string());
        let mut new_node = MockNode::new(node_id.clone());
        // Connect new node to all existing peers
        for peer_id in self.nodes.keys() {
            new_node.connect_peer(peer_id);
        }
        // Update existing nodes to know about the restarted node
        for node in self.nodes.values_mut() {
            node.connect_peer(&node_id);
        }
        self.nodes.insert(node_id, new_node);
    }

    /// Freeze a node — removes it from the cluster temporarily.
    /// The returned `MockNode` retains its runtime and actors.
    /// Use `unfreeze_node` to restore it.
    pub fn freeze_node(&mut self, id: &str) -> Option<MockNode> {
        self.nodes.remove(&NodeId(id.to_string()))
    }

    /// Unfreeze a node — restore it to the cluster.
    /// The node is re-inserted using its original `node_id`.
    pub fn unfreeze_node(&mut self, node: MockNode) {
        self.nodes.insert(node.node_id.clone(), node);
    }

    /// Watch an actor from a watcher on the same node.
    /// Both watcher and target must be on nodes in this cluster.
    pub fn watch<W>(&self, watcher_node: &str, watcher: &TestActorRef<W>, target_id: ActorId)
    where
        W: Actor + Handler<ChildTerminated> + 'static,
    {
        let node = self.node(watcher_node);
        node.runtime.watch(watcher, target_id);
    }

    /// Unwatch an actor.
    pub fn unwatch(&self, node_id: &str, watcher_id: &ActorId, target_id: &ActorId) {
        let node = self.node(node_id);
        node.runtime.unwatch(watcher_id, target_id);
    }

    /// Get a ClusterState snapshot. Returns `None` if the cluster is empty.
    pub fn state(&self) -> Option<ClusterState> {
        if self.nodes.is_empty() {
            return None;
        }
        let node_ids: Vec<NodeId> = self.nodes.keys().cloned().collect();
        let local = node_ids[0].clone();
        Some(ClusterState {
            local_node: local,
            nodes: node_ids,
            is_leader: false,
        })
    }

    /// Register a remote watch via the target node's WatchManager.
    pub fn remote_watch(&mut self, target_node: &str, target: ActorId, watcher: ActorId) {
        let node = self.node_mut(target_node);
        node.watch_manager.watch(target.clone(), watcher);
    }

    /// Remove a remote watch via the target node's WatchManager.
    pub fn remote_unwatch(&mut self, target_node: &str, target: &ActorId, watcher: &ActorId) {
        let node = self.node_mut(target_node);
        node.watch_manager.unwatch(target, watcher);
    }

    /// Notify the target node's WatchManager that an actor has terminated.
    /// Returns notifications for all remote watchers of this actor.
    pub fn notify_terminated(
        &mut self,
        node_id: &str,
        terminated: &ActorId,
    ) -> Vec<dactor::system_actors::WatchNotification> {
        let node = self.node_mut(node_id);
        node.watch_manager.on_terminated(terminated)
    }

    /// Register a cancellation token on a node's CancelManager.
    pub fn register_cancel(
        &mut self,
        node_id: &str,
        request_id: String,
        token: tokio_util::sync::CancellationToken,
    ) {
        let node = self.node_mut(node_id);
        node.cancel_manager.register(request_id, token);
    }

    /// Cancel a request on a node's CancelManager.
    pub fn cancel_request(
        &mut self,
        node_id: &str,
        request_id: &str,
    ) -> dactor::system_actors::CancelResponse {
        let node = self.node_mut(node_id);
        node.cancel_manager.cancel(request_id)
    }
}
