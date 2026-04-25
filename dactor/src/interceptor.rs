//! Inbound interceptor pipeline.
//!
//! Interceptors form an ordered pipeline executed before the actor's handler.
//! Each interceptor can inspect, modify headers, delay, drop, or reject a message
//! before it reaches the handler. After handler completion, interceptors are
//! notified via `on_complete`.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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
    Expand,
    /// Batch feed: deliver pre-collected items without per-item replies.
    Reduce,
    /// Transform: actor consumes input stream and produces output stream.
    Transform,
}

/// Metadata about an inbound message and its target actor.
///
/// Provided to [`InboundInterceptor::on_receive`] and
/// [`InboundInterceptor::on_complete`] so interceptors can make
/// context-aware decisions (e.g., rate-limit only remote messages).
pub struct InboundContext<'a> {
    /// The target actor's identity.
    pub actor_id: ActorId,
    /// The target actor's name.
    pub actor_name: &'a str,
    /// The Rust type name of the message.
    pub message_type: &'static str,
    /// How the message was sent (Tell, Ask, Expand, Reduce, Transform).
    pub send_mode: SendMode,
    /// Whether the message originated from a remote node.
    pub remote: bool,
    /// The originating node, if the message is remote.
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

/// Result of running an interceptor pipeline. Pairs the final [`Disposition`]
/// with the name of the interceptor that produced it (empty for `Continue`).
///
/// Used by runtimes to log/route dropped messages to the dead letter handler
/// with the interceptor's identity.
pub struct InterceptResult {
    /// The final disposition decision.
    pub disposition: Disposition,
    /// Name of the interceptor that produced a non-Continue disposition.
    /// Empty string if all interceptors returned Continue.
    pub interceptor_name: &'static str,
}

impl InterceptResult {
    /// All interceptors returned Continue.
    pub fn continued() -> Self {
        Self {
            disposition: Disposition::Continue,
            interceptor_name: "",
        }
    }

    /// Whether the disposition is `Continue`.
    pub fn is_continue(&self) -> bool {
        matches!(self.disposition, Disposition::Continue)
    }
}

/// Information about a dropped message or stream item.
/// Passed to [`DropObserver::on_drop`] so the runtime never silently
/// discards items.
#[derive(Debug)]
pub struct DropNotice {
    /// The target actor's name.
    pub target_name: String,
    /// The Rust type name of the message or stream item.
    pub message_type: &'static str,
    /// The interceptor that returned `Disposition::Drop`.
    pub interceptor_name: &'static str,
    /// How the message was sent (Tell, Ask, Expand, Reduce, Transform).
    pub send_mode: SendMode,
    /// Context string describing where the drop happened
    /// (e.g., "outbound on_send", "stream reply", "feed item", "transform input item").
    pub context: &'static str,
    /// For stream/feed items, the zero-based sequence number.
    /// `None` for message-level drops (tell/ask/stream initial request).
    pub seq: Option<u64>,
}

/// Global observer for interceptor-driven drops.
///
/// Register on the runtime to observe all messages and stream items
/// that interceptors drop via `Disposition::Drop`. Without an observer,
/// drops are silent — the application is responsible for registering
/// one if it wants visibility.
///
/// Use cases: metrics (count dropped items), alerting, dead-letter routing,
/// audit logging.
pub trait DropObserver: Send + Sync + 'static {
    /// Called whenever an interceptor returns `Disposition::Drop`.
    fn on_drop(&self, notice: DropNotice);
}

/// Notify the drop observer when an interceptor drops an item.
/// If no observer is registered, the drop is silent — the application
/// is responsible for registering an observer if it wants visibility.
pub fn notify_drop(observer: &Option<Arc<dyn DropObserver>>, notice: DropNotice) {
    if let Some(obs) = observer {
        obs.on_drop(notice);
    }
}

/// Run outbound interceptors' `on_expand_item` for a single item.
/// Returns the first non-Continue disposition with the interceptor name.
pub fn intercept_outbound_stream_item(
    interceptors: &[Box<dyn OutboundInterceptor>],
    ctx: &OutboundContext<'_>,
    headers: &Headers,
    seq: u64,
    item: &dyn Any,
) -> InterceptResult {
    for interceptor in interceptors {
        let d = interceptor.on_expand_item(ctx, headers, seq, item);
        if !matches!(d, Disposition::Continue) {
            return InterceptResult {
                disposition: d,
                interceptor_name: interceptor.name(),
            };
        }
    }
    InterceptResult::continued()
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
    AskSuccess {
        /// Type-erased reply value.
        reply: &'a dyn Any,
    },
    /// Handler returned an error or panicked.
    HandlerError {
        /// The error from the handler.
        error: ActorError,
    },
    /// Stream completed normally (future use).
    StreamCompleted {
        /// Number of items emitted before completion.
        items_emitted: u64,
    },
    /// Stream was cancelled (future use).
    StreamCancelled {
        /// Number of items emitted before cancellation.
        items_emitted: u64,
    },
}

impl<'a> std::fmt::Debug for Outcome<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TellSuccess => write!(f, "TellSuccess"),
            Self::AskSuccess { .. } => write!(f, "AskSuccess"),
            Self::HandlerError { error } => write!(f, "HandlerError({:?})", error),
            Self::StreamCompleted { items_emitted } => {
                write!(f, "StreamCompleted({})", items_emitted)
            }
            Self::StreamCancelled { items_emitted } => {
                write!(f, "StreamCancelled({})", items_emitted)
            }
        }
    }
}

/// A boxed closure that wraps a handler future with async context.
///
/// Created by [`InboundInterceptor::wrap_handler`] and applied by the runtime
/// to nest interceptor scopes around the handler dispatch. The closure receives
/// the inner future (handler or next wrapper) and returns a new future that
/// provides the desired async context.
///
/// # Example
///
/// ```rust,ignore
/// tokio::task_local! { static MY_CTX: String; }
///
/// fn wrap_handler<'a>(&'a self, _ctx: &InboundContext, headers: &Headers)
///     -> Option<HandlerWrapper<'a>>
/// {
///     let value = headers.get::<MyHeader>()?.value().clone();
///     Some(Box::new(move |next| {
///         Box::pin(MY_CTX.scope(value, next))
///     }))
/// }
/// ```
pub type HandlerWrapper<'a> = Box<
    dyn FnOnce(Pin<Box<dyn Future<Output = ()> + Send + 'a>>)
            -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + 'a,
>;

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
    /// Returns [`Disposition`] to control per-item flow:
    /// - `Continue` — deliver/forward the item normally.
    /// - `Drop` — silently skip this item.
    /// - `Delay(d)` — pause for `d` before forwarding (backpressure).
    /// - `Reject(reason)` — terminate the stream with an error.
    /// - `Retry(d)` — not meaningful for stream items; treated as `Drop`.
    fn on_expand_item(
        &self,
        ctx: &InboundContext<'_>,
        headers: &Headers,
        seq: u64,
        item: &dyn Any,
    ) -> Disposition {
        let _ = (ctx, headers, seq, item);
        Disposition::Continue
    }

    /// Creates a wrapper for the handler dispatch future, enabling async
    /// context that persists through handler execution.
    ///
    /// Called after [`on_receive`](InboundInterceptor::on_receive) succeeds
    /// (returns `Continue` or `Delay`) and before the handler dispatches.
    /// The runtime collects wrappers from all interceptors in pipeline order,
    /// then applies them so the **first** interceptor's wrapper is the
    /// **outermost** scope.
    ///
    /// Return `None` (default) for no wrapping — zero overhead on the fast
    /// path. Return `Some(wrapper)` to wrap the handler future with async
    /// context such as `tokio::task_local!` scopes or `tracing::Instrument`.
    ///
    /// The wrapper closure **must** await the `next` future it receives;
    /// failing to do so will prevent the handler from executing.
    ///
    /// # Data flow
    ///
    /// Extract what you need from `headers` here and capture it in the
    /// returned closure. This is message-local — no stale-state risk even
    /// if the message is later rejected or the actor processes another message.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// tokio::task_local! {
    ///     static TRACE_CTX: TraceContext;
    /// }
    ///
    /// impl InboundInterceptor for ContextRestorer {
    ///     fn name(&self) -> &'static str { "context-restorer" }
    ///
    ///     fn wrap_handler<'a>(
    ///         &'a self,
    ///         _ctx: &InboundContext<'_>,
    ///         headers: &Headers,
    ///     ) -> Option<HandlerWrapper<'a>> {
    ///         let tc = headers.get::<TraceContext>()?.clone();
    ///         Some(Box::new(move |next| {
    ///             Box::pin(TRACE_CTX.scope(tc, next))
    ///         }))
    ///     }
    /// }
    /// ```
    fn wrap_handler<'a>(
        &'a self,
        ctx: &InboundContext<'_>,
        headers: &Headers,
    ) -> Option<HandlerWrapper<'a>> {
        let _ = (ctx, headers);
        None
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
    ///   to be sent to the target actor.
    ///
    /// `seq` is a zero-based sequence number within this stream/feed.
    /// The item is type-erased; downcast if you know the concrete type.
    ///
    /// Returns [`Disposition`] to control per-item flow:
    /// - `Continue` — deliver/forward the item normally.
    /// - `Drop` — silently skip this item.
    /// - `Delay(d)` — pause for `d` before forwarding (backpressure).
    /// - `Reject(reason)` — terminate the stream with an error.
    /// - `Retry(d)` — not meaningful for stream items; treated as `Drop`.
    fn on_expand_item(
        &self,
        ctx: &OutboundContext<'_>,
        headers: &Headers,
        seq: u64,
        item: &dyn Any,
    ) -> Disposition {
        let _ = (ctx, headers, seq, item);
        Disposition::Continue
    }
}

/// Collect handler wrappers from interceptors.
///
/// Called after the `on_receive` pipeline succeeds. Each interceptor gets
/// a chance to return a [`HandlerWrapper`] that will nest around the handler
/// dispatch future. Returns a `Vec` aligned with the interceptor slice — one
/// `Option<HandlerWrapper>` per interceptor.
pub fn collect_handler_wrappers<'a>(
    interceptors: &'a [Box<dyn InboundInterceptor>],
    ctx: &InboundContext<'_>,
    headers: &Headers,
) -> Vec<Option<HandlerWrapper<'a>>> {
    interceptors
        .iter()
        .map(|i| i.wrap_handler(ctx, headers))
        .collect()
}

/// Apply collected handler wrappers around an inner future.
///
/// Wrappers are applied in **reverse** order so that interceptor\[0\]'s
/// wrapper is the outermost scope — matching the `on_receive` execution order.
///
/// Interceptors that returned `None` are skipped (zero cost).
pub fn apply_handler_wrappers<'a>(
    wrappers: Vec<Option<HandlerWrapper<'a>>>,
    inner: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    wrappers
        .into_iter()
        .rev()
        .fold(inner, |next, wrapper| match wrapper {
            Some(w) => w(next),
            None => next,
        })
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
        assert_ne!(SendMode::Expand, SendMode::Reduce);
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

    #[test]
    fn test_wrap_handler_default_returns_none() {
        let interceptor = NoopInterceptor;
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
        let headers = Headers::new();
        assert!(interceptor.wrap_handler(&ctx, &headers).is_none());
    }

    struct WrappingInterceptor;

    impl InboundInterceptor for WrappingInterceptor {
        fn name(&self) -> &'static str {
            "wrapping"
        }

        fn wrap_handler<'a>(
            &'a self,
            _ctx: &InboundContext<'_>,
            _headers: &Headers,
        ) -> Option<HandlerWrapper<'a>> {
            Some(Box::new(|next| {
                Box::pin(async move {
                    // Just wraps without modification
                    next.await;
                })
            }))
        }
    }

    #[test]
    fn test_wrap_handler_returns_some() {
        let interceptor = WrappingInterceptor;
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
        let headers = Headers::new();
        assert!(interceptor.wrap_handler(&ctx, &headers).is_some());
    }

    #[test]
    fn test_collect_handler_wrappers_mixed() {
        let interceptors: Vec<Box<dyn InboundInterceptor>> = vec![
            Box::new(NoopInterceptor),
            Box::new(WrappingInterceptor),
            Box::new(NoopInterceptor),
        ];
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
        let headers = Headers::new();
        let wrappers = collect_handler_wrappers(&interceptors, &ctx, &headers);
        assert_eq!(wrappers.len(), 3);
        assert!(wrappers[0].is_none());
        assert!(wrappers[1].is_some());
        assert!(wrappers[2].is_none());
    }

    #[tokio::test]
    async fn test_apply_handler_wrappers_executes_inner() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();

        let inner: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            executed_clone.store(true, Ordering::SeqCst);
        });

        // No wrappers — inner should still execute
        let wrapped = apply_handler_wrappers(vec![], inner);
        wrapped.await;
        assert!(executed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_apply_handler_wrappers_nesting_order() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let order = Arc::new(AtomicU32::new(0));

        // Build wrappers that record entry order
        let order1 = order.clone();
        let order2 = order.clone();
        let order_inner = order.clone();

        let w1: HandlerWrapper<'_> = Box::new(move |next| {
            Box::pin(async move {
                // First interceptor enters first (outermost)
                let val = order1.fetch_add(1, Ordering::SeqCst);
                assert_eq!(val, 0, "wrapper 1 should enter first");
                next.await;
            })
        });

        let w2: HandlerWrapper<'_> = Box::new(move |next| {
            Box::pin(async move {
                // Second interceptor enters second (inner)
                let val = order2.fetch_add(1, Ordering::SeqCst);
                assert_eq!(val, 1, "wrapper 2 should enter second");
                next.await;
            })
        });

        let inner: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            let val = order_inner.fetch_add(1, Ordering::SeqCst);
            assert_eq!(val, 2, "inner should execute last");
        });

        let wrappers = vec![Some(w1), Some(w2)];
        let wrapped = apply_handler_wrappers(wrappers, inner);
        wrapped.await;

        assert_eq!(order.load(Ordering::SeqCst), 3);
    }
}
