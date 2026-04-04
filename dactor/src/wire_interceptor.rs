//! Envelope-level interceptor for incoming remote messages.
//!
//! [`WireInterceptor`] operates on [`WireEnvelope`]s at the transport boundary
//! — **before deserialization**. This enables runtime-level load control
//! (delay, reject, drop, rate-limit, prioritize) without paying the cost
//! of deserializing messages that will be discarded.
//!
//! ## Pipeline position
//!
//! ```text
//! Transport → WireInterceptor pipeline → TypeRegistry deserialize → Actor dispatch
//! ```
//!
//! ## Use cases
//!
//! - **Rate limiting by source node** — reject/delay envelopes based on
//!   `target.node` or header values
//! - **Priority routing** — reorder envelope delivery based on priority headers
//! - **Circuit breaker** — drop all envelopes from an unhealthy sender
//! - **Metrics** — count/size incoming envelopes without deserialization
//! - **Authentication** — reject envelopes missing auth headers before dispatch

use std::time::Duration;

use crate::remote::WireEnvelope;

// ---------------------------------------------------------------------------
// WireDisposition
// ---------------------------------------------------------------------------

/// Decision returned by a [`WireInterceptor`] for each incoming envelope.
#[derive(Debug, Clone)]
pub enum WireDisposition {
    /// Accept the envelope and proceed to dispatch.
    Accept,
    /// Delay processing by the specified duration (backpressure).
    Delay(Duration),
    /// Reject the envelope with an error message. For ask envelopes,
    /// a rejection reply will be sent back to the caller.
    Reject(String),
    /// Silently drop the envelope. No reply is sent.
    Drop,
}

// ---------------------------------------------------------------------------
// WireInterceptor trait
// ---------------------------------------------------------------------------

/// Intercepts [`WireEnvelope`]s at the transport boundary before deserialization.
///
/// Unlike [`OutboundInterceptor`](crate::interceptor::OutboundInterceptor)
/// (which runs on the sender side with typed messages) and
/// [`InboundInterceptor`](crate::interceptor::InboundInterceptor) (which
/// runs on the receiver side after deserialization), `WireInterceptor`
/// operates on raw envelopes — headers + bytes. This makes it suitable for
/// load-shedding decisions that should avoid deserialization overhead.
pub trait WireInterceptor: Send + Sync + 'static {
    /// Human-readable name for this interceptor.
    fn name(&self) -> &'static str;

    /// Inspect an incoming wire envelope and decide what to do with it.
    ///
    /// The envelope's `headers`, `message_type`, `send_mode`, and `target`
    /// are available for inspection. The `body` bytes are available but
    /// should NOT be deserialized here (that defeats the purpose).
    fn on_receive(&self, envelope: &WireEnvelope) -> WireDisposition;
}

// ---------------------------------------------------------------------------
// WireInterceptorPipeline
// ---------------------------------------------------------------------------

/// An ordered pipeline of [`WireInterceptor`]s.
///
/// Interceptors run in registration order. The first non-Accept disposition
/// short-circuits the pipeline.
pub struct WireInterceptorPipeline {
    interceptors: Vec<Box<dyn WireInterceptor>>,
}

impl WireInterceptorPipeline {
    /// Create an empty pipeline.
    pub fn new() -> Self {
        Self {
            interceptors: Vec::new(),
        }
    }

    /// Add an interceptor to the end of the pipeline.
    pub fn add(&mut self, interceptor: impl WireInterceptor) {
        self.interceptors.push(Box::new(interceptor));
    }

    /// Run the pipeline on an envelope. Returns the final disposition
    /// along with the name of the interceptor that made the decision
    /// (or `None` if all interceptors accepted).
    pub fn process(&self, envelope: &WireEnvelope) -> (WireDisposition, Option<&'static str>) {
        for interceptor in &self.interceptors {
            match interceptor.on_receive(envelope) {
                WireDisposition::Accept => continue,
                other => return (other, Some(interceptor.name())),
            }
        }
        (WireDisposition::Accept, None)
    }

    /// Process an envelope, returning a result suitable for async callers.
    ///
    /// **Important:** This method does NOT sleep inline for `Delay`
    /// dispositions — it returns `WireProcessResult::Delayed` with the
    /// duration. The caller (transport receive loop) is responsible for
    /// scheduling the delay to avoid head-of-line blocking.
    pub fn process_envelope(
        &self,
        envelope: &WireEnvelope,
    ) -> Result<WireProcessResult, WireRejectError> {
        let (disposition, interceptor_name) = self.process(envelope);
        match disposition {
            WireDisposition::Accept => Ok(WireProcessResult::Accepted),
            WireDisposition::Delay(d) => Ok(WireProcessResult::Delayed(d)),
            WireDisposition::Reject(reason) => Err(WireRejectError {
                interceptor: interceptor_name.unwrap_or("unknown").to_string(),
                reason,
            }),
            WireDisposition::Drop => Ok(WireProcessResult::Dropped),
        }
    }

    /// Number of interceptors in the pipeline.
    pub fn len(&self) -> usize {
        self.interceptors.len()
    }

    /// Whether the pipeline is empty (no interceptors).
    pub fn is_empty(&self) -> bool {
        self.interceptors.is_empty()
    }
}

impl Default for WireInterceptorPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of processing an envelope through the wire interceptor pipeline.
#[derive(Debug, Clone)]
pub enum WireProcessResult {
    /// Envelope accepted for dispatch.
    Accepted,
    /// Envelope should be delayed by the given duration before dispatch.
    /// The caller is responsible for applying the delay to avoid
    /// head-of-line blocking in the transport receive loop.
    Delayed(Duration),
    /// Envelope was silently dropped.
    Dropped,
}

/// Error returned when a wire interceptor rejects an envelope.
#[derive(Debug, Clone)]
pub struct WireRejectError {
    /// Name of the interceptor that rejected the envelope.
    pub interceptor: String,
    /// Reason for rejection.
    pub reason: String,
}

impl std::fmt::Display for WireRejectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wire interceptor '{}' rejected: {}",
            self.interceptor, self.reason
        )
    }
}

impl std::error::Error for WireRejectError {}

// ---------------------------------------------------------------------------
// Built-in wire interceptors
// ---------------------------------------------------------------------------

/// Rejects envelopes that exceed a maximum body size.
///
/// Prevents large messages from consuming memory during deserialization.
pub struct MaxBodySizeInterceptor {
    max_bytes: usize,
}

impl MaxBodySizeInterceptor {
    /// Create a new interceptor that rejects envelopes with bodies larger
    /// than `max_bytes`.
    pub fn new(max_bytes: usize) -> Self {
        Self { max_bytes }
    }
}

impl WireInterceptor for MaxBodySizeInterceptor {
    fn name(&self) -> &'static str {
        "max-body-size"
    }

    fn on_receive(&self, envelope: &WireEnvelope) -> WireDisposition {
        if envelope.body.len() > self.max_bytes {
            WireDisposition::Reject(format!(
                "body size {} exceeds limit {}",
                envelope.body.len(),
                self.max_bytes
            ))
        } else {
            WireDisposition::Accept
        }
    }
}

/// Rate-limits incoming envelopes using a fixed-window counter.
///
/// Tracks the number of envelopes received in the current time window.
/// Envelopes that exceed the rate are **rejected** (not just delayed),
/// ensuring the configured limit is actually enforced.
pub struct RateLimitWireInterceptor {
    /// Maximum envelopes per window.
    max_per_window: u64,
    /// Window duration.
    window: Duration,
    /// Current window state.
    state: std::sync::Mutex<RateLimitState>,
}

struct RateLimitState {
    count: u64,
    window_start: std::time::Instant,
}

impl RateLimitWireInterceptor {
    /// Create a rate limiter that allows `max_per_window` envelopes per
    /// `window` duration. Envelopes exceeding the limit are rejected.
    pub fn new(max_per_window: u64, window: Duration) -> Self {
        Self {
            max_per_window,
            window,
            state: std::sync::Mutex::new(RateLimitState {
                count: 0,
                window_start: std::time::Instant::now(),
            }),
        }
    }
}

impl WireInterceptor for RateLimitWireInterceptor {
    fn name(&self) -> &'static str {
        "rate-limit"
    }

    fn on_receive(&self, _envelope: &WireEnvelope) -> WireDisposition {
        // Recover from mutex poisoning rather than panicking
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = std::time::Instant::now();

        // Reset window if expired
        if now.duration_since(state.window_start) >= self.window {
            state.count = 0;
            state.window_start = now;
        }

        state.count += 1;
        if state.count > self.max_per_window {
            WireDisposition::Reject(format!(
                "rate limit exceeded: {} > {} per {:?}",
                state.count, self.max_per_window, self.window
            ))
        } else {
            WireDisposition::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interceptor::SendMode;
    use crate::node::{ActorId, NodeId};
    use crate::remote::WireHeaders;

    fn test_envelope(body_size: usize) -> WireEnvelope {
        WireEnvelope {
            target: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Msg".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![0u8; body_size],
            request_id: None,
            version: None,
        }
    }

    // -- WireDisposition tests --

    struct AcceptAll;
    impl WireInterceptor for AcceptAll {
        fn name(&self) -> &'static str {
            "accept-all"
        }
        fn on_receive(&self, _: &WireEnvelope) -> WireDisposition {
            WireDisposition::Accept
        }
    }

    struct RejectAll;
    impl WireInterceptor for RejectAll {
        fn name(&self) -> &'static str {
            "reject-all"
        }
        fn on_receive(&self, _: &WireEnvelope) -> WireDisposition {
            WireDisposition::Reject("blocked".into())
        }
    }

    struct DropAll;
    impl WireInterceptor for DropAll {
        fn name(&self) -> &'static str {
            "drop-all"
        }
        fn on_receive(&self, _: &WireEnvelope) -> WireDisposition {
            WireDisposition::Drop
        }
    }

    struct DelayAll(Duration);
    impl WireInterceptor for DelayAll {
        fn name(&self) -> &'static str {
            "delay-all"
        }
        fn on_receive(&self, _: &WireEnvelope) -> WireDisposition {
            WireDisposition::Delay(self.0)
        }
    }

    #[test]
    fn empty_pipeline_accepts() {
        let pipeline = WireInterceptorPipeline::new();
        assert!(pipeline.is_empty());
        let (result, name) = pipeline.process(&test_envelope(10));
        assert!(matches!(result, WireDisposition::Accept));
        assert!(name.is_none());
    }

    #[test]
    fn pipeline_accept_all() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(AcceptAll);
        pipeline.add(AcceptAll);
        let (result, _) = pipeline.process(&test_envelope(10));
        assert!(matches!(result, WireDisposition::Accept));
    }

    #[test]
    fn pipeline_reject_short_circuits() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(AcceptAll);
        pipeline.add(RejectAll);
        pipeline.add(AcceptAll); // should not run
        let (result, name) = pipeline.process(&test_envelope(10));
        assert!(matches!(result, WireDisposition::Reject(_)));
        assert_eq!(name, Some("reject-all"));
    }

    #[test]
    fn pipeline_drop_short_circuits() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(DropAll);
        pipeline.add(AcceptAll); // should not run
        let (result, name) = pipeline.process(&test_envelope(10));
        assert!(matches!(result, WireDisposition::Drop));
        assert_eq!(name, Some("drop-all"));
    }

    #[test]
    fn pipeline_delay_short_circuits() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(AcceptAll);
        pipeline.add(DelayAll(Duration::from_millis(50)));
        pipeline.add(RejectAll); // should not run
        let (result, _) = pipeline.process(&test_envelope(10));
        assert!(matches!(result, WireDisposition::Delay(_)));
    }

    // -- MaxBodySizeInterceptor tests --

    #[test]
    fn max_body_size_accepts_within_limit() {
        let interceptor = MaxBodySizeInterceptor::new(100);
        let result = interceptor.on_receive(&test_envelope(50));
        assert!(matches!(result, WireDisposition::Accept));
    }

    #[test]
    fn max_body_size_accepts_at_limit() {
        let interceptor = MaxBodySizeInterceptor::new(100);
        let result = interceptor.on_receive(&test_envelope(100));
        assert!(matches!(result, WireDisposition::Accept));
    }

    #[test]
    fn max_body_size_rejects_over_limit() {
        let interceptor = MaxBodySizeInterceptor::new(100);
        let result = interceptor.on_receive(&test_envelope(101));
        assert!(matches!(result, WireDisposition::Reject(_)));
        if let WireDisposition::Reject(reason) = result {
            assert!(reason.contains("101"));
            assert!(reason.contains("100"));
        }
    }

    // -- RateLimitWireInterceptor tests --

    #[test]
    fn rate_limit_accepts_within_limit() {
        let rl = RateLimitWireInterceptor::new(3, Duration::from_secs(1));
        let envelope = test_envelope(10);

        assert!(matches!(rl.on_receive(&envelope), WireDisposition::Accept));
        assert!(matches!(rl.on_receive(&envelope), WireDisposition::Accept));
        assert!(matches!(rl.on_receive(&envelope), WireDisposition::Accept));
    }

    #[test]
    fn rate_limit_rejects_over_limit() {
        let rl = RateLimitWireInterceptor::new(2, Duration::from_secs(1));
        let envelope = test_envelope(10);

        assert!(matches!(rl.on_receive(&envelope), WireDisposition::Accept));
        assert!(matches!(rl.on_receive(&envelope), WireDisposition::Accept));
        // Third exceeds the limit — rejected
        let result = rl.on_receive(&envelope);
        assert!(matches!(result, WireDisposition::Reject(_)));
        if let WireDisposition::Reject(reason) = result {
            assert!(reason.contains("rate limit exceeded"));
        }
    }

    // -- process_envelope tests --

    #[test]
    fn process_envelope_accept() {
        let pipeline = WireInterceptorPipeline::new();
        let result = pipeline.process_envelope(&test_envelope(10));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), WireProcessResult::Accepted));
    }

    #[test]
    fn process_envelope_reject() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(RejectAll);
        let result = pipeline.process_envelope(&test_envelope(10));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.interceptor, "reject-all");
        assert!(err.reason.contains("blocked"));
    }

    #[test]
    fn process_envelope_drop() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(DropAll);
        let result = pipeline.process_envelope(&test_envelope(10));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), WireProcessResult::Dropped));
    }

    #[test]
    fn process_envelope_delay() {
        let mut pipeline = WireInterceptorPipeline::new();
        pipeline.add(DelayAll(Duration::from_millis(50)));
        let result = pipeline.process_envelope(&test_envelope(10));
        assert!(result.is_ok());
        if let WireProcessResult::Delayed(d) = result.unwrap() {
            assert_eq!(d, Duration::from_millis(50));
        } else {
            panic!("expected Delayed");
        }
    }

    #[test]
    fn wire_reject_error_display() {
        let err = WireRejectError {
            interceptor: "max-body-size".into(),
            reason: "too large".into(),
        };
        assert_eq!(
            format!("{err}"),
            "wire interceptor 'max-body-size' rejected: too large"
        );
    }

    #[test]
    fn pipeline_len() {
        let mut pipeline = WireInterceptorPipeline::new();
        assert_eq!(pipeline.len(), 0);
        pipeline.add(AcceptAll);
        pipeline.add(RejectAll);
        assert_eq!(pipeline.len(), 2);
    }
}
