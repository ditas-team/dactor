use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Abstraction over time sources, enabling deterministic testing.
pub trait Clock: Send + Sync + 'static {
    /// Returns a monotonic instant for freshness / elapsed-time checks.
    fn now(&self) -> Instant;

    /// Returns wall-clock time as Unix milliseconds (for incarnation generation).
    fn unix_ms(&self) -> i64;
}

/// Production clock delegating to `std::time`.
#[derive(Debug, Clone)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn unix_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}

/// Test clock with manually controlled time.
///
/// Both the monotonic instant and the Unix millisecond counter advance
/// together when [`advance`](TestClock::advance) is called.
#[derive(Debug, Clone)]
pub struct TestClock {
    inner: Arc<TestClockInner>,
}

#[derive(Debug)]
struct TestClockInner {
    start_instant: Instant,
    elapsed_ms: AtomicI64,
    base_unix_ms: i64,
}

impl TestClock {
    /// Create a new test clock frozen at the current real time.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(TestClockInner {
                start_instant: Instant::now(),
                elapsed_ms: AtomicI64::new(0),
                base_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
            }),
        }
    }

    /// Create a test clock with a specific base Unix time.
    pub fn with_base_unix_ms(base_unix_ms: i64) -> Self {
        Self {
            inner: Arc::new(TestClockInner {
                start_instant: Instant::now(),
                elapsed_ms: AtomicI64::new(0),
                base_unix_ms,
            }),
        }
    }

    /// Advance the clock by the given duration.
    pub fn advance(&self, duration: Duration) {
        self.inner
            .elapsed_ms
            .fetch_add(duration.as_millis() as i64, Ordering::SeqCst);
    }

    /// Get the total elapsed time since creation.
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.inner.elapsed_ms.load(Ordering::SeqCst) as u64)
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.inner.start_instant + self.elapsed()
    }

    fn unix_ms(&self) -> i64 {
        self.inner.base_unix_ms + self.inner.elapsed_ms.load(Ordering::SeqCst)
    }
}
