//! V0.2 kameo adapter runtime for the dactor actor framework.
//!
//! Bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API with kameo's
//! single-message-type `kameo::Actor` trait using type-erased dispatch.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use dactor::actor::{
    Actor, ActorContext, ActorError, AskReply, FeedHandler, FeedMessage, Handler, StreamHandler,
    ActorRef,
};
use dactor::dispatch::{
    AskDispatch, Dispatch, FeedDispatch, StreamDispatch, TypedDispatch,
};
use dactor::errors::{ActorSendError, ErrorAction, RuntimeError};
use dactor::mailbox::MailboxConfig;
use dactor::supervision::ChildTerminated;
use dactor::interceptor::{
    Disposition, InboundContext, InboundInterceptor, OutboundContext, OutboundInterceptor, Outcome,
    SendMode,
};
use dactor::message::{Headers, Message, RuntimeHeaders};
use dactor::node::{ActorId, NodeId};
use dactor::stream::{BatchConfig, BatchReader, BatchWriter, BoxStream, StreamReceiver, StreamSender};

use crate::cluster::KameoClusterEvents;

// ---------------------------------------------------------------------------
// Watch registry
// ---------------------------------------------------------------------------

/// A type-erased entry in the watch registry.
struct WatchEntry {
    watcher_id: ActorId,
    /// Closure that delivers a [`ChildTerminated`] to the watcher actor.
    notify: Box<dyn Fn(ChildTerminated) + Send + Sync>,
}

/// Shared watch registry mapping watched actor ID → list of watcher entries.
type WatcherMap = Arc<Mutex<HashMap<ActorId, Vec<WatchEntry>>>>;

// ---------------------------------------------------------------------------
// Kameo wrapper actor
// ---------------------------------------------------------------------------

/// The kameo message type wrapping a type-erased dactor dispatch envelope.
struct DactorMsg<A: Actor>(Box<dyn Dispatch<A>>);

/// The kameo `Actor` implementation that bridges to a dactor `Actor`.
struct KameoDactorActor<A: Actor> {
    actor: A,
    ctx: ActorContext,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
    watchers: WatcherMap,
    stop_reason: Option<String>,
}

/// Arguments passed to the kameo actor at spawn time.
struct KameoSpawnArgs<A: Actor> {
    args: A::Args,
    deps: A::Deps,
    actor_id: ActorId,
    actor_name: String,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
    watchers: WatcherMap,
}

impl<A: Actor + 'static> kameo::Actor for KameoDactorActor<A> {
    type Args = KameoSpawnArgs<A>;
    type Error = kameo::error::Infallible;

    async fn on_start(
        args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        let mut actor = A::create(args.args, args.deps);
        let mut ctx = ActorContext::new(args.actor_id, args.actor_name);
        actor.on_start(&mut ctx).await;
        Ok(Self {
            actor,
            ctx,
            interceptors: args.interceptors,
            watchers: args.watchers,
            stop_reason: None,
        })
    }

    async fn on_stop(
        &mut self,
        _actor_ref: kameo::actor::WeakActorRef<Self>,
        _reason: kameo::error::ActorStopReason,
    ) -> Result<(), Self::Error> {
        // Reset context to lifecycle semantics before on_stop
        self.ctx.send_mode = None;
        self.ctx.headers = Headers::new();
        self.ctx.set_cancellation_token(None);
        self.actor.on_stop().await;

        // Notify all watchers that this actor has terminated.
        let actor_id = self.ctx.actor_id.clone();
        let actor_name = self.ctx.actor_name.clone();
        let entries = {
            let mut watchers = self.watchers.lock().unwrap();
            // Remove entries where this actor is the TARGET (watchers of this actor)
            let target_entries = watchers.remove(&actor_id).unwrap_or_default();
            // Also clean up entries where this actor is the WATCHER (prevent leak)
            for entries in watchers.values_mut() {
                entries.retain(|e| e.watcher_id != actor_id);
            }
            watchers.retain(|_, v| !v.is_empty());
            target_entries
        };
        if !entries.is_empty() {
            let notification = ChildTerminated {
                child_id: actor_id,
                child_name: actor_name,
                reason: self.stop_reason.clone(),
            };
            for entry in &entries {
                (entry.notify)(notification.clone());
            }
        }

        Ok(())
    }
}

impl<A: Actor + 'static> kameo::message::Message<DactorMsg<A>> for KameoDactorActor<A> {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: DactorMsg<A>,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let dispatch = msg.0;

        // Capture metadata before dispatch consumes the message
        let send_mode = dispatch.send_mode();
        let message_type = dispatch.message_type_name();

        self.ctx.send_mode = Some(send_mode);
        self.ctx.headers = Headers::new();

        // Run inbound interceptor pipeline
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let mut total_delay = Duration::ZERO;
        let mut rejection: Option<(String, Disposition)> = None;

        {
            let ictx = InboundContext {
                actor_id: self.ctx.actor_id.clone(),
                actor_name: &self.ctx.actor_name,
                message_type,
                send_mode,
                remote: false,
                origin_node: None,
            };

            for interceptor in &self.interceptors {
                match interceptor.on_receive(
                    &ictx,
                    &runtime_headers,
                    &mut headers,
                    dispatch.message_any(),
                ) {
                    Disposition::Continue => {}
                    Disposition::Delay(d) => {
                        total_delay += d;
                    }
                    disp @ (Disposition::Drop | Disposition::Reject(_) | Disposition::Retry(_)) => {
                        rejection = Some((interceptor.name().to_string(), disp));
                        break;
                    }
                }
            }
        }

        // If rejected/dropped/retry, propagate proper error to caller
        if let Some((interceptor_name, disposition)) = rejection {
            dispatch.reject(disposition, &interceptor_name);
            return;
        }

        if !total_delay.is_zero() {
            tokio::time::sleep(total_delay).await;
        }

        // Copy interceptor-populated headers to ActorContext
        self.ctx.headers = headers;

        // Propagate cancellation token to context
        let cancel_token = dispatch.cancel_token();
        self.ctx.set_cancellation_token(cancel_token.clone());

        // Check if already cancelled before dispatching
        if let Some(ref token) = cancel_token {
            if token.is_cancelled() {
                dispatch.cancel();
                self.ctx.set_cancellation_token(None);
                return;
            }
        }

        // Dispatch with panic catching and cancellation racing
        let result = if let Some(ref token) = cancel_token {
            let dispatch_fut = std::panic::AssertUnwindSafe(
                dispatch.dispatch(&mut self.actor, &mut self.ctx),
            )
            .catch_unwind();
            tokio::select! {
                biased;
                r = dispatch_fut => r,
                _ = token.cancelled() => {
                    // In-flight cancellation: dispatch_fut is dropped, which drops
                    // reply_tx inside it. Caller's AskReply sees channel closed.
                    // Pre-dispatch cancellation (above) sends RuntimeError::Cancelled.
                    self.ctx.set_cancellation_token(None);
                    return;
                }
            }
        } else {
            std::panic::AssertUnwindSafe(
                dispatch.dispatch(&mut self.actor, &mut self.ctx),
            )
            .catch_unwind()
            .await
        };

        self.ctx.set_cancellation_token(None);

        // Build context for on_complete
        let ictx = InboundContext {
            actor_id: self.ctx.actor_id.clone(),
            actor_name: &self.ctx.actor_name,
            message_type,
            send_mode,
            remote: false,
            origin_node: None,
        };

        match result {
            Ok(dispatch_result) => {
                let outcome = match (&dispatch_result.reply, send_mode) {
                    (Some(reply), SendMode::Ask) => Outcome::AskSuccess { reply: reply.as_ref() },
                    _ => Outcome::TellSuccess,
                };

                for interceptor in &self.interceptors {
                    interceptor.on_complete(&ictx, &runtime_headers, &self.ctx.headers, &outcome);
                }

                // Send reply to caller AFTER interceptors have seen it
                dispatch_result.send_reply();
            }
            Err(_panic) => {
                let error = ActorError::internal("handler panicked");
                let action = self.actor.on_error(&error);

                let outcome = Outcome::HandlerError { error };
                for interceptor in &self.interceptors {
                    interceptor.on_complete(&ictx, &runtime_headers, &self.ctx.headers, &outcome);
                }

                match action {
                    ErrorAction::Resume => {
                        // Actor resumes — do NOT set stop_reason (graceful lifecycle continues)
                    }
                    ErrorAction::Stop | ErrorAction::Escalate => {
                        self.stop_reason = Some("handler panicked".into());
                        // Signal kameo to stop via the context — but we don't
                        // have access to the kameo Context here since we
                        // consumed it in the signature. Instead, use kill on
                        // the actor_ref stored externally. For now, log and
                        // rely on the fact that kameo's on_panic default
                        // behavior already stops the actor on panic. We
                        // caught the panic ourselves, so we need to re-panic
                        // to trigger kameo's stop.
                        //
                        // Actually, since we caught the panic with
                        // catch_unwind, kameo won't see it. We can use the
                        // kameo context's stop() method. But we don't have
                        // the kameo message::Context here.
                        //
                        // Re-panic to let kameo's on_panic handler deal with it.
                        std::panic::resume_unwind(Box::new("dactor: actor stop requested after panic"));
                    }
                    ErrorAction::Restart => {
                        tracing::warn!("Restart not fully implemented, treating as Resume");
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// KameoActorRef — dactor ActorRef backed by kameo
// ---------------------------------------------------------------------------

/// A dactor `ActorRef` backed by a kameo `ActorRef`.
///
/// Messages are delivered through kameo's mailbox as type-erased dispatch
/// envelopes, enabling multiple `Handler<M>` impls per actor.
pub struct KameoActorRef<A: Actor> {
    id: ActorId,
    name: String,
    inner: kameo::actor::ActorRef<KameoDactorActor<A>>,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
}

impl<A: Actor> Clone for KameoActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            inner: self.inner.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
}

impl<A: Actor> std::fmt::Debug for KameoActorRef<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KameoActorRef({}, {:?})", self.name, self.id)
    }
}

impl<A: Actor + 'static> ActorRef<A> for KameoActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    fn stop(&self) {
        self.inner.kill();
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        // Run outbound interceptors
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Tell,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {} // Not supported in sync tell
                Disposition::Drop => return Ok(()),
                Disposition::Reject(_) => return Ok(()),
                Disposition::Retry(_) => return Ok(()),
            }
        }

        let dispatch: Box<dyn Dispatch<A>> = Box::new(TypedDispatch { msg });
        self.inner
            .tell(DactorMsg(dispatch))
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))
    }

    fn ask<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message,
    {
        // Run outbound interceptors
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Ask,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {} // Not supported in sync context
                Disposition::Drop => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Err(RuntimeError::ActorNotFound(
                        "message dropped by outbound interceptor".into(),
                    )));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Reject(reason) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::Rejected {
                        interceptor: name,
                        reason,
                    }));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Retry(retry_after) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::RetryAfter {
                        interceptor: name,
                        retry_after,
                    }));
                    return Ok(AskReply::new(rx));
                }
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(AskDispatch {
            msg,
            reply_tx: tx,
            cancel,
        });
        self.inner
            .tell(DactorMsg(dispatch))
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))?;
        Ok(AskReply::new(rx))
    }

    fn stream<M>(
        &self,
        msg: M,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        // Run outbound interceptors
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Stream,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    return Err(ActorSendError(
                        "stream dropped by outbound interceptor".into(),
                    ));
                }
                Disposition::Reject(reason) => {
                    return Err(ActorSendError(format!("stream rejected: {}", reason)));
                }
                Disposition::Retry(_) => {
                    return Err(ActorSendError(
                        "stream retry requested by interceptor".into(),
                    ));
                }
            }
        }

        let buffer = buffer.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(buffer);
        let sender = StreamSender::new(tx);
        let dispatch: Box<dyn Dispatch<A>> = Box::new(StreamDispatch { msg, sender, cancel });
        self.inner
            .tell(DactorMsg(dispatch))
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Wrap the stream to call on_stream_item for each reply item
        let interceptors = self.outbound_interceptors.clone();
        let target_id = self.id.clone();
        let target_name = self.name.clone();
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let intercepted = futures::StreamExt::scan(stream, 0u64, move |seq, item| {
            let octx = OutboundContext {
                target_id: target_id.clone(),
                target_name: &target_name,
                message_type: std::any::type_name::<M>(),
                send_mode: SendMode::Stream,
                remote: false,
            };
            let item_headers = Headers::new();
            for interceptor in interceptors.iter() {
                interceptor.on_stream_item(&octx, &item_headers, *seq, &item as &dyn Any);
            }
            *seq += 1;
            std::future::ready(Some(item))
        });
        Ok(Box::pin(intercepted))
    }

    fn feed<M>(
        &self,
        msg: M,
        input: BoxStream<M::Item>,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: FeedHandler<M>,
        M: FeedMessage,
    {
        // Run outbound interceptors
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Feed,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Err(RuntimeError::ActorNotFound(
                        "message dropped by outbound interceptor".into(),
                    )));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Reject(reason) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::Rejected {
                        interceptor: name,
                        reason,
                    }));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Retry(retry_after) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::RetryAfter {
                        interceptor: name,
                        retry_after,
                    }));
                    return Ok(AskReply::new(rx));
                }
            }
        }

        let buffer = buffer.max(1);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(FeedDispatch {
            msg,
            receiver,
            reply_tx,
            cancel: cancel.clone(),
        });
        self.inner
            .tell(DactorMsg(dispatch))
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Drain the input stream into the item channel with per-item interception
        let interceptors = self.outbound_interceptors.clone();
        let target_id = self.id.clone();
        let target_name = self.name.clone();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut input = input;
            let mut seq: u64 = 0;
            let item_headers = Headers::new();
            let result = std::panic::AssertUnwindSafe(async {
                if let Some(ref token) = cancel {
                    loop {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => break,
                            item = input.next() => match item {
                                Some(item) => {
                                    let octx = OutboundContext {
                                        target_id: target_id.clone(),
                                        target_name: &target_name,
                                        message_type: std::any::type_name::<M>(),
                                        send_mode: SendMode::Feed,
                                        remote: false,
                                    };
                                    for interceptor in interceptors.iter() {
                                        interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                                    }
                                    seq += 1;
                                    if item_tx.send(item).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                } else {
                    while let Some(item) = input.next().await {
                        let octx = OutboundContext {
                            target_id: target_id.clone(),
                            target_name: &target_name,
                            message_type: std::any::type_name::<M>(),
                            send_mode: SendMode::Feed,
                            remote: false,
                        };
                        for interceptor in interceptors.iter() {
                            interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                        }
                        seq += 1;
                        if item_tx.send(item).await.is_err() {
                            break;
                        }
                    }
                }
            })
            .catch_unwind()
            .await;
            if result.is_err() {
                tracing::error!("feed drain task panicked");
            }
        });

        Ok(AskReply::new(reply_rx))
    }

    fn stream_batched<M>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: BatchConfig,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        // Run outbound interceptors (same as stream())
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Stream,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    return Err(ActorSendError(
                        "stream dropped by outbound interceptor".into(),
                    ));
                }
                Disposition::Reject(reason) => {
                    return Err(ActorSendError(format!("stream rejected: {}", reason)));
                }
                Disposition::Retry(_) => {
                    return Err(ActorSendError(
                        "stream retry requested by interceptor".into(),
                    ));
                }
            }
        }

        let buffer = buffer.max(1);
        // Unbatched channel: handler pushes individual items
        let (tx, mut rx) = tokio::sync::mpsc::channel::<M::Reply>(buffer);
        let sender = StreamSender::new(tx);
        let dispatch: Box<dyn Dispatch<A>> = Box::new(StreamDispatch { msg, sender, cancel });
        self.inner
            .tell(DactorMsg(dispatch))
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Batched channel: drain task batches items for the caller
        let (batch_tx, batch_rx) = tokio::sync::mpsc::channel::<Vec<M::Reply>>(buffer);
        let reader = BatchReader::new(batch_rx);

        let batch_delay = batch_config.max_delay;
        tokio::spawn(async move {
            let mut writer = BatchWriter::new(batch_tx, batch_config);
            loop {
                if writer.buffered_count() > 0 {
                    let deadline = tokio::time::Instant::now() + batch_delay;
                    tokio::select! {
                        biased;
                        item = rx.recv() => match item {
                            Some(item) => {
                                if writer.push(item).await.is_err() { break; }
                            }
                            None => break,
                        },
                        _ = tokio::time::sleep_until(deadline) => {
                            if writer.check_deadline().await.is_err() { break; }
                        }
                    }
                } else {
                    match rx.recv().await {
                        Some(item) => {
                            if writer.push(item).await.is_err() { break; }
                        }
                        None => break,
                    }
                }
            }
            let _ = writer.flush().await;
        });

        // Wrap the output stream to call on_stream_item for each reply item
        let interceptors = self.outbound_interceptors.clone();
        let target_id = self.id.clone();
        let target_name = self.name.clone();
        let intercepted = futures::StreamExt::scan(reader.into_stream(), 0u64, move |seq, item| {
            let octx = OutboundContext {
                target_id: target_id.clone(),
                target_name: &target_name,
                message_type: std::any::type_name::<M>(),
                send_mode: SendMode::Stream,
                remote: false,
            };
            let item_headers = Headers::new();
            for interceptor in interceptors.iter() {
                interceptor.on_stream_item(&octx, &item_headers, *seq, &item as &dyn Any);
            }
            *seq += 1;
            std::future::ready(Some(item))
        });
        Ok(Box::pin(intercepted))
    }

    fn feed_batched<M>(
        &self,
        msg: M,
        input: BoxStream<M::Item>,
        buffer: usize,
        batch_config: BatchConfig,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: FeedHandler<M>,
        M: FeedMessage,
    {
        // Run outbound interceptors (same as feed())
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type: std::any::type_name::<M>(),
            send_mode: SendMode::Feed,
            remote: false,
        };

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&octx, &runtime_headers, &mut headers, &msg as &dyn Any) {
                Disposition::Continue => {}
                Disposition::Delay(_) => {}
                Disposition::Drop => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Err(RuntimeError::ActorNotFound(
                        "message dropped by outbound interceptor".into(),
                    )));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Reject(reason) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::Rejected {
                        interceptor: name,
                        reason,
                    }));
                    return Ok(AskReply::new(rx));
                }
                Disposition::Retry(retry_after) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let name = interceptor.name().to_string();
                    let _ = tx.send(Err(RuntimeError::RetryAfter {
                        interceptor: name,
                        retry_after,
                    }));
                    return Ok(AskReply::new(rx));
                }
            }
        }

        let buffer = buffer.max(1);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(FeedDispatch {
            msg,
            receiver,
            reply_tx,
            cancel: cancel.clone(),
        });
        self.inner
            .tell(DactorMsg(dispatch))
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Drain task: batch input items, then unbatch into item_tx
        let interceptors = self.outbound_interceptors.clone();
        let target_id = self.id.clone();
        let target_name = self.name.clone();
        tokio::spawn(async move {
            use futures::StreamExt;

            // Wrap input stream with per-item outbound interception
            let mut seq: u64 = 0;
            let input: BoxStream<M::Item> = Box::pin(input.map(move |item| {
                let octx = OutboundContext {
                    target_id: target_id.clone(),
                    target_name: &target_name,
                    message_type: std::any::type_name::<M>(),
                    send_mode: SendMode::Feed,
                    remote: false,
                };
                let item_headers = Headers::new();
                for interceptor in interceptors.iter() {
                    interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                }
                seq += 1;
                item
            }));

            let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<Vec<M::Item>>(buffer);

            let cancel_clone = cancel.clone();
            let writer_handle = tokio::spawn(async move {
                let mut input = input;
                let batch_delay = batch_config.max_delay;
                let mut writer = BatchWriter::new(batch_tx, batch_config);
                let result = std::panic::AssertUnwindSafe(async {
                    loop {
                        if writer.buffered_count() > 0 {
                            let deadline = tokio::time::Instant::now() + batch_delay;
                            if let Some(ref token) = cancel_clone {
                                tokio::select! {
                                    biased;
                                    _ = token.cancelled() => break,
                                    item = input.next() => match item {
                                        Some(item) => {
                                            if writer.push(item).await.is_err() { break; }
                                        }
                                        None => break,
                                    },
                                    _ = tokio::time::sleep_until(deadline) => {
                                        if writer.check_deadline().await.is_err() { break; }
                                    }
                                }
                            } else {
                                tokio::select! {
                                    biased;
                                    item = input.next() => match item {
                                        Some(item) => {
                                            if writer.push(item).await.is_err() { break; }
                                        }
                                        None => break,
                                    },
                                    _ = tokio::time::sleep_until(deadline) => {
                                        if writer.check_deadline().await.is_err() { break; }
                                    }
                                }
                            }
                        } else if let Some(ref token) = cancel_clone {
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => break,
                                item = input.next() => match item {
                                    Some(item) => {
                                        if writer.push(item).await.is_err() { break; }
                                    }
                                    None => break,
                                }
                            }
                        } else {
                            match input.next().await {
                                Some(item) => {
                                    if writer.push(item).await.is_err() { break; }
                                }
                                None => break,
                            }
                        }
                    }
                    let _ = writer.flush().await;
                })
                .catch_unwind()
                .await;

                if result.is_err() {
                    tracing::error!("feed_batched drain task panicked");
                }
            });

            // Reader side: unbatch and forward to actor
            let mut send_failed = false;
            while let Some(batch) = batch_rx.recv().await {
                for item in batch {
                    if item_tx.send(item).await.is_err() {
                        send_failed = true;
                        break;
                    }
                }
                if send_failed { break; }
            }

            let _ = writer_handle.await;
        });

        Ok(AskReply::new(reply_rx))
    }
}

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor, including the inbound interceptor pipeline.
/// Use `..Default::default()` to future-proof against new fields.
pub struct SpawnOptions {
    /// Inbound interceptors to attach to the actor.
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
    /// Mailbox capacity configuration.
    ///
    /// **Note:** The kameo adapter currently only supports unbounded mailboxes.
    /// Setting `Bounded` will log a warning and fall back to unbounded.
    pub mailbox: MailboxConfig,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::Unbounded,
        }
    }
}

// ---------------------------------------------------------------------------
// KameoRuntime
// ---------------------------------------------------------------------------

/// A dactor v0.2 runtime backed by kameo.
///
/// Actors are spawned as real kameo actors via kameo's `Spawn` trait.
/// Messages are delivered through kameo's mailbox as type-erased dispatch
/// envelopes, supporting multiple `Handler<M>` impls per actor.
///
/// Unlike the ractor adapter, kameo's `Spawn::spawn()` is synchronous
/// (it calls `tokio::spawn` internally), so `spawn()` does not require a
/// sync→async bridge thread.
pub struct KameoRuntime {
    node_id: NodeId,
    next_local: AtomicU64,
    cluster_events: KameoClusterEvents,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    watchers: WatcherMap,
}

impl KameoRuntime {
    /// Create a new `KameoRuntime`.
    pub fn new() -> Self {
        Self {
            node_id: NodeId("kameo-node".into()),
            next_local: AtomicU64::new(1),
            cluster_events: KameoClusterEvents::new(),
            outbound_interceptors: Arc::new(Vec::new()),
            watchers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Add a global outbound interceptor.
    ///
    /// **Must be called before any actors are spawned.** Panics if actors
    /// already hold references to the interceptor list (i.e., after `spawn()`).
    pub fn add_outbound_interceptor(&mut self, interceptor: Box<dyn OutboundInterceptor>) {
        Arc::get_mut(&mut self.outbound_interceptors)
            .expect("cannot add interceptors after actors are spawned")
            .push(interceptor);
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events_handle(&self) -> &KameoClusterEvents {
        &self.cluster_events
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events(&self) -> &KameoClusterEvents {
        &self.cluster_events
    }

    /// Spawn an actor with `Deps = ()`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> KameoActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_internal::<A>(name, args, (), Vec::new())
    }

    /// Spawn an actor with explicit dependencies.
    pub fn spawn_with_deps<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
    ) -> KameoActorRef<A>
    where
        A: Actor + 'static,
    {
        self.spawn_internal::<A>(name, args, deps, Vec::new())
    }

    /// Spawn an actor with spawn options (including inbound interceptors).
    pub fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> KameoActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        if !matches!(options.mailbox, MailboxConfig::Unbounded) {
            tracing::warn!("kameo adapter: bounded mailbox not yet implemented, using unbounded");
        }
        self.spawn_internal::<A>(name, args, (), options.interceptors)
    }

    fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
    ) -> KameoActorRef<A>
    where
        A: Actor + 'static,
    {
        use kameo::actor::Spawn;

        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();

        let spawn_args = KameoSpawnArgs {
            args,
            deps,
            actor_id: actor_id.clone(),
            actor_name: actor_name.clone(),
            interceptors,
            watchers: self.watchers.clone(),
        };

        // kameo's Spawn::spawn_with_mailbox is synchronous (internally calls tokio::spawn)
        let actor_ref = KameoDactorActor::<A>::spawn_with_mailbox(
            spawn_args,
            kameo::mailbox::unbounded(),
        );

        KameoActorRef {
            id: actor_id,
            name: actor_name,
            inner: actor_ref,
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
    /// Register actor `watcher` to be notified when `target_id` terminates.
    ///
    /// The watcher must implement `Handler<ChildTerminated>`. When the target
    /// stops, the runtime delivers a [`ChildTerminated`] message to the watcher.
    pub fn watch<W>(&self, watcher: &KameoActorRef<W>, target_id: ActorId)
    where
        W: Actor + Handler<ChildTerminated> + 'static,
    {
        let watcher_id = watcher.id();
        let watcher_inner = watcher.inner.clone();

        let entry = WatchEntry {
            watcher_id,
            notify: Box::new(move |msg: ChildTerminated| {
                let dispatch: Box<dyn Dispatch<W>> = Box::new(TypedDispatch { msg });
                if watcher_inner.tell(DactorMsg(dispatch)).try_send().is_err() {
                    tracing::debug!("watch notification dropped — watcher may have stopped");
                }
            }),
        };

        let mut watchers = self.watchers.lock().unwrap();
        watchers.entry(target_id).or_default().push(entry);
    }

    /// Unregister `watcher_id` from notifications about `target_id`.
    pub fn unwatch(&self, watcher_id: &ActorId, target_id: &ActorId) {
        let mut watchers = self.watchers.lock().unwrap();
        if let Some(entries) = watchers.get_mut(target_id) {
            entries.retain(|e| &e.watcher_id != watcher_id);
            if entries.is_empty() {
                watchers.remove(target_id);
            }
        }
    }
}

impl Default for KameoRuntime {
    fn default() -> Self {
        Self::new()
    }
}
