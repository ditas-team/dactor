//! # dactor-test-harness
//!
//! Multi-process integration test harness for the [`dactor`] actor framework.
//!
//! Provides utilities for spawning actor nodes as separate processes,
//! coordinating cluster formation, injecting faults, and observing
//! distributed events during integration tests.

pub mod protocol;
pub mod node;
pub mod cluster;
pub mod fault;
pub mod events;

pub use cluster::{TestCluster, TestClusterBuilder};
pub use node::{TestNode, TestNodeConfig};
pub use fault::FaultInjector;
pub use events::EventStream;
