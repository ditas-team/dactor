//! Type-erased dispatch infrastructure for delivering messages to actors.
//!
//! This module provides the core dispatch types used by all dactor runtimes
//! (TestRuntime, ractor adapter, kameo adapter) to wrap concrete messages
//! into type-erased envelopes that know how to invoke the correct handler.

use std::any::Any;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorContext, ReduceHandler, Handler, ExpandHandler, TransformHandler};
use crate::errors::RuntimeError;
use crate::interceptor::{Disposition, SendMode};
use crate::message::Message;
use crate::stream::{StreamReceiver, StreamSender};

// ---------------------------------------------------------------------------
// Type-erased dispatch via async trait
// ---------------------------------------------------------------------------

/// Type-erased message envelope. Each concrete message type is wrapped in a
/// `TypedDispatch<M>`, `AskDispatch<M>`, `ExpandDispatch<M>`,
/// `ReduceDispatch<M>`, or `TransformDispatch<M>` that knows how to invoke
/// the appropriate handler.
#[async_trait]
pub trait Dispatch<A: Actor>: Send {
    /// Dispatch the message to the actor's handler.
    /// Returns the type-erased reply for ask (Some), or None for tell.
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult;

    /// The message as `&dyn Any` for interceptor inspection.
    fn message_any(&self) -> &dyn Any;

    /// The send mode (Tell, Ask, Stream, or Feed).
    fn send_mode(&self) -> SendMode;

    /// The message type name.
    fn message_type_name(&self) -> &'static str;

    /// Reject this dispatch — for ask/feed, sends the proper error to the caller.
    /// For tell/stream, silently drops.
    fn reject(self: Box<Self>, disposition: Disposition, interceptor_name: &str);

    /// Cancel this dispatch — sends RuntimeError::Cancelled to the caller.
    /// For tell/stream, silently drops.
    fn cancel(self: Box<Self>);

    /// The cancellation token for this dispatch (None for tell).
    fn cancel_token(&self) -> Option<CancellationToken>;
}

/// Convenience alias for a boxed dispatch envelope.
pub type BoxedDispatch<A> = Box<dyn Dispatch<A>>;

/// Type alias for the reply sender closure in DispatchResult.
pub type ReplySender = Box<dyn FnOnce(Box<dyn Any + Send>) + Send>;

/// Result of dispatching a message, including the type-erased reply for ask.
pub struct DispatchResult {
    /// The type-erased reply value (Some for ask, None for tell).
    pub reply: Option<Box<dyn Any + Send>>,
    /// Oneshot sender to deliver the reply to the caller (Some for ask).
    pub reply_sender: Option<ReplySender>,
}

impl DispatchResult {
    /// Create a result for tell (no reply).
    pub fn tell() -> Self {
        Self {
            reply: None,
            reply_sender: None,
        }
    }

    /// Send the reply to the caller (for ask). Must be called after interceptors inspect it.
    pub fn send_reply(self) {
        if let (Some(reply), Some(sender)) = (self.reply, self.reply_sender) {
            sender(reply);
        }
    }
}

// ---------------------------------------------------------------------------
// TypedDispatch (tell)
// ---------------------------------------------------------------------------

/// Tell envelope: carries the message for fire-and-forget delivery.
pub struct TypedDispatch<M: Message> {
    /// The message payload.
    pub msg: M,
}

#[async_trait]
impl<A, M> Dispatch<A> for TypedDispatch<M>
where
    A: Handler<M>,
    M: Message<Reply = ()>,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        actor.handle(self.msg, ctx).await;
        DispatchResult::tell()
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Tell
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }

    fn reject(self: Box<Self>, _: Disposition, _: &str) {}

    fn cancel(self: Box<Self>) {}

    fn cancel_token(&self) -> Option<CancellationToken> {
        None
    }
}

// ---------------------------------------------------------------------------
// AskDispatch
// ---------------------------------------------------------------------------

/// Ask envelope: carries the message and a oneshot sender for the reply.
pub struct AskDispatch<M: Message> {
    /// The message payload.
    pub msg: M,
    /// Channel to send the reply back to the caller.
    pub reply_tx: tokio::sync::oneshot::Sender<Result<M::Reply, RuntimeError>>,
    /// Optional cancellation token for the ask operation.
    pub cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, M> Dispatch<A> for AskDispatch<M>
where
    A: Handler<M>,
    M: Message,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        let reply = actor.handle(self.msg, ctx).await;
        let reply_any: Box<dyn Any + Send> = Box::new(reply);
        let reply_tx = self.reply_tx;
        DispatchResult {
            reply: Some(reply_any),
            reply_sender: Some(Box::new(move |boxed_reply| {
                if let Ok(reply) = boxed_reply.downcast::<M::Reply>() {
                    if reply_tx.send(Ok(*reply)).is_err() {
                        tracing::debug!(
                            "reply dropped — caller may have timed out or been cancelled"
                        );
                    }
                }
            })),
        }
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Ask
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }

    fn reject(self: Box<Self>, disposition: Disposition, interceptor_name: &str) {
        let error = match disposition {
            Disposition::Reject(reason) => RuntimeError::Rejected {
                interceptor: interceptor_name.to_string(),
                reason,
            },
            Disposition::Retry(retry_after) => RuntimeError::RetryAfter {
                interceptor: interceptor_name.to_string(),
                retry_after,
            },
            Disposition::Drop => RuntimeError::ActorNotFound(format!(
                "dropped by interceptor '{}'",
                interceptor_name
            )),
            _ => return,
        };
        let _ = self.reply_tx.send(Err(error));
    }

    fn cancel(self: Box<Self>) {
        let _ = self.reply_tx.send(Err(RuntimeError::Cancelled));
    }

    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}

// ---------------------------------------------------------------------------
// ExpandDispatch
// ---------------------------------------------------------------------------

/// Stream envelope: carries the message and a StreamSender for pushing items.
pub struct ExpandDispatch<M: Send + 'static, OutputItem: Send + 'static> {
    /// The message payload.
    pub msg: M,
    /// Sender for pushing stream reply items.
    pub sender: StreamSender<OutputItem>,
    /// Optional cancellation token for the stream.
    pub cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, M, OutputItem> Dispatch<A> for ExpandDispatch<M, OutputItem>
where
    A: ExpandHandler<M, OutputItem>,
    M: Send + 'static,
    OutputItem: Send + 'static,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        actor.handle_expand(self.msg, self.sender, ctx).await;
        DispatchResult::tell()
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Expand
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }

    fn reject(self: Box<Self>, _: Disposition, _: &str) {}

    fn cancel(self: Box<Self>) {}

    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}

// ---------------------------------------------------------------------------
// ReduceDispatch
// ---------------------------------------------------------------------------

/// Feed envelope: carries a StreamReceiver for items and a oneshot for the reply.
pub struct ReduceDispatch<InputItem: Send + 'static, Reply: Send + 'static> {
    /// Receiver for incoming stream items.
    pub receiver: StreamReceiver<InputItem>,
    /// Channel to send the final reply back to the caller.
    pub reply_tx: tokio::sync::oneshot::Sender<Result<Reply, RuntimeError>>,
    /// Optional cancellation token for the feed operation.
    pub cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, InputItem, Reply> Dispatch<A> for ReduceDispatch<InputItem, Reply>
where
    A: ReduceHandler<InputItem, Reply>,
    InputItem: Send + 'static,
    Reply: Send + 'static,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        let reply = actor.handle_reduce(self.receiver, ctx).await;
        let reply_any: Box<dyn Any + Send> = Box::new(reply);
        let reply_tx = self.reply_tx;
        DispatchResult {
            reply: Some(reply_any),
            reply_sender: Some(Box::new(move |boxed_reply| {
                if let Ok(reply) = boxed_reply.downcast::<Reply>() {
                    if reply_tx.send(Ok(*reply)).is_err() {
                        tracing::debug!(
                            "reply dropped — caller may have timed out or been cancelled"
                        );
                    }
                }
            })),
        }
    }

    fn message_any(&self) -> &dyn Any {
        // ReduceDispatch has no message — return unit
        &()
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Reduce
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<InputItem>()
    }

    fn reject(self: Box<Self>, disposition: Disposition, interceptor_name: &str) {
        let error = match disposition {
            Disposition::Reject(reason) => RuntimeError::Rejected {
                interceptor: interceptor_name.to_string(),
                reason,
            },
            Disposition::Retry(retry_after) => RuntimeError::RetryAfter {
                interceptor: interceptor_name.to_string(),
                retry_after,
            },
            Disposition::Drop => RuntimeError::ActorNotFound(format!(
                "dropped by interceptor '{}'",
                interceptor_name
            )),
            _ => return,
        };
        let _ = self.reply_tx.send(Err(error));
    }

    fn cancel(self: Box<Self>) {
        let _ = self.reply_tx.send(Err(RuntimeError::Cancelled));
    }

    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}

// ---------------------------------------------------------------------------
// TransformDispatch
// ---------------------------------------------------------------------------

/// Transform envelope: carries a StreamReceiver for input items and a
/// StreamSender for output items. The actor consumes input, produces output.
pub struct TransformDispatch<A, InputItem, OutputItem>
where
    A: TransformHandler<InputItem, OutputItem>,
    InputItem: Send + 'static,
    OutputItem: Send + 'static,
{
    /// Receiver for incoming stream items.
    pub receiver: StreamReceiver<InputItem>,
    /// Sender for pushing output items.
    pub sender: StreamSender<OutputItem>,
    /// Optional cancellation token.
    pub cancel: Option<CancellationToken>,
    /// Phantom data for the actor type.
    _phantom: std::marker::PhantomData<fn() -> A>,
}

impl<A, InputItem, OutputItem> TransformDispatch<A, InputItem, OutputItem>
where
    A: TransformHandler<InputItem, OutputItem>,
    InputItem: Send + 'static,
    OutputItem: Send + 'static,
{
    /// Create a new TransformDispatch.
    pub fn new(
        receiver: StreamReceiver<InputItem>,
        sender: StreamSender<OutputItem>,
        cancel: Option<CancellationToken>,
    ) -> Self {
        Self {
            receiver,
            sender,
            cancel,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<A, InputItem, OutputItem> Dispatch<A> for TransformDispatch<A, InputItem, OutputItem>
where
    A: TransformHandler<InputItem, OutputItem>,
    InputItem: Send + 'static,
    OutputItem: Send + 'static,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        let mut receiver = self.receiver;
        let sender = self.sender;
        let cancel = self.cancel;
        let mut cancelled = false;

        while let Some(item) = receiver.recv().await {
            // Check cancellation before processing each item
            if let Some(ref token) = cancel {
                if token.is_cancelled() {
                    cancelled = true;
                    break;
                }
            }
            actor.handle_transform(item, &sender, ctx).await;
        }

        // Only call on_transform_complete if the stream ended normally (not cancelled)
        if !cancelled {
            actor.on_transform_complete(&sender, ctx).await;
        }

        DispatchResult::tell()
    }

    fn message_any(&self) -> &dyn Any {
        &()
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Transform
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<InputItem>()
    }

    fn reject(self: Box<Self>, _: Disposition, _: &str) {}

    fn cancel(self: Box<Self>) {}

    fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel.clone()
    }
}
