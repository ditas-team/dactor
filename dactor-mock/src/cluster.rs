use std::collections::HashMap;
use std::sync::Arc;

use dactor::node::NodeId;

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
}
