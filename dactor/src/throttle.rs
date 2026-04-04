use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::interceptor::*;
use crate::message::{Headers, RuntimeHeaders};
use crate::node::ActorId;

/// Per-actor rate limiter — throttles messages if an actor exceeds
/// the configured send rate within a fixed time window.
///
/// Uses a **tumbling window** (not sliding): the counter resets when the
/// window duration expires. This means bursts across window boundaries
/// can momentarily exceed the rate. For most use cases this is acceptable.
///
/// - Under limit → `Continue`
/// - Over limit (1×–2×) → `Delay` with proportional backoff
/// - Severely over (>2×) → `Reject`
///
/// **Note:** Entries for inactive actors accumulate in memory. In systems
/// with high actor churn, consider periodic cleanup or bounded cache.
pub struct ActorRateLimiter {
    max_rate: u64,
    window: Duration,
    counts: Mutex<HashMap<ActorId, (u64, std::time::Instant)>>,
}

impl ActorRateLimiter {
    /// Create a rate limiter with the given max messages per window.
    pub fn new(max_rate: u64, window: Duration) -> Self {
        Self {
            max_rate,
            window,
            counts: Mutex::new(HashMap::new()),
        }
    }
}

impl OutboundInterceptor for ActorRateLimiter {
    fn name(&self) -> &'static str {
        "actor-rate-limiter"
    }

    fn on_send(
        &self,
        ctx: &OutboundContext<'_>,
        _rh: &RuntimeHeaders,
        _h: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        let mut counts = self.counts.lock().unwrap();
        let now = std::time::Instant::now();
        let entry = counts.entry(ctx.target_id.clone()).or_insert((0, now));

        // Reset window if expired
        if now.duration_since(entry.1) > self.window {
            *entry = (1, now); // reset and count this message
        } else {
            entry.0 += 1;
        }
        let count = entry.0;

        if count > self.max_rate * 2 {
            Disposition::Reject(format!(
                "actor {} exceeded 2× rate limit ({} messages, limit {})",
                ctx.target_name, count, self.max_rate
            ))
        } else if count > self.max_rate {
            let backoff_ms = ((count - self.max_rate) * 10).min(1000);
            Disposition::Delay(Duration::from_millis(backoff_ms))
        } else {
            Disposition::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;

    fn make_ctx(target_name: &str) -> (ActorId, String) {
        let id = ActorId {
            node: NodeId("n1".into()),
            local: 1,
        };
        (id, target_name.to_string())
    }

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let limiter = ActorRateLimiter::new(10, Duration::from_secs(1));
        let (id, name) = make_ctx("actor");
        let rh = RuntimeHeaders::new();
        let mut h = Headers::new();
        let msg = 42u64;

        let ctx = OutboundContext {
            target_id: id,
            target_name: &name,
            message_type: "Test",
            send_mode: SendMode::Tell,
            remote: false,
        };

        for _ in 0..10 {
            assert!(matches!(
                limiter.on_send(&ctx, &rh, &mut h, &msg as &dyn Any),
                Disposition::Continue
            ));
        }
    }

    #[test]
    fn test_rate_limiter_delays_over_limit() {
        let limiter = ActorRateLimiter::new(5, Duration::from_secs(1));
        let (id, name) = make_ctx("actor");
        let rh = RuntimeHeaders::new();
        let mut h = Headers::new();
        let msg = 42u64;

        let ctx = OutboundContext {
            target_id: id,
            target_name: &name,
            message_type: "Test",
            send_mode: SendMode::Tell,
            remote: false,
        };

        // First 5: Continue
        for _ in 0..5 {
            assert!(matches!(
                limiter.on_send(&ctx, &rh, &mut h, &msg),
                Disposition::Continue
            ));
        }

        // 6th: Delay
        assert!(matches!(
            limiter.on_send(&ctx, &rh, &mut h, &msg),
            Disposition::Delay(_)
        ));
    }

    #[test]
    fn test_rate_limiter_rejects_severely_over_limit() {
        let limiter = ActorRateLimiter::new(5, Duration::from_secs(1));
        let (id, name) = make_ctx("actor");
        let rh = RuntimeHeaders::new();
        let mut h = Headers::new();
        let msg = 42u64;

        let ctx = OutboundContext {
            target_id: id,
            target_name: &name,
            message_type: "Test",
            send_mode: SendMode::Tell,
            remote: false,
        };

        // Send 11 (> 2 * 5 = 10): should Reject
        for _ in 0..11 {
            limiter.on_send(&ctx, &rh, &mut h, &msg);
        }
        assert!(matches!(
            limiter.on_send(&ctx, &rh, &mut h, &msg),
            Disposition::Reject(_)
        ));
    }

    #[test]
    fn test_rate_limiter_resets_after_window() {
        let limiter = ActorRateLimiter::new(5, Duration::from_millis(1));
        let (id, name) = make_ctx("actor");
        let rh = RuntimeHeaders::new();
        let mut h = Headers::new();
        let msg = 42u64;

        let ctx = OutboundContext {
            target_id: id,
            target_name: &name,
            message_type: "Test",
            send_mode: SendMode::Tell,
            remote: false,
        };

        // Fill window
        for _ in 0..5 {
            limiter.on_send(&ctx, &rh, &mut h, &msg);
        }

        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(5));

        // Should be Continue again
        assert!(matches!(
            limiter.on_send(&ctx, &rh, &mut h, &msg),
            Disposition::Continue
        ));
    }
}
