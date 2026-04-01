use std::sync::Mutex;

use dactor::node::NodeId;

/// Simulated network between mock nodes.
/// Routes messages and can inject faults (partitions, latency, drops).
pub struct MockNetwork {
    partitions: Mutex<Vec<(NodeId, NodeId)>>,
    delivered: Mutex<u64>,
    dropped: Mutex<u64>,
}

impl MockNetwork {
    pub fn new() -> Self {
        Self {
            partitions: Mutex::new(Vec::new()),
            delivered: Mutex::new(0),
            dropped: Mutex::new(0),
        }
    }

    /// Partition two nodes — block all messages between them.
    pub fn partition(&self, a: &NodeId, b: &NodeId) {
        let mut parts = self.partitions.lock().unwrap();
        parts.push((a.clone(), b.clone()));
    }

    /// Remove a partition between two nodes, restoring connectivity.
    pub fn remove_partition(&self, a: &NodeId, b: &NodeId) {
        let mut parts = self.partitions.lock().unwrap();
        parts.retain(|(x, y)| !((x == a && y == b) || (x == b && y == a)));
    }

    /// Check if two nodes are partitioned.
    pub fn is_partitioned(&self, a: &NodeId, b: &NodeId) -> bool {
        let parts = self.partitions.lock().unwrap();
        parts
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
    }

    /// Total messages delivered.
    pub fn delivered_count(&self) -> u64 {
        *self.delivered.lock().unwrap()
    }

    /// Total messages dropped (due to partitions, etc).
    pub fn dropped_count(&self) -> u64 {
        *self.dropped.lock().unwrap()
    }

    /// Check if a message from src to dst should be delivered.
    /// Returns false if nodes are partitioned.
    pub fn can_deliver(&self, src: &NodeId, dst: &NodeId) -> bool {
        if src == dst {
            return true;
        }
        !self.is_partitioned(src, dst)
    }

    /// Record a successful delivery.
    pub fn record_delivered(&self) {
        *self.delivered.lock().unwrap() += 1;
    }

    /// Record a dropped message.
    pub fn record_dropped(&self) {
        *self.dropped.lock().unwrap() += 1;
    }
}

impl Default for MockNetwork {
    fn default() -> Self {
        Self::new()
    }
}
