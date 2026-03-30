/// A handle to a scheduled timer that can be cancelled.
pub trait TimerHandle: Send + 'static {
    /// Cancel the timer. Idempotent — calling cancel on an already-cancelled
    /// or fired timer is a no-op.
    fn cancel(self);
}
