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

/// How the message was sent to the actor.
///
/// Interceptors and actor context use this to distinguish between
/// fire-and-forget, request-reply, and streaming delivery modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SendMode {
    /// Fire-and-forget: no reply expected.
    Tell,
    /// Request-reply: caller awaits a single response.
    Ask,
    /// Streaming request: handler pushes multiple items to a `StreamSender`.
    Stream,
    /// Batch feed: deliver pre-collected items without per-item replies.
    Feed,
}

/// Metadata about an inbound message and its target actor.
///
/// Provided to [`InboundInterceptor::on_receive`] and
/// [`InboundInterceptor::on_complete`] so interceptors can make
/// context-aware decisions (e.g., rate-limit only remote messages).
pub struct InboundContext<'a> {
    pub actor_id: ActorId,
    pub actor_name: &'a str,
    pub message_type: &'static str,
    pub send_mode: SendMode,
    pub remote: bool,
    pub origin_node: Option<NodeId>,
}

/// The decision returned by an interceptor after inspecting a message.
///
/// Controls whether the message proceeds through the pipeline,
/// is delayed, dropped, rejected, or bounced back with a retry hint.
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

/// Result reported to interceptors after handler execution completes.
///
/// The reply (for ask) is type-erased — interceptors can downcast via
/// `reply.downcast_ref::<ConcreteReply>()` if they know the concrete type.
/// Use for metrics, logging, or audit trails.
pub enum Outcome<'a> {
    /// Tell: handler returned successfully. No reply value.
    TellSuccess,
    /// Ask: handler returned a reply successfully.
    /// The reply is type-erased for interceptor inspection.
    AskSuccess { reply: &'a dyn Any },
    /// Handler returned an error or panicked.
    HandlerError { error: ActorError },
    /// Stream completed normally (future use).
    StreamCompleted { items_emitted: u64 },
    /// Stream was cancelled (future use).
    StreamCancelled { items_emitted: u64 },
}

impl<'a> std::fmt::Debug for Outcome<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TellSuccess => write!(f, "TellSuccess"),
            Self::AskSuccess { .. } => write!(f, "AskSuccess"),
            Self::HandlerError { error } => write!(f, "HandlerError({:?})", error),
            Self::StreamCompleted { items_emitted } => write!(f, "StreamCompleted({})", items_emitted),
            Self::StreamCancelled { items_emitted } => write!(f, "StreamCancelled({})", items_emitted),
        }
    }
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
    /// For ask, the `Outcome::AskSuccess` variant carries the type-erased reply.
    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let _ = (ctx, runtime_headers, headers, outcome);
    }

    /// Called for each item in a stream or feed.
    ///
    /// - **Stream (server-streaming):** called when the handler emits each
    ///   reply item via `StreamSender::send()`.
    /// - **Feed (client-streaming):** called when each input item is
    ///   delivered to the actor via `StreamReceiver`.
    ///
    /// `seq` is a zero-based sequence number within this stream/feed.
    /// The item is type-erased; downcast if you know the concrete type.
    ///
    /// This is an observation hook. For per-item rejection, cancel the
    /// stream via [`CancellationToken`](tokio_util::sync::CancellationToken).
    fn on_stream_item(
        &self,
        ctx: &InboundContext<'_>,
        headers: &Headers,
        seq: u64,
        item: &dyn Any,
    ) {
        let _ = (ctx, headers, seq, item);
    }
}

/// Metadata about an outbound message, provided to outbound interceptors.
///
/// Available in [`OutboundInterceptor::on_send`] and
/// [`OutboundInterceptor::on_reply`] for sender-side decision-making.
pub struct OutboundContext<'a> {
    /// The target actor's ID.
    pub target_id: ActorId,
    /// The target actor's name.
    pub target_name: &'a str,
    /// The Rust type name of the message.
    pub message_type: &'static str,
    /// How the message is being sent.
    pub send_mode: SendMode,
    /// Whether the target is on a remote node.
    pub remote: bool,
}

/// An outbound interceptor that runs on the SENDER side before the message
/// is delivered to the target actor's mailbox.
///
/// Use cases: tracing context propagation, rate limiting, circuit breaking,
/// header stamping, metrics, logging.
pub trait OutboundInterceptor: Send + Sync + 'static {
    /// Human-readable name.
    fn name(&self) -> &'static str;

    /// Called before the message is sent. Can modify headers, delay, reject,
    /// or retry. The message body is provided as `&dyn Any` for inspection.
    fn on_send(
        &self,
        ctx: &OutboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        headers: &mut Headers,
        message: &dyn Any,
    ) -> Disposition {
        let _ = (ctx, runtime_headers, headers, message);
        Disposition::Continue
    }

    /// Called when an ask() reply is received back on the sender side.
    /// The reply is type-erased — downcast if you know the type.
    ///
    /// **Note:** Not yet wired in TestRuntime — will be connected when
    /// the reply path flows through the outbound pipeline (future PR).
    fn on_reply(
        &self,
        ctx: &OutboundContext<'_>,
        runtime_headers: &RuntimeHeaders,
        headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let _ = (ctx, runtime_headers, headers, outcome);
    }

    /// Called for each item flowing through a stream or feed on the sender side.
    ///
    /// - **Stream (server-streaming):** called when each reply item arrives
    ///   back at the caller.
    /// - **Feed (client-streaming):** called when each input item is about
    ///   to be sent to the target actor. This is where throttling
    ///   interceptors can observe per-item byte sizes via the
    ///   `ContentLength` header (stamped by the transport layer for
    ///   remote actors).
    ///
    /// `seq` is a zero-based sequence number within this stream/feed.
    /// The item is type-erased; downcast if you know the concrete type.
    fn on_stream_item(
        &self,
        ctx: &OutboundContext<'_>,
        headers: &Headers,
        seq: u64,
        item: &dyn Any,
    ) {
        let _ = (ctx, headers, seq, item);
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
        let _ = Outcome::TellSuccess;
        let val = 42u64;
        let _ = Outcome::AskSuccess { reply: &val };
        let _ = Outcome::HandlerError {
            error: ActorError::internal("test"),
        };
        let _ = Outcome::StreamCompleted { items_emitted: 10 };
        let _ = Outcome::StreamCancelled { items_emitted: 5 };
    }
}
