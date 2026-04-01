use dactor::node::NodeId;
use dactor::test_support::v2_test_runtime::V2TestRuntime;

/// A simulated node in a MockCluster.
/// Each node has its own runtime and identity.
pub struct MockNode {
    pub node_id: NodeId,
    pub runtime: V2TestRuntime,
}

impl MockNode {
    pub fn new(node_id: NodeId) -> Self {
        let runtime = V2TestRuntime::with_node_id(node_id.clone());
        Self { node_id, runtime }
    }
}
