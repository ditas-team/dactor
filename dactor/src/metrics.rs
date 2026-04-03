//! Built-in observability interceptor and metrics store.
//!
//! [`MetricsInterceptor`] is an [`InboundInterceptor`] that records per-actor
//! message counts, error counts, and handler latencies. Metrics are stored in a
//! shared, thread-safe [`MetricsStore`] that callers can query at any time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::any::Any;

use crate::interceptor::*;
use crate::message::{Headers, RuntimeHeaders};
use crate::node::ActorId;

/// System-level runtime metrics snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMetrics {
    /// Total actors tracked (that have received at least one message).
    pub actor_count: usize,
    /// Total messages processed across all actors.
    pub total_messages: u64,
    /// Total errors across all actors.
    pub total_errors: u64,
}

/// Per-actor metrics.
#[derive(Debug, Clone)]
pub struct ActorMetrics {
    /// Total messages received by this actor.
    pub message_count: u64,
    /// Total handler errors (panics or ActorError returns).
    pub error_count: u64,
    /// Total messages by type name.
    pub message_counts_by_type: HashMap<String, u64>,
    /// Handler latencies (most recent N, for computing percentiles).
    pub latencies: Vec<Duration>,
    /// Maximum number of latencies to keep.
    max_latencies: usize,
}

impl ActorMetrics {
    fn new(max_latencies: usize) -> Self {
        Self {
            message_count: 0,
            error_count: 0,
            message_counts_by_type: HashMap::new(),
            latencies: Vec::new(),
            max_latencies,
        }
    }

    fn record_message(&mut self, message_type: &str) {
        self.message_count += 1;
        *self.message_counts_by_type
            .entry(message_type.to_string())
            .or_insert(0) += 1;
    }

    fn record_error(&mut self) {
        self.error_count += 1;
    }

    fn record_latency(&mut self, latency: Duration) {
        if self.latencies.len() >= self.max_latencies {
            self.latencies.remove(0);
        }
        self.latencies.push(latency);
    }

    /// Average handler latency. Returns `None` if no latencies recorded.
    pub fn avg_latency(&self) -> Option<Duration> {
        if self.latencies.is_empty() {
            return None;
        }
        let total: Duration = self.latencies.iter().sum();
        Some(total / self.latencies.len() as u32)
    }

    /// Maximum handler latency. Returns `None` if no latencies recorded.
    pub fn max_latency(&self) -> Option<Duration> {
        self.latencies.iter().max().copied()
    }

    /// P99 handler latency (approximate). Returns `None` if no latencies recorded.
    pub fn p99_latency(&self) -> Option<Duration> {
        if self.latencies.is_empty() { return None; }
        let mut sorted = self.latencies.clone();
        sorted.sort();
        let idx = ((sorted.len() as f64) * 0.99).ceil() as usize - 1;
        Some(sorted[idx.min(sorted.len() - 1)])
    }
}

/// Shared metrics store. Thread-safe, queryable.
///
/// Backed by `Arc<Mutex<…>>` so it can be cheaply cloned and shared between the
/// [`MetricsInterceptor`] and application code that wants to read metrics.
#[derive(Clone)]
pub struct MetricsStore {
    inner: Arc<Mutex<HashMap<ActorId, ActorMetrics>>>,
    max_latencies: usize,
}

impl MetricsStore {
    /// Create a new metrics store.
    /// `max_latencies` controls how many recent latencies to keep per actor.
    pub fn new(max_latencies: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_latencies,
        }
    }

    /// Get metrics for a specific actor. Returns `None` if no messages recorded.
    pub fn get(&self, actor_id: &ActorId) -> Option<ActorMetrics> {
        self.inner.lock().unwrap().get(actor_id).cloned()
    }

    /// Get metrics for all actors.
    pub fn all(&self) -> HashMap<ActorId, ActorMetrics> {
        self.inner.lock().unwrap().clone()
    }

    /// Total messages across all actors.
    pub fn total_messages(&self) -> u64 {
        self.inner.lock().unwrap().values().map(|m| m.message_count).sum()
    }

    /// Total errors across all actors.
    pub fn total_errors(&self) -> u64 {
        self.inner.lock().unwrap().values().map(|m| m.error_count).sum()
    }

    /// Number of actors with recorded metrics.
    pub fn actor_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Return a point-in-time snapshot of system-level metrics.
    /// Takes the lock once for a consistent snapshot.
    pub fn runtime_metrics(&self) -> RuntimeMetrics {
        let store = self.inner.lock().unwrap();
        RuntimeMetrics {
            actor_count: store.len(),
            total_messages: store.values().map(|m| m.message_count).sum(),
            total_errors: store.values().map(|m| m.error_count).sum(),
        }
    }

    fn record_message(&self, actor_id: &ActorId, message_type: &str) {
        let mut store = self.inner.lock().unwrap();
        let metrics = store.entry(actor_id.clone()).or_insert_with(|| ActorMetrics::new(self.max_latencies));
        metrics.record_message(message_type);
    }

    fn record_error(&self, actor_id: &ActorId) {
        let mut store = self.inner.lock().unwrap();
        if let Some(metrics) = store.get_mut(actor_id) {
            metrics.record_error();
        }
    }

    fn record_latency(&self, actor_id: &ActorId, latency: Duration) {
        let mut store = self.inner.lock().unwrap();
        if let Some(metrics) = store.get_mut(actor_id) {
            metrics.record_latency(latency);
        }
    }
}

impl Default for MetricsStore {
    fn default() -> Self { Self::new(1000) }
}

/// Built-in inbound interceptor that records per-actor metrics.
///
/// Tracks message counts (total and by type), error counts, and handler
/// latencies. Query metrics via the shared [`MetricsStore`].
///
/// ```ignore
/// let store = MetricsStore::new(1000);
/// let interceptor = MetricsInterceptor::new(store.clone());
/// // Register as inbound interceptor...
/// // Later:
/// let metrics = store.get(&actor_id).unwrap();
/// println!("Messages: {}, Errors: {}", metrics.message_count, metrics.error_count);
/// ```
pub struct MetricsInterceptor {
    store: MetricsStore,
}

impl MetricsInterceptor {
    /// Create a new `MetricsInterceptor` backed by the given store.
    pub fn new(store: MetricsStore) -> Self {
        Self { store }
    }

    /// Get a reference to the underlying [`MetricsStore`].
    pub fn store(&self) -> &MetricsStore {
        &self.store
    }
}

impl InboundInterceptor for MetricsInterceptor {
    fn name(&self) -> &'static str { "metrics" }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        self.store.record_message(&ctx.actor_id, ctx.message_type);
        Disposition::Continue
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let latency = runtime_headers.timestamp.elapsed();
        self.store.record_latency(&ctx.actor_id, latency);

        if matches!(outcome, Outcome::HandlerError { .. }) {
            self.store.record_error(&ctx.actor_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;

    fn make_id(local: u64) -> ActorId {
        ActorId { node: NodeId("n1".into()), local }
    }

    #[test]
    fn test_actor_metrics_counts() {
        let mut m = ActorMetrics::new(100);
        m.record_message("Increment");
        m.record_message("Increment");
        m.record_message("GetCount");
        assert_eq!(m.message_count, 3);
        assert_eq!(m.message_counts_by_type["Increment"], 2);
        assert_eq!(m.message_counts_by_type["GetCount"], 1);
    }

    #[test]
    fn test_actor_metrics_errors() {
        let mut m = ActorMetrics::new(100);
        m.record_message("Msg");
        m.record_error();
        assert_eq!(m.error_count, 1);
        assert_eq!(m.message_count, 1);
    }

    #[test]
    fn test_actor_metrics_latencies() {
        let mut m = ActorMetrics::new(100);
        m.record_latency(Duration::from_millis(10));
        m.record_latency(Duration::from_millis(20));
        m.record_latency(Duration::from_millis(30));
        assert_eq!(m.avg_latency().unwrap(), Duration::from_millis(20));
        assert_eq!(m.max_latency().unwrap(), Duration::from_millis(30));
    }

    #[test]
    fn test_actor_metrics_latency_cap() {
        let mut m = ActorMetrics::new(3);
        for i in 0..5 {
            m.record_latency(Duration::from_millis(i * 10));
        }
        assert_eq!(m.latencies.len(), 3);
    }

    #[test]
    fn test_metrics_store_per_actor() {
        let store = MetricsStore::new(100);
        let id1 = make_id(1);
        let id2 = make_id(2);

        store.record_message(&id1, "A");
        store.record_message(&id1, "B");
        store.record_message(&id2, "A");

        assert_eq!(store.get(&id1).unwrap().message_count, 2);
        assert_eq!(store.get(&id2).unwrap().message_count, 1);
        assert_eq!(store.total_messages(), 3);
        assert_eq!(store.actor_count(), 2);
    }

    #[test]
    fn test_metrics_store_errors() {
        let store = MetricsStore::new(100);
        let id = make_id(1);
        store.record_message(&id, "Msg");
        store.record_error(&id);
        assert_eq!(store.get(&id).unwrap().error_count, 1);
        assert_eq!(store.total_errors(), 1);
    }

    #[test]
    fn test_metrics_store_empty() {
        let store = MetricsStore::new(100);
        assert!(store.get(&make_id(999)).is_none());
        assert_eq!(store.total_messages(), 0);
        assert_eq!(store.actor_count(), 0);
    }

    #[test]
    fn test_metrics_interceptor_name() {
        let store = MetricsStore::new(100);
        let interceptor = MetricsInterceptor::new(store);
        assert_eq!(interceptor.name(), "metrics");
    }

    #[test]
    fn test_p99_latency() {
        let mut m = ActorMetrics::new(1000);
        for i in 1..=100 {
            m.record_latency(Duration::from_millis(i));
        }
        let p99 = m.p99_latency().unwrap();
        assert!(p99 >= Duration::from_millis(99));
    }
}
