use std::time::Instant;

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
