use dactor::node::NodeId;
use dactor::test_support::test_runtime::TestRuntime;

/// A simulated node in a MockCluster.
/// Each node has its own runtime and identity.
pub struct MockNode {
    pub node_id: NodeId,
    pub runtime: TestRuntime,
}

impl MockNode {
    pub fn new(node_id: NodeId) -> Self {
        let runtime = TestRuntime::with_node_id(node_id.clone());
        Self { node_id, runtime }
    }
}
