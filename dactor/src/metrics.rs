//! Per-actor metrics with lock-free recording and windowed queries.
//!
//! Each actor gets its own [`ActorMetricsHandle`] at spawn time. The handle
//! uses atomic counters for message and error counts — no global lock per
//! message. A shared [`MetricsRegistry`] holds `Arc<ActorMetricsHandle>`
//! references; its lock is only taken when actors are created or destroyed.
//!
//! [`MetricsInterceptor`] is an [`InboundInterceptor`] that holds a direct
//! `Arc<ActorMetricsHandle>` and records metrics without any global map lookup.

use std::any::Any;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::interceptor::*;
use crate::message::{Headers, RuntimeHeaders};
use crate::node::ActorId;

// ---------------------------------------------------------------------------
// RuntimeMetrics
// ---------------------------------------------------------------------------

/// System-level runtime metrics snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeMetrics {
    /// Total actors tracked.
    pub actor_count: usize,
    /// Total messages across all actors (all-time).
    pub total_messages: u64,
    /// Total errors across all actors (all-time).
    pub total_errors: u64,
    /// Messages per second within the window.
    pub message_rate: f64,
    /// Errors per second within the window.
    pub error_rate: f64,
    /// The window duration used for this snapshot.
    pub window: Duration,
}

// ---------------------------------------------------------------------------
// MetricsBucket (internal)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// ActorMetricsSnapshot
// ---------------------------------------------------------------------------

/// Immutable point-in-time snapshot of an actor's metrics.
#[derive(Debug, Clone)]
pub struct ActorMetricsSnapshot {
    /// Total messages received (all-time).
    pub message_count: u64,
    /// Total errors (all-time).
    pub error_count: u64,
    /// Messages per second within the window.
    pub message_rate: f64,
    /// Errors per second within the window.
    pub error_rate: f64,
    /// Message counts by type within the window.
    pub message_counts_by_type: HashMap<String, u64>,
    /// Average handler latency within the window.
    pub avg_latency: Option<Duration>,
    /// Maximum handler latency within the window.
    pub max_latency: Option<Duration>,
    /// P99 handler latency within the window.
    pub p99_latency: Option<Duration>,
}

// ---------------------------------------------------------------------------
// ActorMetricsHandle
// ---------------------------------------------------------------------------

/// Per-actor metrics handle. Uses atomics for all-time counters and a
/// per-actor `Mutex` for windowed bucket data.
///
/// **Single actor:** The actor task is the only writer, so the Mutex has
/// zero contention on the write path. Readers (metrics queries) briefly
/// contend but are infrequent.
///
/// **Actor pool:** Multiple workers share one handle, so the Mutex is
/// contended proportionally to pool throughput. For very high-throughput
/// pools, consider per-worker handles merged at query time.
pub struct ActorMetricsHandle {
    // All-time counters (lock-free recording path)
    message_count: AtomicU64,
    error_count: AtomicU64,
    // Windowed bucket data (per-actor, low contention: single writer)
    inner: Mutex<HandleInner>,
    window: Duration,
    bucket_duration: Duration,
}

struct HandleInner {
    buckets: VecDeque<MetricsBucket>,
}

impl HandleInner {
    fn new() -> Self {
        Self {
            buckets: VecDeque::new(),
        }
    }

    fn evict_expired(&mut self, window: Duration) {
        let now = Instant::now();
        while let Some(front) = self.buckets.front() {
            if now.duration_since(front.timestamp) > window {
                self.buckets.pop_front();
            } else {
                break;
            }
        }
    }

    fn current_bucket(
        &mut self,
        bucket_duration: Duration,
        window: Duration,
    ) -> &mut MetricsBucket {
        let now = Instant::now();
        self.evict_expired(window);

        let need_new = match self.buckets.back() {
            Some(last) => now.duration_since(last.timestamp) >= bucket_duration,
            None => true,
        };

        if need_new {
            self.buckets.push_back(MetricsBucket::new(now));
        }

        self.buckets.back_mut().unwrap()
    }

    fn active_buckets(&self, window: Duration) -> impl Iterator<Item = &MetricsBucket> {
        let now = Instant::now();
        self.buckets
            .iter()
            .filter(move |b| now.duration_since(b.timestamp) <= window)
    }

    /// Windowed message count (for rate computation).
    fn windowed_message_count(&self, window: Duration) -> u64 {
        self.active_buckets(window).map(|b| b.message_count).sum()
    }

    /// Windowed error count (for rate computation).
    fn windowed_error_count(&self, window: Duration) -> u64 {
        self.active_buckets(window).map(|b| b.error_count).sum()
    }
}

impl ActorMetricsHandle {
    /// Create a new per-actor handle with the given window and bucket duration.
    pub fn new(window: Duration, bucket_duration: Duration) -> Self {
        Self {
            message_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            inner: Mutex::new(HandleInner::new()),
            window,
            bucket_duration,
        }
    }

    /// Record a message receipt. Atomic increment + brief per-actor lock for
    /// windowed bucket data.
    pub fn record_message(&self, message_type: &str) {
        self.message_count.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        let bucket = inner.current_bucket(self.bucket_duration, self.window);
        bucket.message_count += 1;
        *bucket
            .message_counts_by_type
            .entry(message_type.to_string())
            .or_insert(0) += 1;
    }

    /// Record an error. Atomic increment + brief per-actor lock.
    pub fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        let bucket = inner.current_bucket(self.bucket_duration, self.window);
        bucket.error_count += 1;
    }

    /// Record a handler latency. Brief per-actor lock.
    pub fn record_latency(&self, latency: Duration) {
        let mut inner = self.inner.lock().unwrap();
        let bucket = inner.current_bucket(self.bucket_duration, self.window);
        bucket.latencies.push(latency);
    }

    /// All-time message count (lock-free read).
    pub fn message_count(&self) -> u64 {
        self.message_count.load(Ordering::Relaxed)
    }

    /// All-time error count (lock-free read).
    pub fn error_count(&self) -> u64 {
        self.error_count.load(Ordering::Relaxed)
    }

    /// Create a point-in-time snapshot of this actor's metrics.
    pub fn snapshot(&self) -> ActorMetricsSnapshot {
        let message_count = self.message_count.load(Ordering::Relaxed);
        let error_count = self.error_count.load(Ordering::Relaxed);

        let inner = self.inner.lock().unwrap();
        let secs = self.window.as_secs_f64();

        let windowed_messages = inner.windowed_message_count(self.window);
        let windowed_errors = inner.windowed_error_count(self.window);

        // Merged by-type counts from active buckets
        let mut by_type = HashMap::new();
        for bucket in inner.active_buckets(self.window) {
            for (k, v) in &bucket.message_counts_by_type {
                *by_type.entry(k.clone()).or_insert(0) += v;
            }
        }

        // Latencies from active buckets
        let latencies: Vec<Duration> = inner
            .active_buckets(self.window)
            .flat_map(|b| b.latencies.iter().copied())
            .collect();

        let avg_latency = if latencies.is_empty() {
            None
        } else {
            let total: Duration = latencies.iter().sum();
            Some(total / latencies.len() as u32)
        };

        let max_latency = latencies.iter().max().copied();

        let p99_latency = if latencies.is_empty() {
            None
        } else {
            let mut sorted = latencies;
            sorted.sort();
            let idx = ((sorted.len() as f64) * 0.99).ceil() as usize - 1;
            Some(sorted[idx.min(sorted.len() - 1)])
        };

        ActorMetricsSnapshot {
            message_count,
            error_count,
            message_rate: if secs > 0.0 {
                windowed_messages as f64 / secs
            } else {
                0.0
            },
            error_rate: if secs > 0.0 {
                windowed_errors as f64 / secs
            } else {
                0.0
            },
            message_counts_by_type: by_type,
            avg_latency,
            max_latency,
            p99_latency,
        }
    }
}

impl std::fmt::Debug for ActorMetricsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorMetricsHandle")
            .field("message_count", &self.message_count.load(Ordering::Relaxed))
            .field("error_count", &self.error_count.load(Ordering::Relaxed))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// MetricsRegistry
// ---------------------------------------------------------------------------

/// Central registry of per-actor metrics handles.
///
/// The registry lock is only taken when actors are registered (spawned) or
/// unregistered (stopped). Recording metrics on a handle requires no
/// registry lock at all.
#[derive(Clone)]
pub struct MetricsRegistry {
    handles: Arc<Mutex<HashMap<ActorId, Arc<ActorMetricsHandle>>>>,
    window: Duration,
    bucket_duration: Duration,
}

impl MetricsRegistry {
    /// Create a new registry.
    ///
    /// * `window` — sliding window duration (e.g., 60 seconds). Only events
    ///   within this window contribute to windowed rates and latencies.
    /// * `bucket_duration` — granularity of each time bucket (e.g., 1 second).
    pub fn new(window: Duration, bucket_duration: Duration) -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            window,
            bucket_duration,
        }
    }

    /// Register an actor and return its metrics handle.
    pub fn register(&self, actor_id: ActorId) -> Arc<ActorMetricsHandle> {
        let handle = Arc::new(ActorMetricsHandle::new(self.window, self.bucket_duration));
        self.handles
            .lock()
            .unwrap()
            .insert(actor_id, handle.clone());
        handle
    }

    /// Register a pre-existing handle under the given actor ID.
    /// Used for pools where all workers share a single handle.
    pub fn register_handle(&self, actor_id: ActorId, handle: Arc<ActorMetricsHandle>) {
        self.handles.lock().unwrap().insert(actor_id, handle);
    }

    /// Create a new handle without registering it.
    /// Useful for pools that create the handle before knowing the final ID.
    pub fn create_handle(&self) -> Arc<ActorMetricsHandle> {
        Arc::new(ActorMetricsHandle::new(self.window, self.bucket_duration))
    }

    /// Remove an actor's metrics handle.
    pub fn unregister(&self, actor_id: &ActorId) {
        self.handles.lock().unwrap().remove(actor_id);
    }

    /// Get the metrics handle for a specific actor.
    pub fn get(&self, actor_id: &ActorId) -> Option<Arc<ActorMetricsHandle>> {
        self.handles.lock().unwrap().get(actor_id).cloned()
    }

    /// Snapshot metrics for all registered actors.
    pub fn all(&self) -> Vec<(ActorId, ActorMetricsSnapshot)> {
        let handles = self.handles.lock().unwrap();
        handles
            .iter()
            .map(|(id, h)| (id.clone(), h.snapshot()))
            .collect()
    }

    /// Number of registered actors.
    pub fn actor_count(&self) -> usize {
        self.handles.lock().unwrap().len()
    }

    /// Total messages across all actors (all-time, from atomics).
    pub fn total_messages(&self) -> u64 {
        let handles = self.handles.lock().unwrap();
        handles.values().map(|h| h.message_count()).sum()
    }

    /// Total errors across all actors (all-time, from atomics).
    pub fn total_errors(&self) -> u64 {
        let handles = self.handles.lock().unwrap();
        handles.values().map(|h| h.error_count()).sum()
    }

    /// Point-in-time snapshot of system-level metrics.
    pub fn runtime_metrics(&self) -> RuntimeMetrics {
        let handles = self.handles.lock().unwrap();
        let total_messages: u64 = handles.values().map(|h| h.message_count()).sum();
        let total_errors: u64 = handles.values().map(|h| h.error_count()).sum();

        // Windowed rates: sum windowed counts across all handles
        let mut windowed_messages: u64 = 0;
        let mut windowed_errors: u64 = 0;
        for h in handles.values() {
            let inner = h.inner.lock().unwrap();
            windowed_messages += inner.windowed_message_count(h.window);
            windowed_errors += inner.windowed_error_count(h.window);
        }

        let secs = self.window.as_secs_f64();
        RuntimeMetrics {
            actor_count: handles.len(),
            total_messages,
            total_errors,
            message_rate: if secs > 0.0 {
                windowed_messages as f64 / secs
            } else {
                0.0
            },
            error_rate: if secs > 0.0 {
                windowed_errors as f64 / secs
            } else {
                0.0
            },
            window: self.window,
        }
    }
}

/// Default: 60-second window with 1-second buckets.
impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), Duration::from_secs(1))
    }
}

// ---------------------------------------------------------------------------
// MetricsInterceptor
// ---------------------------------------------------------------------------

/// Built-in inbound interceptor that records per-actor metrics.
///
/// Each instance holds a direct `Arc<ActorMetricsHandle>` — no global map
/// lookup on the message path. Counters use atomics; the only lock is a
/// per-actor `Mutex` with minimal contention (single writer).
///
/// ```ignore
/// let registry = MetricsRegistry::default();
/// let handle = registry.register(actor_id.clone());
/// let interceptor = MetricsInterceptor::new(handle);
/// // Register as inbound interceptor...
/// // Later:
/// let snapshot = registry.get(&actor_id).unwrap().snapshot();
/// println!("Messages: {}", snapshot.message_count);
/// ```
pub struct MetricsInterceptor {
    handle: Arc<ActorMetricsHandle>,
}

impl MetricsInterceptor {
    /// Create a new `MetricsInterceptor` backed by the given handle.
    pub fn new(handle: Arc<ActorMetricsHandle>) -> Self {
        Self { handle }
    }

    /// Get a reference to the underlying [`ActorMetricsHandle`].
    pub fn handle(&self) -> &Arc<ActorMetricsHandle> {
        &self.handle
    }
}

impl InboundInterceptor for MetricsInterceptor {
    fn name(&self) -> &'static str {
        "metrics"
    }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        self.handle.record_message(ctx.message_type);
        Disposition::Continue
    }

    fn on_complete(
        &self,
        _ctx: &InboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let latency = runtime_headers.timestamp.elapsed();
        self.handle.record_latency(latency);

        if matches!(outcome, Outcome::HandlerError { .. }) {
            self.handle.record_error();
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
        ActorId {
            node: NodeId("n1".into()),
            local,
        }
    }

    #[test]
    fn test_handle_counts() {
        let h = ActorMetricsHandle::new(test_window(), test_bucket());
        h.record_message("Increment");
        h.record_message("Increment");
        h.record_message("GetCount");
        assert_eq!(h.message_count(), 3);
        let snap = h.snapshot();
        assert_eq!(snap.message_count, 3);
        assert_eq!(snap.message_counts_by_type["Increment"], 2);
        assert_eq!(snap.message_counts_by_type["GetCount"], 1);
    }

    #[test]
    fn test_handle_errors() {
        let h = ActorMetricsHandle::new(test_window(), test_bucket());
        h.record_message("Msg");
        h.record_error();
        assert_eq!(h.error_count(), 1);
        assert_eq!(h.message_count(), 1);
    }

    #[test]
    fn test_handle_latencies() {
        let h = ActorMetricsHandle::new(test_window(), test_bucket());
        h.record_latency(Duration::from_millis(10));
        h.record_latency(Duration::from_millis(20));
        h.record_latency(Duration::from_millis(30));
        let snap = h.snapshot();
        assert_eq!(snap.avg_latency.unwrap(), Duration::from_millis(20));
        assert_eq!(snap.max_latency.unwrap(), Duration::from_millis(30));
    }

    #[test]
    fn test_handle_rates() {
        let h = ActorMetricsHandle::new(test_window(), test_bucket());
        h.record_message("A");
        h.record_message("A");
        h.record_error();
        let snap = h.snapshot();
        // 2 messages in a 60s window → ~0.033 msg/s
        assert!(snap.message_rate > 0.0);
        assert!(snap.error_rate > 0.0);
    }

    #[test]
    fn test_registry_per_actor() {
        let registry = MetricsRegistry::new(test_window(), test_bucket());
        let id1 = make_id(1);
        let id2 = make_id(2);

        let h1 = registry.register(id1.clone());
        let h2 = registry.register(id2.clone());

        h1.record_message("A");
        h1.record_message("B");
        h2.record_message("A");

        assert_eq!(h1.message_count(), 2);
        assert_eq!(h2.message_count(), 1);
        assert_eq!(registry.total_messages(), 3);
        assert_eq!(registry.actor_count(), 2);
    }

    #[test]
    fn test_registry_errors() {
        let registry = MetricsRegistry::new(test_window(), test_bucket());
        let id = make_id(1);
        let h = registry.register(id);

        h.record_message("Msg");
        h.record_error();
        assert_eq!(h.error_count(), 1);
        assert_eq!(registry.total_errors(), 1);
    }

    #[test]
    fn test_registry_empty() {
        let registry = MetricsRegistry::new(test_window(), test_bucket());
        assert!(registry.get(&make_id(999)).is_none());
        assert_eq!(registry.total_messages(), 0);
        assert_eq!(registry.actor_count(), 0);
    }

    #[test]
    fn test_metrics_interceptor_name() {
        let handle = Arc::new(ActorMetricsHandle::new(test_window(), test_bucket()));
        let interceptor = MetricsInterceptor::new(handle);
        assert_eq!(interceptor.name(), "metrics");
    }

    #[test]
    fn test_p99_latency() {
        let h = ActorMetricsHandle::new(test_window(), test_bucket());
        for i in 1..=100 {
            h.record_latency(Duration::from_millis(i));
        }
        let snap = h.snapshot();
        assert!(snap.p99_latency.unwrap() >= Duration::from_millis(99));
    }

    #[test]
    fn test_runtime_metrics_snapshot_includes_rates() {
        let registry = MetricsRegistry::new(test_window(), test_bucket());
        let id = make_id(1);
        let h = registry.register(id);

        h.record_message("A");
        h.record_message("B");

        let snapshot = registry.runtime_metrics();
        assert_eq!(snapshot.actor_count, 1);
        assert_eq!(snapshot.total_messages, 2);
        assert_eq!(snapshot.total_errors, 0);
        assert_eq!(snapshot.window, test_window());
        assert!(snapshot.message_rate > 0.0);
        assert_eq!(snapshot.error_rate, 0.0);
    }

    #[test]
    fn test_default_registry() {
        let registry = MetricsRegistry::default();
        assert_eq!(registry.total_messages(), 0);
        let snapshot = registry.runtime_metrics();
        assert_eq!(snapshot.window, Duration::from_secs(60));
    }

    #[test]
    fn test_shared_handle_for_pool() {
        let registry = MetricsRegistry::new(test_window(), test_bucket());
        let handle = registry.create_handle();
        let pool_id = make_id(100);

        // Multiple "workers" share the same handle
        handle.record_message("A");
        handle.record_message("B");
        handle.record_message("A");

        // Register under the pool's ActorId
        registry.register_handle(pool_id.clone(), handle.clone());

        assert_eq!(handle.message_count(), 3);
        assert_eq!(registry.total_messages(), 3);
        assert_eq!(registry.actor_count(), 1);
        let snap = handle.snapshot();
        assert_eq!(snap.message_counts_by_type["A"], 2);
        assert_eq!(snap.message_counts_by_type["B"], 1);
    }

    #[test]
    fn test_unregister() {
        let registry = MetricsRegistry::new(test_window(), test_bucket());
        let id = make_id(1);
        let h = registry.register(id.clone());
        h.record_message("A");

        assert_eq!(registry.actor_count(), 1);
        registry.unregister(&id);
        assert_eq!(registry.actor_count(), 0);
        assert!(registry.get(&id).is_none());
    }
}
