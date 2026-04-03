//! Type-erased dispatch infrastructure for delivering messages to actors.
//!
//! This module provides the core dispatch types used by all dactor runtimes
//! (TestRuntime, ractor adapter, kameo adapter) to wrap concrete messages
//! into type-erased envelopes that know how to invoke the correct handler.

use std::any::Any;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorContext, FeedHandler, Handler, StreamHandler};
use crate::errors::RuntimeError;
use crate::interceptor::{Disposition, SendMode};
use crate::message::Message;
use crate::stream::{StreamReceiver, StreamSender};

// ---------------------------------------------------------------------------
// Type-erased dispatch via async trait
// ---------------------------------------------------------------------------

/// Type-erased message envelope. Each concrete message type is wrapped in a
/// `TypedDispatch<M>`, `AskDispatch<M>`, `StreamDispatch<M>`, or
/// `FeedDispatch<M>` that knows how to invoke the appropriate handler.
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
    pub msg: M,
    pub reply_tx: tokio::sync::oneshot::Sender<Result<M::Reply, RuntimeError>>,
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
                        tracing::debug!("reply dropped — caller may have timed out or been cancelled");
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
            Disposition::Drop => {
                RuntimeError::ActorNotFound(format!("dropped by interceptor '{}'", interceptor_name))
            }
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
// StreamDispatch
// ---------------------------------------------------------------------------

/// Stream envelope: carries the message and a StreamSender for pushing items.
pub struct StreamDispatch<M: Message> {
    pub msg: M,
    pub sender: StreamSender<M::Reply>,
    pub cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, M> Dispatch<A> for StreamDispatch<M>
where
    A: StreamHandler<M>,
    M: Message,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        actor.handle_stream(self.msg, self.sender, ctx).await;
        DispatchResult::tell()
    }

    fn message_any(&self) -> &dyn Any {
        &self.msg
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Stream
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
// FeedDispatch
// ---------------------------------------------------------------------------

/// Feed envelope: carries a StreamReceiver for items and a oneshot for the reply.
pub struct FeedDispatch<Item: Send + 'static, Reply: Send + 'static> {
    pub receiver: StreamReceiver<Item>,
    pub reply_tx: tokio::sync::oneshot::Sender<Result<Reply, RuntimeError>>,
    pub cancel: Option<CancellationToken>,
}

#[async_trait]
impl<A, Item, Reply> Dispatch<A> for FeedDispatch<Item, Reply>
where
    A: FeedHandler<Item, Reply>,
    Item: Send + 'static,
    Reply: Send + 'static,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) -> DispatchResult {
        let reply = actor.handle_feed(self.receiver, ctx).await;
        let reply_any: Box<dyn Any + Send> = Box::new(reply);
        let reply_tx = self.reply_tx;
        DispatchResult {
            reply: Some(reply_any),
            reply_sender: Some(Box::new(move |boxed_reply| {
                if let Ok(reply) = boxed_reply.downcast::<Reply>() {
                    if reply_tx.send(Ok(*reply)).is_err() {
                        tracing::debug!("reply dropped — caller may have timed out or been cancelled");
                    }
                }
            })),
        }
    }

    fn message_any(&self) -> &dyn Any {
        // FeedDispatch has no message — return unit
        &()
    }

    fn send_mode(&self) -> SendMode {
        SendMode::Feed
    }

    fn message_type_name(&self) -> &'static str {
        std::any::type_name::<Item>()
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
            Disposition::Drop => {
                RuntimeError::ActorNotFound(format!("dropped by interceptor '{}'", interceptor_name))
            }
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
