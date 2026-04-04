//! OpenTelemetry-compatible tracing interceptor.
//!
//! [`OtelInterceptor`] emits `tracing` events for every actor message,
//! providing observability integration without a direct dependency on
//! the `opentelemetry` crate. Users connect to their OTel backend via
//! the standard `tracing-opentelemetry` bridge in their subscriber setup.
//!
//! ## Tracing events emitted
//!
//! - **Inbound (receiver):** `actor.recv` event in `on_receive`, `actor.complete`
//!   event in `on_complete` with outcome status
//! - **Outbound (sender):** `actor.send` event in `on_send`, `actor.reply`
//!   event in `on_reply` with outcome status
//!
//! ## Span lifecycle limitation
//!
//! The interceptor API calls `on_receive` before handler dispatch and
//! `on_complete` after. The interceptor cannot hold a span guard across
//! these calls (no shared state between them). Therefore, events are
//! emitted as discrete tracing events rather than wrapping spans.
//!
//! For full parent-child span trees, use `tracing::instrument` directly
//! in actor handlers.
//!
//! ## Setup
//!
//! ```rust,ignore
//! use dactor::otel::OtelInterceptor;
//!
//! // Add as inbound + outbound interceptor
//! runtime.add_inbound_interceptor(Arc::new(OtelInterceptor::new()));
//! runtime.add_outbound_interceptor(Arc::new(OtelInterceptor::new()));
//!
//! // Wire tracing-opentelemetry in your subscriber (requires user deps:
//! // opentelemetry, opentelemetry-otlp, tracing-opentelemetry)
//! ```

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::interceptor::{
    Disposition, InboundContext, InboundInterceptor, OutboundContext, OutboundInterceptor, Outcome,
    SendMode,
};
use crate::message::{Headers, RuntimeHeaders};

/// Tracing-based interceptor for OpenTelemetry integration.
///
/// Emits structured `tracing` events for actor message send/receive.
/// Works as both [`InboundInterceptor`] and [`OutboundInterceptor`].
pub struct OtelInterceptor {
    traced_count: AtomicU64,
}

impl OtelInterceptor {
    /// Create a new OTel interceptor.
    pub fn new() -> Self {
        Self {
            traced_count: AtomicU64::new(0),
        }
    }

    /// Total messages traced since creation.
    pub fn traced_count(&self) -> u64 {
        self.traced_count.load(Ordering::Relaxed)
    }

    fn send_mode_str(mode: SendMode) -> &'static str {
        match mode {
            SendMode::Tell => "tell",
            SendMode::Ask => "ask",
            SendMode::Stream => "stream",
            SendMode::Feed => "feed",
        }
    }

    fn outcome_status(outcome: &Outcome<'_>) -> &'static str {
        match outcome {
            Outcome::TellSuccess | Outcome::AskSuccess { .. } => "OK",
            Outcome::HandlerError { .. } => "ERROR",
            Outcome::StreamCompleted { .. } => "OK",
            Outcome::StreamCancelled { .. } => "ERROR",
        }
    }
}

impl Default for OtelInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl InboundInterceptor for OtelInterceptor {
    fn name(&self) -> &'static str {
        "otel"
    }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        self.traced_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            actor.id = %ctx.actor_id,
            actor.name = ctx.actor_name,
            message.r#type = ctx.message_type,
            message.send_mode = Self::send_mode_str(ctx.send_mode),
            message.remote = ctx.remote,
            otel.kind = "CONSUMER",
            "actor.recv"
        );
        Disposition::Continue
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        tracing::info!(
            actor.id = %ctx.actor_id,
            actor.name = ctx.actor_name,
            message.r#type = ctx.message_type,
            otel.status_code = Self::outcome_status(outcome),
            "actor.complete"
        );
    }
}

impl OutboundInterceptor for OtelInterceptor {
    fn name(&self) -> &'static str {
        "otel"
    }

    fn on_send(
        &self,
        ctx: &OutboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        self.traced_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            target.id = %ctx.target_id,
            target.name = ctx.target_name,
            message.r#type = ctx.message_type,
            message.send_mode = Self::send_mode_str(ctx.send_mode),
            message.remote = ctx.remote,
            otel.kind = "PRODUCER",
            "actor.send"
        );
        Disposition::Continue
    }

    fn on_reply(
        &self,
        ctx: &OutboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        tracing::info!(
            target.id = %ctx.target_id,
            target.name = ctx.target_name,
            message.r#type = ctx.message_type,
            otel.status_code = Self::outcome_status(outcome),
            "actor.reply"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ActorError;
    use crate::node::{ActorId, NodeId};

    fn test_inbound_ctx() -> InboundContext<'static> {
        InboundContext {
            actor_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            actor_name: "counter",
            message_type: "test::Increment",
            send_mode: SendMode::Tell,
            remote: false,
            origin_node: None,
        }
    }

    fn test_outbound_ctx() -> OutboundContext<'static> {
        OutboundContext {
            target_id: ActorId {
                node: NodeId("n2".into()),
                local: 2,
            },
            target_name: "worker",
            message_type: "test::Task",
            send_mode: SendMode::Ask,
            remote: true,
        }
    }

    #[test]
    fn inbound_on_receive_returns_continue() {
        let otel = OtelInterceptor::new();
        let ctx = test_inbound_ctx();
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg: u64 = 42;
        let result = otel.on_receive(&ctx, &rh, &mut headers, &msg);
        assert!(matches!(result, Disposition::Continue));
        assert_eq!(otel.traced_count(), 1);
    }

    #[test]
    fn inbound_on_complete_success() {
        let otel = OtelInterceptor::new();
        let ctx = test_inbound_ctx();
        let rh = RuntimeHeaders::new();
        let headers = Headers::new();
        otel.on_complete(&ctx, &rh, &headers, &Outcome::TellSuccess);
    }

    #[test]
    fn inbound_on_complete_error() {
        let otel = OtelInterceptor::new();
        let ctx = test_inbound_ctx();
        let rh = RuntimeHeaders::new();
        let headers = Headers::new();
        otel.on_complete(
            &ctx,
            &rh,
            &headers,
            &Outcome::HandlerError {
                error: ActorError::internal("fail"),
            },
        );
    }

    #[test]
    fn outbound_on_send_returns_continue() {
        let otel = OtelInterceptor::new();
        let ctx = test_outbound_ctx();
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg: u64 = 99;
        let result = otel.on_send(&ctx, &rh, &mut headers, &msg);
        assert!(matches!(result, Disposition::Continue));
        assert_eq!(otel.traced_count(), 1);
    }

    #[test]
    fn outbound_on_reply_success() {
        let otel = OtelInterceptor::new();
        let ctx = test_outbound_ctx();
        let rh = RuntimeHeaders::new();
        let headers = Headers::new();
        let reply: u64 = 42;
        otel.on_reply(&ctx, &rh, &headers, &Outcome::AskSuccess { reply: &reply });
    }

    #[test]
    fn traced_count_accumulates() {
        let otel = OtelInterceptor::new();
        let ctx = test_inbound_ctx();
        let rh = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let msg: u64 = 1;
        otel.on_receive(&ctx, &rh, &mut headers, &msg);
        otel.on_receive(&ctx, &rh, &mut headers, &msg);
        otel.on_receive(&ctx, &rh, &mut headers, &msg);
        assert_eq!(otel.traced_count(), 3);
    }

    #[test]
    fn default_and_name() {
        let otel = OtelInterceptor::default();
        assert_eq!(otel.traced_count(), 0);
        assert_eq!(InboundInterceptor::name(&otel), "otel");
        assert_eq!(OutboundInterceptor::name(&otel), "otel");
    }

    #[test]
    fn outcome_status_mapping() {
        assert_eq!(OtelInterceptor::outcome_status(&Outcome::TellSuccess), "OK");
        let reply: u64 = 0;
        assert_eq!(
            OtelInterceptor::outcome_status(&Outcome::AskSuccess { reply: &reply }),
            "OK"
        );
        assert_eq!(
            OtelInterceptor::outcome_status(&Outcome::HandlerError {
                error: ActorError::internal("x")
            }),
            "ERROR"
        );
        assert_eq!(
            OtelInterceptor::outcome_status(&Outcome::StreamCompleted { items_emitted: 5 }),
            "OK"
        );
        assert_eq!(
            OtelInterceptor::outcome_status(&Outcome::StreamCancelled { items_emitted: 3 }),
            "ERROR"
        );
    }
}
