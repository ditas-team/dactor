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

    /// Crash a node — stops all actors immediately.
    pub fn crash_node(&mut self, id: &str) {
        self.nodes.remove(&NodeId(id.to_string()));
    }

    /// Restart a node — creates a fresh node with same ID.
    pub fn restart_node(&mut self, id: &str) {
        let node_id = NodeId(id.to_string());
        self.nodes.insert(node_id.clone(), MockNode::new(node_id));
    }

    /// Freeze a node — pauses are simulated, actors remain but don't process.
    /// For M2, this is a stub that removes the node temporarily.
    pub fn freeze_node(&mut self, id: &str) -> Option<MockNode> {
        self.nodes.remove(&NodeId(id.to_string()))
    }

    /// Unfreeze a node — restore it.
    pub fn unfreeze_node(&mut self, id: &str, node: MockNode) {
        self.nodes.insert(NodeId(id.to_string()), node);
    }

    /// Get a ClusterState snapshot.
    pub fn state(&self) -> ClusterState {
        let node_ids: Vec<NodeId> = self.nodes.keys().cloned().collect();
        let local = node_ids.first().cloned().unwrap_or(NodeId("unknown".into()));
        ClusterState {
            local_node: local,
            nodes: node_ids,
            is_leader: false,
        }
    }
}
