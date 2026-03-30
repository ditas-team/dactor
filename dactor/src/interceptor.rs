//! Inbound interceptor pipeline.
//!
//! Interceptors form an ordered pipeline executed before the actor's handler.
//! Each interceptor can inspect, modify headers, delay, drop, or reject a message
//! before it reaches the handler. After handler completion, interceptors are
//! notified via `on_complete`.

use std::any::Any;
use std::time::Duration;

use crate::actor::ActorError;
use crate::message::{Headers, RuntimeHeaders};
use crate::node::{ActorId, NodeId};

/// How the message was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    Tell,
    Ask,
    Stream,
    Feed,
}

/// Metadata about the message and its target, provided to interceptors.
pub struct InboundContext<'a> {
    pub actor_id: ActorId,
    pub actor_name: &'a str,
    pub message_type: &'static str,
    pub send_mode: SendMode,
    pub remote: bool,
    pub origin_node: Option<NodeId>,
}

/// Outcome of an interceptor's pre-dispatch decision.
#[derive(Debug)]
pub enum Disposition {
    /// Continue to the next interceptor / deliver the message.
    Continue,
    /// Delay the message by the specified duration before proceeding.
    /// Multiple delays are cumulative.
    Delay(Duration),
    /// Drop the message silently.
    Drop,
    /// Reject the message with a reason.
    Reject(String),
    /// Tell the caller to retry sending the message after the specified duration.
    /// Unlike `Delay` (which holds the message in the pipeline), `Retry` returns
    /// immediately to the caller with `Err(RuntimeError::RetryAfter { .. })`,
    /// letting the caller decide whether and when to resend.
    ///
    /// **Inbound:** The message is NOT delivered. The caller receives the retry
    /// hint and can resend after the suggested delay.
    ///
    /// **Outbound:** The message is NOT sent. Same caller-visible behavior.
    ///
    /// Use cases: circuit breakers, load shedding, backpressure signaling.
    Retry(Duration),
}

/// Outcome reported to interceptors after handler completion.
#[derive(Debug)]
pub enum Outcome {
    /// Tell completed successfully (no reply).
    Success,
    /// Ask completed and reply was sent.
    Replied,
    /// Handler returned an error or panicked.
    HandlerError { error: ActorError },
}

/// An interceptor that observes or modifies inbound messages.
/// Interceptors form an ordered pipeline executed before the handler.
pub trait InboundInterceptor: Send + Sync + 'static {
    /// Human-readable name for this interceptor.
    fn name(&self) -> &'static str;

    /// Called before the message is delivered to the handler.
    /// The message body is provided as `&dyn Any` for optional downcasting.
    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        headers: &mut Headers,
        message: &dyn Any,
    ) -> Disposition {
        let _ = (ctx, runtime_headers, headers, message);
        Disposition::Continue
    }

    /// Called after the handler finishes. Called exactly once per delivered message.
    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        headers: &Headers,
        outcome: &Outcome,
    ) {
        let _ = (ctx, runtime_headers, headers, outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopInterceptor;

    impl InboundInterceptor for NoopInterceptor {
        fn name(&self) -> &'static str {
            "noop"
        }
    }

    #[test]
    fn test_noop_interceptor_defaults() {
        let interceptor = NoopInterceptor;
        assert_eq!(interceptor.name(), "noop");

        let ctx = InboundContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "test",
            message_type: "TestMsg",
            send_mode: SendMode::Tell,
            remote: false,
            origin_node: None,
        };
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg: u64 = 42;
        let disposition = interceptor.on_receive(&ctx, &rh, &mut headers, &msg);
        assert!(matches!(disposition, Disposition::Continue));
    }

    #[test]
    fn test_send_mode_variants() {
        assert_eq!(SendMode::Tell, SendMode::Tell);
        assert_ne!(SendMode::Tell, SendMode::Ask);
        assert_ne!(SendMode::Stream, SendMode::Feed);
    }

    #[test]
    fn test_disposition_variants() {
        let _ = Disposition::Continue;
        let _ = Disposition::Delay(Duration::from_millis(100));
        let _ = Disposition::Drop;
        let _ = Disposition::Reject("forbidden".into());
    }

    #[test]
    fn test_outcome_variants() {
        let _ = Outcome::Success;
        let _ = Outcome::Replied;
        let _ = Outcome::HandlerError {
            error: ActorError::new("test"),
        };
    }
}
