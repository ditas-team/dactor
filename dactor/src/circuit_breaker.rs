//! Circuit breaker interceptor for per-actor fault isolation.
//!
//! Tracks error rates per actor and trips the circuit when the error count
//! within a sliding time window exceeds a configurable threshold.
//!
//! # States
//!
//! - **Closed** — normal operation, messages flow through.
//! - **Open** — too many errors; all messages are rejected immediately.
//! - **HalfOpen** — cooldown elapsed; one probe message is allowed through
//!   to test whether the actor has recovered.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::interceptor::*;
use crate::message::{Headers, RuntimeHeaders};
use crate::node::ActorId;

/// The three states of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal — messages flow through.
    Closed,
    /// Tripped — messages are rejected immediately.
    Open,
    /// Testing — one probe message is allowed through.
    HalfOpen,
}

/// Per-actor bookkeeping for the circuit breaker.
struct CircuitBreakerState {
    state: CircuitState,
    /// Timestamps of recent errors (within the error window).
    error_timestamps: VecDeque<Instant>,
    /// When the circuit was last opened. Used to determine cooldown expiry.
    opened_at: Option<Instant>,
    /// Whether a probe message is currently in flight (HalfOpen).
    probe_in_flight: bool,
}

impl CircuitBreakerState {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            error_timestamps: VecDeque::new(),
            opened_at: None,
            probe_in_flight: false,
        }
    }

    /// Remove error timestamps older than the window.
    fn prune_errors(&mut self, window: Duration, now: Instant) {
        while let Some(&front) = self.error_timestamps.front() {
            if now.duration_since(front) > window {
                self.error_timestamps.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Inbound interceptor that implements the circuit breaker pattern.
///
/// When an actor's error count within a sliding time window exceeds
/// `error_threshold`, the circuit opens and all subsequent messages are
/// rejected. After `cooldown` elapses the circuit enters half-open state,
/// allowing a single probe message through. If the probe succeeds the
/// circuit closes; if it fails the circuit re-opens.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use dactor::circuit_breaker::CircuitBreakerInterceptor;
///
/// let cb = CircuitBreakerInterceptor::new(
///     5,                          // trip after 5 errors
///     Duration::from_secs(60),    // within a 60-second window
///     Duration::from_secs(30),    // stay open for 30 seconds
/// );
/// ```
pub struct CircuitBreakerInterceptor {
    /// Number of errors within the window required to trip the circuit.
    error_threshold: u32,
    /// Sliding window for counting errors.
    error_window: Duration,
    /// How long the circuit stays open before transitioning to half-open.
    cooldown: Duration,
    /// Per-actor circuit state.
    states: Mutex<HashMap<ActorId, CircuitBreakerState>>,
}

impl CircuitBreakerInterceptor {
    /// Create a new circuit breaker interceptor.
    ///
    /// * `error_threshold` — number of errors within `error_window` to trip.
    /// * `error_window`    — sliding window for error counting.
    /// * `cooldown`        — duration the circuit stays open before half-open.
    pub fn new(error_threshold: u32, error_window: Duration, cooldown: Duration) -> Self {
        Self {
            error_threshold,
            error_window,
            cooldown,
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Query the current circuit state for an actor.
    /// Returns `Closed` if the actor has no recorded state.
    pub fn state_for(&self, actor_id: &ActorId) -> CircuitState {
        let states = self.states.lock().unwrap();
        states
            .get(actor_id)
            .map_or(CircuitState::Closed, |s| s.state)
    }
}

impl InboundInterceptor for CircuitBreakerInterceptor {
    fn name(&self) -> &'static str {
        "circuit-breaker"
    }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        let mut states = self.states.lock().unwrap();
        let now = Instant::now();
        let entry = states
            .entry(ctx.actor_id.clone())
            .or_insert_with(CircuitBreakerState::new);

        match entry.state {
            CircuitState::Closed => Disposition::Continue,
            CircuitState::Open => {
                // Check whether cooldown has elapsed → transition to HalfOpen.
                if let Some(opened_at) = entry.opened_at {
                    if now.duration_since(opened_at) >= self.cooldown {
                        entry.state = CircuitState::HalfOpen;
                        entry.probe_in_flight = true;
                        return Disposition::Continue;
                    }
                }
                Disposition::Reject(format!("circuit breaker open: {}", ctx.actor_name))
            }
            CircuitState::HalfOpen => {
                if entry.probe_in_flight {
                    // A probe is already in flight; reject additional messages.
                    Disposition::Reject(format!("circuit breaker open: {}", ctx.actor_name))
                } else {
                    // Allow the probe message.
                    entry.probe_in_flight = true;
                    Disposition::Continue
                }
            }
        }
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let is_error = matches!(outcome, Outcome::HandlerError { .. });

        let mut states = self.states.lock().unwrap();
        let now = Instant::now();
        let entry = states
            .entry(ctx.actor_id.clone())
            .or_insert_with(CircuitBreakerState::new);

        match entry.state {
            CircuitState::Closed => {
                if is_error {
                    entry.error_timestamps.push_back(now);
                    entry.prune_errors(self.error_window, now);

                    if entry.error_timestamps.len() as u32 >= self.error_threshold {
                        entry.state = CircuitState::Open;
                        entry.opened_at = Some(now);
                    }
                }
            }
            CircuitState::HalfOpen => {
                entry.probe_in_flight = false;
                if is_error {
                    // Probe failed — re-open.
                    entry.state = CircuitState::Open;
                    entry.opened_at = Some(now);
                    entry.error_timestamps.push_back(now);
                    entry.prune_errors(self.error_window, now);
                } else {
                    // Probe succeeded — close and reset errors.
                    entry.state = CircuitState::Closed;
                    entry.error_timestamps.clear();
                    entry.opened_at = None;
                }
            }
            CircuitState::Open => {
                // Shouldn't normally receive on_complete while open,
                // but handle gracefully by recording errors if any.
                if is_error {
                    entry.error_timestamps.push_back(now);
                    entry.prune_errors(self.error_window, now);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ActorError;
    use crate::node::NodeId;

    fn make_actor_id(local: u64) -> ActorId {
        ActorId {
            node: NodeId("test-node".into()),
            local,
        }
    }

    fn make_ctx<'a>(actor_id: &'a ActorId, name: &'a str) -> InboundContext<'a> {
        InboundContext {
            actor_id: actor_id.clone(),
            actor_name: name,
            message_type: "TestMsg",
            send_mode: SendMode::Tell,
            remote: false,
            origin_node: None,
        }
    }

    /// Helper: send on_receive + on_complete(error) for the given interceptor.
    fn send_error(cb: &CircuitBreakerInterceptor, actor_id: &ActorId, name: &str) -> Disposition {
        let ctx = make_ctx(actor_id, name);
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg = 0u64;

        let d = cb.on_receive(&ctx, &rh, &mut headers, &msg);
        if matches!(d, Disposition::Continue) {
            cb.on_complete(
                &ctx,
                &rh,
                &headers,
                &Outcome::HandlerError {
                    error: ActorError::internal("boom"),
                },
            );
        }
        d
    }

    /// Helper: send on_receive + on_complete(success).
    fn send_success(cb: &CircuitBreakerInterceptor, actor_id: &ActorId, name: &str) -> Disposition {
        let ctx = make_ctx(actor_id, name);
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg = 0u64;

        let d = cb.on_receive(&ctx, &rh, &mut headers, &msg);
        if matches!(d, Disposition::Continue) {
            cb.on_complete(&ctx, &rh, &headers, &Outcome::TellSuccess);
        }
        d
    }

    #[test]
    fn messages_flow_normally_under_threshold() {
        let cb =
            CircuitBreakerInterceptor::new(5, Duration::from_secs(60), Duration::from_secs(30));
        let id = make_actor_id(1);

        // 4 errors (threshold = 5) — circuit stays closed
        for _ in 0..4 {
            let d = send_error(&cb, &id, "actor-a");
            assert!(matches!(d, Disposition::Continue));
        }
        assert_eq!(cb.state_for(&id), CircuitState::Closed);

        // Successes don't count toward the threshold
        let d = send_success(&cb, &id, "actor-a");
        assert!(matches!(d, Disposition::Continue));
        assert_eq!(cb.state_for(&id), CircuitState::Closed);
    }

    #[test]
    fn circuit_opens_after_n_errors() {
        let cb =
            CircuitBreakerInterceptor::new(3, Duration::from_secs(60), Duration::from_secs(30));
        let id = make_actor_id(1);

        for _ in 0..3 {
            send_error(&cb, &id, "actor-a");
        }
        assert_eq!(cb.state_for(&id), CircuitState::Open);
    }

    #[test]
    fn messages_rejected_when_circuit_is_open() {
        let cb =
            CircuitBreakerInterceptor::new(2, Duration::from_secs(60), Duration::from_secs(30));
        let id = make_actor_id(1);

        // Trip the circuit
        for _ in 0..2 {
            send_error(&cb, &id, "actor-a");
        }
        assert_eq!(cb.state_for(&id), CircuitState::Open);

        // Subsequent messages should be rejected
        let ctx = make_ctx(&id, "actor-a");
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg = 0u64;
        let d = cb.on_receive(&ctx, &rh, &mut headers, &msg);
        match d {
            Disposition::Reject(reason) => {
                assert!(reason.contains("circuit breaker open"));
                assert!(reason.contains("actor-a"));
            }
            other => panic!("expected Reject, got {:?}", other),
        }
    }

    #[test]
    fn circuit_transitions_to_half_open_after_cooldown() {
        // Use a tiny cooldown so the test is fast.
        let cb =
            CircuitBreakerInterceptor::new(2, Duration::from_secs(60), Duration::from_millis(1));
        let id = make_actor_id(1);

        // Trip the circuit
        for _ in 0..2 {
            send_error(&cb, &id, "actor-a");
        }
        assert_eq!(cb.state_for(&id), CircuitState::Open);

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(5));

        // Next on_receive should transition to HalfOpen and allow the probe
        let ctx = make_ctx(&id, "actor-a");
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg = 0u64;
        let d = cb.on_receive(&ctx, &rh, &mut headers, &msg);
        assert!(matches!(d, Disposition::Continue));
        assert_eq!(cb.state_for(&id), CircuitState::HalfOpen);
    }

    #[test]
    fn successful_probe_closes_circuit() {
        let cb =
            CircuitBreakerInterceptor::new(2, Duration::from_secs(60), Duration::from_millis(1));
        let id = make_actor_id(1);

        // Trip the circuit
        for _ in 0..2 {
            send_error(&cb, &id, "actor-a");
        }
        assert_eq!(cb.state_for(&id), CircuitState::Open);

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(5));

        // Probe succeeds
        let d = send_success(&cb, &id, "actor-a");
        assert!(matches!(d, Disposition::Continue));
        assert_eq!(cb.state_for(&id), CircuitState::Closed);
    }

    #[test]
    fn failed_probe_reopens_circuit() {
        let cb =
            CircuitBreakerInterceptor::new(2, Duration::from_secs(60), Duration::from_millis(1));
        let id = make_actor_id(1);

        // Trip the circuit
        for _ in 0..2 {
            send_error(&cb, &id, "actor-a");
        }
        assert_eq!(cb.state_for(&id), CircuitState::Open);

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(5));

        // Probe fails
        let d = send_error(&cb, &id, "actor-a");
        assert!(matches!(d, Disposition::Continue)); // probe was allowed through
        assert_eq!(cb.state_for(&id), CircuitState::Open); // re-opened
    }

    #[test]
    fn errors_outside_window_are_pruned() {
        // Window of 1 ms, threshold of 3, cooldown irrelevant
        let cb =
            CircuitBreakerInterceptor::new(3, Duration::from_millis(1), Duration::from_secs(30));
        let id = make_actor_id(1);

        // Record 2 errors
        send_error(&cb, &id, "actor-a");
        send_error(&cb, &id, "actor-a");

        // Wait for the window to expire
        std::thread::sleep(Duration::from_millis(5));

        // One more error shouldn't trip the circuit (old errors pruned)
        send_error(&cb, &id, "actor-a");
        assert_eq!(cb.state_for(&id), CircuitState::Closed);
    }

    #[test]
    fn independent_per_actor_state() {
        let cb =
            CircuitBreakerInterceptor::new(2, Duration::from_secs(60), Duration::from_secs(30));
        let id1 = make_actor_id(1);
        let id2 = make_actor_id(2);

        // Trip actor 1
        for _ in 0..2 {
            send_error(&cb, &id1, "actor-1");
        }
        assert_eq!(cb.state_for(&id1), CircuitState::Open);

        // Actor 2 should still be closed
        assert_eq!(cb.state_for(&id2), CircuitState::Closed);
        let d = send_success(&cb, &id2, "actor-2");
        assert!(matches!(d, Disposition::Continue));
    }

    #[test]
    fn half_open_rejects_extra_messages_while_probe_in_flight() {
        let cb =
            CircuitBreakerInterceptor::new(2, Duration::from_secs(60), Duration::from_millis(1));
        let id = make_actor_id(1);

        // Trip the circuit
        for _ in 0..2 {
            send_error(&cb, &id, "actor-a");
        }

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(5));

        // First message → probe (allowed)
        let ctx = make_ctx(&id, "actor-a");
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg = 0u64;
        let d = cb.on_receive(&ctx, &rh, &mut headers, &msg);
        assert!(matches!(d, Disposition::Continue));

        // Second message while probe is in flight → rejected
        let d2 = cb.on_receive(&ctx, &rh, &mut headers, &msg);
        assert!(matches!(d2, Disposition::Reject(_)));
    }
}
