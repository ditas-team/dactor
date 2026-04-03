//! Built-in observability interceptor and windowed metrics store.
//!
//! [`MetricsInterceptor`] is an [`InboundInterceptor`] that records per-actor
//! message counts, error counts, and handler latencies using tumbling time
//! buckets. Only events within a configurable sliding window are visible to
//! queries, giving a point-in-time view of system health.
//!
//! Metrics are stored in a shared, thread-safe [`MetricsStore`] that callers
//! can query at any time.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::any::Any;

use crate::interceptor::*;
use crate::message::{Headers, RuntimeHeaders};
use crate::node::ActorId;

/// System-level runtime metrics snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeMetrics {
    /// Total actors tracked (that have received at least one message).
    pub actor_count: usize,
    /// Total messages within the window.
    pub total_messages: u64,
    /// Total errors within the window.
    pub total_errors: u64,
    /// Messages per second within the window.
    pub message_rate: f64,
    /// Errors per second within the window.
    pub error_rate: f64,
    /// The window duration used for this snapshot.
    pub window: Duration,
}

/// Metrics for a single time bucket.
#[derive(Debug, Clone)]
struct MetricsBucket {
    timestamp: Instant,
    message_count: u64,
    error_count: u64,
    latencies: Vec<Duration>,
    message_counts_by_type: HashMap<String, u64>,
}

impl MetricsBucket {
    fn new(timestamp: Instant) -> Self {
        Self {
            timestamp,
            message_count: 0,
            error_count: 0,
            latencies: Vec::new(),
            message_counts_by_type: HashMap::new(),
        }
    }
}

/// Per-actor windowed metrics.
///
/// Uses tumbling time buckets to provide a sliding-window view. Old buckets
/// are lazily evicted on record or query operations. Recording is O(1) and
/// queries are O(number of buckets in the window).
#[derive(Debug, Clone)]
pub struct ActorMetrics {
    buckets: VecDeque<MetricsBucket>,
    bucket_duration: Duration,
    window: Duration,
}

impl ActorMetrics {
    fn new(window: Duration, bucket_duration: Duration) -> Self {
        Self {
            buckets: VecDeque::new(),
            bucket_duration,
            window,
        }
    }

    /// Remove buckets older than the window.
    fn evict_expired(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.buckets.front() {
            if now.duration_since(front.timestamp) > self.window {
                self.buckets.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get or create the current bucket.
    fn current_bucket(&mut self) -> &mut MetricsBucket {
        let now = Instant::now();
        self.evict_expired();

        let need_new = match self.buckets.back() {
            Some(last) => now.duration_since(last.timestamp) >= self.bucket_duration,
            None => true,
        };

        if need_new {
            self.buckets.push_back(MetricsBucket::new(now));
        }

        self.buckets.back_mut().unwrap()
    }

    fn record_message(&mut self, message_type: &str) {
        let bucket = self.current_bucket();
        bucket.message_count += 1;
        *bucket
            .message_counts_by_type
            .entry(message_type.to_string())
            .or_insert(0) += 1;
    }

    fn record_error(&mut self) {
        let bucket = self.current_bucket();
        bucket.error_count += 1;
    }

    fn record_latency(&mut self, latency: Duration) {
        let bucket = self.current_bucket();
        bucket.latencies.push(latency);
    }

    /// Iterator over non-expired buckets.
    fn active_buckets(&self) -> impl Iterator<Item = &MetricsBucket> {
        let now = Instant::now();
        let window = self.window;
        self.buckets
            .iter()
            .filter(move |b| now.duration_since(b.timestamp) <= window)
    }

    /// Total messages within the window.
    pub fn message_count(&self) -> u64 {
        self.active_buckets().map(|b| b.message_count).sum()
    }

    /// Total errors within the window.
    pub fn error_count(&self) -> u64 {
        self.active_buckets().map(|b| b.error_count).sum()
    }

    /// Messages per second within the window.
    pub fn message_rate(&self) -> f64 {
        let secs = self.window.as_secs_f64();
        if secs == 0.0 {
            return 0.0;
        }
        self.message_count() as f64 / secs
    }

    /// Errors per second within the window.
    pub fn error_rate(&self) -> f64 {
        let secs = self.window.as_secs_f64();
        if secs == 0.0 {
            return 0.0;
        }
        self.error_count() as f64 / secs
    }

    /// Merged message counts by type across the window.
    pub fn message_counts_by_type(&self) -> HashMap<String, u64> {
        let mut merged = HashMap::new();
        for bucket in self.active_buckets() {
            for (k, v) in &bucket.message_counts_by_type {
                *merged.entry(k.clone()).or_insert(0) += v;
            }
        }
        merged
    }

    /// Collect all latencies across the window.
    fn all_latencies(&self) -> Vec<Duration> {
        self.active_buckets()
            .flat_map(|b| b.latencies.iter().copied())
            .collect()
    }

    /// Average handler latency within the window. Returns `None` if no
    /// latencies recorded.
    pub fn avg_latency(&self) -> Option<Duration> {
        let latencies = self.all_latencies();
        if latencies.is_empty() {
            return None;
        }
        let total: Duration = latencies.iter().sum();
        Some(total / latencies.len() as u32)
    }

    /// Maximum handler latency within the window. Returns `None` if no
    /// latencies recorded.
    pub fn max_latency(&self) -> Option<Duration> {
        self.all_latencies().into_iter().max()
    }

    /// P99 handler latency (approximate) within the window. Returns `None` if
    /// no latencies recorded.
    pub fn p99_latency(&self) -> Option<Duration> {
        let mut sorted = self.all_latencies();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort();
        let idx = ((sorted.len() as f64) * 0.99).ceil() as usize - 1;
        Some(sorted[idx.min(sorted.len() - 1)])
    }
}

/// Shared windowed metrics store. Thread-safe, queryable.
///
/// Uses tumbling time buckets for memory-bounded, time-windowed metrics.
/// Backed by `Arc<Mutex<…>>` so it can be cheaply cloned and shared between the
/// [`MetricsInterceptor`] and application code that wants to read metrics.
#[derive(Clone)]
pub struct MetricsStore {
    inner: Arc<Mutex<HashMap<ActorId, ActorMetrics>>>,
    window: Duration,
    bucket_duration: Duration,
}

impl MetricsStore {
    /// Create a new metrics store.
    ///
    /// * `window` — sliding window duration (e.g., 60 seconds). Only events
    ///   within this window are visible to queries.
    /// * `bucket_duration` — granularity of each time bucket (e.g., 1 second).
    pub fn new(window: Duration, bucket_duration: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            window,
            bucket_duration,
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

    /// Total messages across all actors within the window.
    pub fn total_messages(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|m| m.message_count())
            .sum()
    }

    /// Total errors across all actors within the window.
    pub fn total_errors(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|m| m.error_count())
            .sum()
    }

    /// Number of actors with recorded metrics.
    pub fn actor_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Return a point-in-time snapshot of system-level windowed metrics.
    /// Takes the lock once for a consistent snapshot.
    pub fn runtime_metrics(&self) -> RuntimeMetrics {
        let store = self.inner.lock().unwrap();
        let total_messages: u64 = store.values().map(|m| m.message_count()).sum();
        let total_errors: u64 = store.values().map(|m| m.error_count()).sum();
        let secs = self.window.as_secs_f64();
        RuntimeMetrics {
            actor_count: store.len(),
            total_messages,
            total_errors,
            message_rate: if secs > 0.0 { total_messages as f64 / secs } else { 0.0 },
            error_rate: if secs > 0.0 { total_errors as f64 / secs } else { 0.0 },
            window: self.window,
        }
    }

    fn record_message(&self, actor_id: &ActorId, message_type: &str) {
        let mut store = self.inner.lock().unwrap();
        let metrics = store
            .entry(actor_id.clone())
            .or_insert_with(|| ActorMetrics::new(self.window, self.bucket_duration));
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

/// Default: 60-second window with 1-second buckets.
impl Default for MetricsStore {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), Duration::from_secs(1))
    }
}

/// Built-in inbound interceptor that records per-actor metrics.
///
/// Tracks message counts (total and by type), error counts, and handler
/// latencies using windowed time buckets. Query metrics via the shared
/// [`MetricsStore`].
///
/// ```ignore
/// let store = MetricsStore::default(); // 60s window, 1s buckets
/// let interceptor = MetricsInterceptor::new(store.clone());
/// // Register as inbound interceptor...
/// // Later:
/// let metrics = store.get(&actor_id).unwrap();
/// println!("Messages: {}, Errors: {}", metrics.message_count(), metrics.error_count());
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

    /// Use a large window so all events stay visible during fast tests.
    fn test_window() -> Duration {
        Duration::from_secs(60)
    }

    fn test_bucket() -> Duration {
        Duration::from_secs(1)
    }

    fn make_id(local: u64) -> ActorId {
        ActorId { node: NodeId("n1".into()), local }
    }

    #[test]
    fn test_actor_metrics_counts() {
        let mut m = ActorMetrics::new(test_window(), test_bucket());
        m.record_message("Increment");
        m.record_message("Increment");
        m.record_message("GetCount");
        assert_eq!(m.message_count(), 3);
        let by_type = m.message_counts_by_type();
        assert_eq!(by_type["Increment"], 2);
        assert_eq!(by_type["GetCount"], 1);
    }

    #[test]
    fn test_actor_metrics_errors() {
        let mut m = ActorMetrics::new(test_window(), test_bucket());
        m.record_message("Msg");
        m.record_error();
        assert_eq!(m.error_count(), 1);
        assert_eq!(m.message_count(), 1);
    }

    #[test]
    fn test_actor_metrics_latencies() {
        let mut m = ActorMetrics::new(test_window(), test_bucket());
        m.record_latency(Duration::from_millis(10));
        m.record_latency(Duration::from_millis(20));
        m.record_latency(Duration::from_millis(30));
        assert_eq!(m.avg_latency().unwrap(), Duration::from_millis(20));
        assert_eq!(m.max_latency().unwrap(), Duration::from_millis(30));
    }

    #[test]
    fn test_actor_metrics_rates() {
        let mut m = ActorMetrics::new(test_window(), test_bucket());
        m.record_message("A");
        m.record_message("A");
        m.record_error();
        // 2 messages in a 60s window → ~0.033 msg/s
        assert!(m.message_rate() > 0.0);
        assert!(m.error_rate() > 0.0);
    }

    #[test]
    fn test_metrics_store_per_actor() {
        let store = MetricsStore::new(test_window(), test_bucket());
        let id1 = make_id(1);
        let id2 = make_id(2);

        store.record_message(&id1, "A");
        store.record_message(&id1, "B");
        store.record_message(&id2, "A");

        assert_eq!(store.get(&id1).unwrap().message_count(), 2);
        assert_eq!(store.get(&id2).unwrap().message_count(), 1);
        assert_eq!(store.total_messages(), 3);
        assert_eq!(store.actor_count(), 2);
    }

    #[test]
    fn test_metrics_store_errors() {
        let store = MetricsStore::new(test_window(), test_bucket());
        let id = make_id(1);
        store.record_message(&id, "Msg");
        store.record_error(&id);
        assert_eq!(store.get(&id).unwrap().error_count(), 1);
        assert_eq!(store.total_errors(), 1);
    }

    #[test]
    fn test_metrics_store_empty() {
        let store = MetricsStore::new(test_window(), test_bucket());
        assert!(store.get(&make_id(999)).is_none());
        assert_eq!(store.total_messages(), 0);
        assert_eq!(store.actor_count(), 0);
    }

    #[test]
    fn test_metrics_interceptor_name() {
        let store = MetricsStore::new(test_window(), test_bucket());
        let interceptor = MetricsInterceptor::new(store);
        assert_eq!(interceptor.name(), "metrics");
    }

    #[test]
    fn test_p99_latency() {
        let mut m = ActorMetrics::new(test_window(), test_bucket());
        for i in 1..=100 {
            m.record_latency(Duration::from_millis(i));
        }
        let p99 = m.p99_latency().unwrap();
        assert!(p99 >= Duration::from_millis(99));
    }

    #[test]
    fn test_runtime_metrics_snapshot_includes_rates() {
        let store = MetricsStore::new(test_window(), test_bucket());
        let id = make_id(1);
        store.record_message(&id, "A");
        store.record_message(&id, "B");

        let snapshot = store.runtime_metrics();
        assert_eq!(snapshot.actor_count, 1);
        assert_eq!(snapshot.total_messages, 2);
        assert_eq!(snapshot.total_errors, 0);
        assert_eq!(snapshot.window, test_window());
        assert!(snapshot.message_rate > 0.0);
        assert_eq!(snapshot.error_rate, 0.0);
    }

    #[test]
    fn test_default_store() {
        let store = MetricsStore::default();
        assert_eq!(store.total_messages(), 0);
        // Verify default window/bucket via runtime_metrics
        let snapshot = store.runtime_metrics();
        assert_eq!(snapshot.window, Duration::from_secs(60));
    }
}
