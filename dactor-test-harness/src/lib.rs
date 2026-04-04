//! # dactor-test-harness
//!
//! Multi-process integration test harness for the [`dactor`] actor framework.
//!
//! Provides utilities for spawning actor nodes as separate processes,
//! coordinating cluster formation, injecting faults, and observing
//! distributed events during integration tests.

pub mod cluster;
pub mod events;
pub mod fault;
pub mod node;
pub mod protocol;

pub use cluster::{TestCluster, TestClusterBuilder};
pub use events::EventStream;
pub use fault::FaultInjector;
pub use node::{TestNode, TestNodeConfig};
