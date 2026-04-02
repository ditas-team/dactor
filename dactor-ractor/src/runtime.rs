//! V0.2 ractor adapter runtime for the dactor actor framework.
//!
//! Bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API with ractor's
//! single-message-type `ractor::Actor` trait using type-erased dispatch.

use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
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

use crate::cluster::RactorClusterEvents;

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
// Ractor wrapper actor
// ---------------------------------------------------------------------------

/// The ractor message type wrapping a type-erased dactor dispatch envelope.
struct DactorMsg<A: Actor>(Box<dyn Dispatch<A>>);

/// The ractor `Actor` implementation that bridges to a dactor `Actor`.
struct RactorDactorActor<A: Actor> {
    _phantom: PhantomData<fn() -> A>,
}

/// State held by the ractor actor, containing the dactor `Actor` instance.
struct RactorActorState<A: Actor> {
    actor: A,
    ctx: ActorContext,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
    watchers: WatcherMap,
    stop_reason: Option<String>,
}

/// Arguments passed to the ractor actor at spawn time.
struct RactorSpawnArgs<A: Actor> {
    args: A::Args,
    deps: A::Deps,
    actor_id: ActorId,
    actor_name: String,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
    watchers: WatcherMap,
}

impl<A: Actor + 'static> ractor::Actor for RactorDactorActor<A> {
    type Msg = DactorMsg<A>;
    type State = RactorActorState<A>;
    type Arguments = RactorSpawnArgs<A>;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        let mut actor = A::create(args.args, args.deps);
        let mut ctx = ActorContext::new(args.actor_id, args.actor_name);
        actor.on_start(&mut ctx).await;
        Ok(RactorActorState {
            actor,
            ctx,
            interceptors: args.interceptors,
            watchers: args.watchers,
            stop_reason: None,
        })
    }

    async fn handle(
        &self,
        myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        let dispatch = message.0;

        // Capture metadata before dispatch consumes the message
        let send_mode = dispatch.send_mode();
        let message_type = dispatch.message_type_name();

        state.ctx.send_mode = Some(send_mode);
        state.ctx.headers = Headers::new();

        // Run inbound interceptor pipeline
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let mut total_delay = Duration::ZERO;
        let mut rejection: Option<(String, Disposition)> = None;

        {
            let ictx = InboundContext {
                actor_id: state.ctx.actor_id.clone(),
                actor_name: &state.ctx.actor_name,
                message_type,
                send_mode,
                remote: false,
                origin_node: None,
            };

            for interceptor in &state.interceptors {
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
            return Ok(());
        }

        if !total_delay.is_zero() {
            tokio::time::sleep(total_delay).await;
        }

        // Copy interceptor-populated headers to ActorContext so handler can access them
        state.ctx.headers = headers;

        // Propagate cancellation token to context
        let cancel_token = dispatch.cancel_token();
        state.ctx.set_cancellation_token(cancel_token.clone());

        // Check if already cancelled before dispatching
        if let Some(ref token) = cancel_token {
            if token.is_cancelled() {
                dispatch.cancel();
                state.ctx.set_cancellation_token(None);
                return Ok(());
            }
        }

        // Dispatch with panic catching and cancellation racing
        let result = if let Some(ref token) = cancel_token {
            let dispatch_fut = std::panic::AssertUnwindSafe(
                dispatch.dispatch(&mut state.actor, &mut state.ctx),
            )
            .catch_unwind();
            tokio::select! {
                biased;
                r = dispatch_fut => r,
                _ = token.cancelled() => {
                    // In-flight cancellation: dispatch_fut is dropped, which drops
                    // reply_tx inside it. Caller's AskReply sees channel closed.
                    // Pre-dispatch cancellation (above) sends RuntimeError::Cancelled.
                    state.ctx.set_cancellation_token(None);
                    return Ok(());
                }
            }
        } else {
            std::panic::AssertUnwindSafe(
                dispatch.dispatch(&mut state.actor, &mut state.ctx),
            )
            .catch_unwind()
            .await
        };

        state.ctx.set_cancellation_token(None);

        // Build context for on_complete
        let ictx = InboundContext {
            actor_id: state.ctx.actor_id.clone(),
            actor_name: &state.ctx.actor_name,
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

                for interceptor in &state.interceptors {
                    interceptor.on_complete(&ictx, &runtime_headers, &state.ctx.headers, &outcome);
                }

                // Send reply to caller AFTER interceptors have seen it
                dispatch_result.send_reply();
            }
            Err(_panic) => {
                let error = ActorError::internal("handler panicked");
                let action = state.actor.on_error(&error);

                let outcome = Outcome::HandlerError { error };
                for interceptor in &state.interceptors {
                    interceptor.on_complete(&ictx, &runtime_headers, &state.ctx.headers, &outcome);
                }

                match action {
                    ErrorAction::Resume => {
                        // Actor resumes — do NOT set stop_reason
                    }
                    ErrorAction::Stop | ErrorAction::Escalate => {
                        state.stop_reason = Some("handler panicked".into());
                        myself.stop(None);
                    }
                    ErrorAction::Restart => {
                        tracing::warn!("Restart not fully implemented, treating as Resume");
                    }
                }
            }
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        // Reset context to lifecycle semantics before on_stop
        state.ctx.send_mode = None;
        state.ctx.headers = Headers::new();
        state.ctx.set_cancellation_token(None);
        state.actor.on_stop().await;

        // Notify all watchers that this actor has terminated.
        let actor_id = state.ctx.actor_id.clone();
        let actor_name = state.ctx.actor_name.clone();
        let entries = {
            let mut watchers = state.watchers.lock().unwrap();
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
                reason: state.stop_reason.clone(),
            };
            for entry in &entries {
                (entry.notify)(notification.clone());
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RactorActorRef — dactor ActorRef backed by ractor
// ---------------------------------------------------------------------------

/// A dactor `ActorRef` backed by a ractor `ActorRef`.
///
/// Messages are delivered through ractor's mailbox as type-erased dispatch
/// envelopes, enabling multiple `Handler<M>` impls per actor.
pub struct RactorActorRef<A: Actor> {
    id: ActorId,
    name: String,
    inner: ractor::ActorRef<DactorMsg<A>>,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
}

impl<A: Actor> Clone for RactorActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            inner: self.inner.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
        }
    }
}

impl<A: Actor> std::fmt::Debug for RactorActorRef<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RactorActorRef({}, {:?})", self.name, self.id)
    }
}

impl<A: Actor + 'static> ActorRef<A> for RactorActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        matches!(
            self.inner.get_status(),
            ractor::ActorStatus::Running
                | ractor::ActorStatus::Starting
                | ractor::ActorStatus::Upgrading
        )
    }

    fn stop(&self) {
        self.inner.stop(None);
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
            .cast(DactorMsg(dispatch))
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
            .cast(DactorMsg(dispatch))
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
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Wrap the stream to call on_stream_item for each reply item
        let interceptors = self.outbound_interceptors.clone();
        let target_id = self.id.clone();
        let target_name = self.name.clone();
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<M::Reply>(buffer);
        tokio::spawn(async move {
            use tokio_stream::StreamExt;
            let mut stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            let mut seq: u64 = 0;
            while let Some(item) = stream.next().await {
                let octx = OutboundContext {
                    target_id: target_id.clone(),
                    target_name: &target_name,
                    message_type: std::any::type_name::<M>(),
                    send_mode: SendMode::Stream,
                    remote: false,
                };
                let item_headers = Headers::new();
                let mut disposition = Disposition::Continue;
                for interceptor in interceptors.iter() {
                    disposition = interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                    if !matches!(disposition, Disposition::Continue) { break; }
                }
                seq += 1;
                match disposition {
                    Disposition::Continue => {
                        if out_tx.send(item).await.is_err() { break; }
                    }
                    Disposition::Drop => continue,
                    Disposition::Delay(d) => {
                        tokio::time::sleep(d).await;
                        if out_tx.send(item).await.is_err() { break; }
                    }
                    Disposition::Reject(_) | Disposition::Retry(_) => break,
                }
            }
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(out_rx)))
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
        // Note: no on_send() for FeedMessage — it's a type tag with no data.
        // Per-item interception happens via on_stream_item() in the drain task.

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
            .cast(DactorMsg(dispatch))
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
                loop {
                    let next_item = if let Some(ref token) = cancel {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => break,
                            item = input.next() => item,
                        }
                    } else {
                        input.next().await
                    };
                    match next_item {
                        Some(item) => {
                            let octx = OutboundContext {
                                target_id: target_id.clone(),
                                target_name: &target_name,
                                message_type: std::any::type_name::<M>(),
                                send_mode: SendMode::Feed,
                                remote: false,
                            };
                            let mut disposition = Disposition::Continue;
                            for interceptor in interceptors.iter() {
                                disposition = interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                                if !matches!(disposition, Disposition::Continue) { break; }
                            }
                            seq += 1;
                            match disposition {
                                Disposition::Continue => {
                                    if item_tx.send(item).await.is_err() { break; }
                                }
                                Disposition::Drop => continue,
                                Disposition::Delay(d) => {
                                    tokio::time::sleep(d).await;
                                    if item_tx.send(item).await.is_err() { break; }
                                }
                                Disposition::Reject(_) | Disposition::Retry(_) => break,
                            }
                        }
                        None => break,
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
            .cast(DactorMsg(dispatch))
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
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<M::Reply>(buffer);
        tokio::spawn(async move {
            use tokio_stream::StreamExt;
            let mut stream = reader.into_stream();
            let mut seq: u64 = 0;
            while let Some(item) = stream.next().await {
                let octx = OutboundContext {
                    target_id: target_id.clone(),
                    target_name: &target_name,
                    message_type: std::any::type_name::<M>(),
                    send_mode: SendMode::Stream,
                    remote: false,
                };
                let item_headers = Headers::new();
                let mut disposition = Disposition::Continue;
                for interceptor in interceptors.iter() {
                    disposition = interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                    if !matches!(disposition, Disposition::Continue) { break; }
                }
                seq += 1;
                match disposition {
                    Disposition::Continue => {
                        if out_tx.send(item).await.is_err() { break; }
                    }
                    Disposition::Drop => continue,
                    Disposition::Delay(d) => {
                        tokio::time::sleep(d).await;
                        if out_tx.send(item).await.is_err() { break; }
                    }
                    Disposition::Reject(_) | Disposition::Retry(_) => break,
                }
            }
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(out_rx)))
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
        // Note: no on_send() for FeedMessage — it's a type tag with no data.
        // Per-item interception happens via on_stream_item() in the drain task.

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
            .cast(DactorMsg(dispatch))
            .map_err(|e| ActorSendError(e.to_string()))?;

        // Drain task: intercept + batch input items, then unbatch into item_tx
        let interceptors = self.outbound_interceptors.clone();
        let target_id = self.id.clone();
        let target_name = self.name.clone();
        tokio::spawn(async move {
            use futures::{FutureExt, StreamExt};

            // Intercept input stream: run on_stream_item per item, handle Disposition
            let (intercepted_tx, intercepted_rx) = tokio::sync::mpsc::channel::<M::Item>(buffer);
            let intercept_handle = tokio::spawn({
                let cancel = cancel.clone();
                async move {
                    let mut input = input;
                    let mut seq: u64 = 0;
                    let item_headers = Headers::new();
                    loop {
                        let next_item = if let Some(ref token) = cancel {
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => break,
                                item = input.next() => item,
                            }
                        } else {
                            input.next().await
                        };
                        match next_item {
                            Some(item) => {
                                let octx = OutboundContext {
                                    target_id: target_id.clone(),
                                    target_name: &target_name,
                                    message_type: std::any::type_name::<M>(),
                                    send_mode: SendMode::Feed,
                                    remote: false,
                                };
                                let mut disposition = Disposition::Continue;
                                let mut drop_interceptor = "";
                                for interceptor in interceptors.iter() {
                                    disposition = interceptor.on_stream_item(&octx, &item_headers, seq, &item as &dyn Any);
                                    if !matches!(disposition, Disposition::Continue) {
                                        drop_interceptor = interceptor.name();
                                        break;
                                    }
                                }
                                seq += 1;
                                match disposition {
                                    Disposition::Continue => {
                                        if intercepted_tx.send(item).await.is_err() { break; }
                                    }
                                    Disposition::Drop => {
                                        tracing::warn!(
                                            target_name = %target_name,
                                            message_type = std::any::type_name::<M>(),
                                            interceptor = drop_interceptor,
                                            seq = seq - 1,
                                            "stream item dropped by interceptor"
                                        );
                                        continue;
                                    }
                                    Disposition::Delay(d) => {
                                        tokio::time::sleep(d).await;
                                        if intercepted_tx.send(item).await.is_err() { break; }
                                    }
                                    Disposition::Reject(_) | Disposition::Retry(_) => break,
                                }
                            }
                            None => break,
                        }
                    }
                }
            });

            // Batched channel
            let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<Vec<M::Item>>(buffer);

            // Writer task: batch intercepted items
            let writer_handle = tokio::spawn(async move {
                let mut intercepted_rx = intercepted_rx;
                let batch_delay = batch_config.max_delay;
                let mut writer = BatchWriter::new(batch_tx, batch_config);
                let result = std::panic::AssertUnwindSafe(async {
                    loop {
                        if writer.buffered_count() > 0 {
                            let deadline = tokio::time::Instant::now() + batch_delay;
                            tokio::select! {
                                biased;
                                item = intercepted_rx.recv() => match item {
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
                            match intercepted_rx.recv().await {
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
            let _ = intercept_handle.await;
        });

        Ok(AskReply::new(reply_rx))
    }
}

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor, including the inbound interceptor pipeline.
/// Options for spawning an actor. Use `..Default::default()` to future-proof
/// against new fields.
pub struct SpawnOptions {
    /// Inbound interceptors to attach to the actor.
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
    /// Mailbox capacity configuration.
    ///
    /// **Note:** The ractor adapter currently only supports unbounded mailboxes.
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
// RactorRuntime
// ---------------------------------------------------------------------------

/// A dactor v0.2 runtime backed by ractor.
///
/// Actors are spawned as real ractor actors via [`ractor::Actor::spawn`].
/// Messages are delivered through ractor's mailbox as type-erased dispatch
/// envelopes, supporting multiple `Handler<M>` impls per actor.
pub struct RactorRuntime {
    node_id: NodeId,
    next_local: AtomicU64,
    cluster_events: RactorClusterEvents,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    watchers: WatcherMap,
}

impl RactorRuntime {
    /// Create a new `RactorRuntime`.
    pub fn new() -> Self {
        Self {
            node_id: NodeId("ractor-node".into()),
            next_local: AtomicU64::new(1),
            cluster_events: RactorClusterEvents::new(),
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
    pub fn cluster_events_handle(&self) -> &RactorClusterEvents {
        &self.cluster_events
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events(&self) -> &RactorClusterEvents {
        &self.cluster_events
    }

    /// Spawn an actor with `Deps = ()`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> RactorActorRef<A>
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
    ) -> RactorActorRef<A>
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
    ) -> RactorActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        if !matches!(options.mailbox, MailboxConfig::Unbounded) {
            tracing::warn!("ractor adapter: bounded mailbox not yet implemented, using unbounded");
        }
        self.spawn_internal::<A>(name, args, (), options.interceptors)
    }

    fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
    ) -> RactorActorRef<A>
    where
        A: Actor + 'static,
    {
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();

        let wrapper = RactorDactorActor::<A> {
            _phantom: PhantomData,
        };
        let spawn_args = RactorSpawnArgs {
            args,
            deps,
            actor_id: actor_id.clone(),
            actor_name: actor_name.clone(),
            interceptors,
            watchers: self.watchers.clone(),
        };

        // Bridge sync → async: use a std thread to avoid blocking the
        // current tokio runtime while awaiting ractor::Actor::spawn.
        let name_for_ractor = name.to_string();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            handle.block_on(async move {
                let result =
                    ractor::Actor::spawn(Some(name_for_ractor), wrapper, spawn_args).await;
                match result {
                    Ok((actor_ref, _join)) => {
                        let _ = tx.send(Ok(actor_ref));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                    }
                }
            });
        });

        let actor_ref = match rx.recv() {
            Ok(Ok(actor_ref)) => actor_ref,
            Ok(Err(e)) => panic!("failed to spawn ractor actor: {e}"),
            Err(_) => panic!("ractor actor spawn channel closed unexpectedly"),
        };

        RactorActorRef {
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
    pub fn watch<W>(&self, watcher: &RactorActorRef<W>, target_id: ActorId)
    where
        W: Actor + Handler<ChildTerminated> + 'static,
    {
        let watcher_id = watcher.id();
        let watcher_inner = watcher.inner.clone();

        let entry = WatchEntry {
            watcher_id,
            notify: Box::new(move |msg: ChildTerminated| {
                let dispatch: Box<dyn Dispatch<W>> = Box::new(TypedDispatch { msg });
                if watcher_inner.cast(DactorMsg(dispatch)).is_err() {
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

impl Default for RactorRuntime {
    fn default() -> Self {
        Self::new()
    }
}
