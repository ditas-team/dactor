use std::collections::HashMap;
use std::sync::Arc;

use dactor::node::NodeId;
use dactor::remote::ClusterState;

use crate::network::MockNetwork;
use crate::node::MockNode;

/// A simulated multi-node cluster for testing.
///
/// Each node has its own runtime. Cross-node messaging goes through
/// the MockNetwork which can simulate failures.
pub struct MockCluster {
    nodes: HashMap<NodeId, MockNode>,
    network: Arc<MockNetwork>,
}

impl MockCluster {
    /// Create a cluster with the given node IDs.
    pub fn new(node_ids: &[&str]) -> Self {
        let mut nodes = HashMap::new();
        for id in node_ids {
            let node_id = NodeId(id.to_string());
            nodes.insert(node_id.clone(), MockNode::new(node_id));
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
    /// **Note:** This removes the node from cluster tracking. Actors with
    /// existing `ActorRef` handles may continue running if references are
    /// held. True actor termination semantics will be added when the mock
    /// runtime supports a shutdown-all-actors API.
    pub fn crash_node(&mut self, id: &str) {
        self.nodes.remove(&NodeId(id.to_string()));
    }

    /// Restart a node — creates a fresh node with same ID.
    /// Old actors from the previous runtime are not explicitly stopped
    /// (see `crash_node` note).
    pub fn restart_node(&mut self, id: &str) {
        let node_id = NodeId(id.to_string());
        self.nodes.insert(node_id.clone(), MockNode::new(node_id));
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
}
