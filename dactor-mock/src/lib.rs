//! Mock multi-node cluster for testing dactor actor systems.
//!
//! Provides `MockCluster` for simulating a cluster of actor nodes
//! in a single process. Use for unit testing cross-node messaging,
//! fault injection, and cluster behavior without real networking.

pub mod cluster;
pub mod network;
pub mod node;

pub use cluster::MockCluster;
pub use network::MockNetwork;
pub use node::MockNode;
