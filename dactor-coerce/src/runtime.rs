//! V0.2 coerce adapter runtime for the dactor actor framework.
//!
//! Bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API with coerce-rs's
//! actor system using type-erased dispatch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use dactor::actor::{
    Actor, ActorContext, ActorError, ActorRef, AskReply, ReduceHandler, Handler, ExpandHandler,
    TransformHandler,
};
use dactor::dead_letter::{DeadLetterEvent, DeadLetterHandler, DeadLetterReason};
use dactor::dispatch::{AskDispatch, Dispatch, ReduceDispatch, ExpandDispatch, TransformDispatch, TypedDispatch};
use dactor::errors::{ActorSendError, ErrorAction, RuntimeError};
use dactor::interceptor::{
    collect_handler_wrappers, apply_handler_wrappers,
    Disposition, DropObserver, InboundContext, InboundInterceptor, OutboundInterceptor, Outcome,
    SendMode,
};
use dactor::mailbox::MailboxConfig;
use dactor::message::{Headers, Message, RuntimeHeaders};
use dactor::node::{ActorId, NodeId};
use dactor::runtime_support::{
    spawn_reduce_batched_drain, spawn_reduce_drain, spawn_transform_drain,
    wrap_batched_stream_with_interception, wrap_stream_with_interception, OutboundPipeline,
};
use dactor::stream::{
    BatchConfig, BatchReader, BatchWriter, BoxStream, StreamReceiver, StreamSender,
};
use dactor::supervision::ChildTerminated;
use dactor::system_actors::{
    CancelManager, CancelResponse, NodeDirectory, PeerStatus, SpawnManager, SpawnRequest,
    SpawnResponse, WatchManager, WatchNotification,
};
use dactor::type_registry::TypeRegistry;

use crate::cluster::CoerceClusterEvents;

// -- coerce-rs imports --
use coerce::actor::context::ActorContext as CoerceActorCtx;
use coerce::actor::message::{Handler as CoerceHandler, Message as CoerceMessage};
use coerce::actor::system::ActorSystem;
use coerce::actor::{Actor as CoerceActor, LocalActorRef};

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
// Coerce wrapper actor
// ---------------------------------------------------------------------------

/// The coerce message type wrapping a type-erased dactor dispatch envelope.
// Wrap Box<dyn Dispatch<A>> in a Sync-safe container using Mutex.
// coerce::actor::message::Message requires Sync + Send + 'static.
// The Mutex is never contended — it's locked once in handle() to take
// the dispatch out. This avoids an unsound manual Sync impl.
struct DactorMsg<A: Actor>(std::sync::Mutex<Option<Box<dyn Dispatch<A>>>>);

impl<A: Actor> DactorMsg<A> {
    fn new(dispatch: Box<dyn Dispatch<A>>) -> Self {
        Self(std::sync::Mutex::new(Some(dispatch)))
    }

    fn take(&self) -> Option<Box<dyn Dispatch<A>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl<A: Actor + Send + Sync + 'static> CoerceMessage for DactorMsg<A> {
    type Result = ();
}

/// The coerce `Actor` implementation that bridges to a dactor `Actor`.
struct CoerceDactorActor<A: Actor> {
    actor: A,
    ctx: ActorContext,
    interceptors: Vec<Box<dyn InboundInterceptor>>,
    watchers: WatcherMap,
    stop_reason: Option<String>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
    /// Notified when the actor stops (for await_stop).
    /// Wrapped in Mutex to make CoerceDactorActor Sync (oneshot::Sender is !Sync).
    stop_notifier: Mutex<Option<tokio::sync::oneshot::Sender<Result<(), String>>>>,
}

#[async_trait::async_trait]
impl<A: Actor + Send + Sync + 'static> CoerceActor for CoerceDactorActor<A> {
    async fn started(&mut self, _ctx: &mut CoerceActorCtx) {
        self.actor.on_start(&mut self.ctx).await;
    }

    async fn stopped(&mut self, _ctx: &mut CoerceActorCtx) {
        // Reset context to lifecycle semantics before on_stop
        self.ctx.send_mode = None;
        self.ctx.headers = Headers::new();
        self.ctx.set_cancellation_token(None);

        // Run on_stop with panic catching so we can propagate errors
        let stop_result =
            std::panic::AssertUnwindSafe(self.actor.on_stop())
                .catch_unwind()
                .await;
        let stop_err = match stop_result {
            Ok(()) => None,
            Err(_panic) => Some("actor panicked in on_stop".to_string()),
        };

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

        // Notify await_stop() waiters with the result
        if let Some(tx) = self.stop_notifier.lock().unwrap().take() {
            let result = match &stop_err {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            };
            let _ = tx.send(result);
        }
    }
}

#[async_trait::async_trait]
impl<A: Actor + Send + Sync + 'static> CoerceHandler<DactorMsg<A>> for CoerceDactorActor<A> {
    async fn handle(&mut self, msg: DactorMsg<A>, coerce_ctx: &mut CoerceActorCtx) {
        let dispatch = match msg.take() {
            Some(d) => d,
            None => return,
        };

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
            if matches!(disposition, Disposition::Drop) {
                if let Some(ref handler) = *self.dead_letter_handler {
                    let event = DeadLetterEvent {
                        target_id: self.ctx.actor_id.clone(),
                        target_name: Some(self.ctx.actor_name.clone()),
                        message_type,
                        send_mode,
                        reason: DeadLetterReason::DroppedByInterceptor {
                            interceptor: interceptor_name.clone(),
                        },
                        message: None,
                    };
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler.on_dead_letter(event);
                    }));
                }
            }
            dispatch.reject(disposition, &interceptor_name);
            return;
        }

        if !total_delay.is_zero() {
            tokio::time::sleep(total_delay).await;
        }

        // Collect handler wrappers BEFORE moving headers into ctx,
        // so interceptors can read headers without borrow conflicts.
        let ictx_for_wrap = InboundContext {
            actor_id: self.ctx.actor_id.clone(),
            actor_name: &self.ctx.actor_name,
            message_type,
            send_mode,
            remote: false,
            origin_node: None,
        };
        let wrappers = collect_handler_wrappers(&self.interceptors, &ictx_for_wrap, &headers);
        let needs_wrap = wrappers.iter().any(|w| w.is_some());

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
        let result = if needs_wrap {
            let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();

            let inner: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> = Box::pin(async {
                let r = std::panic::AssertUnwindSafe(
                    dispatch.dispatch(&mut self.actor, &mut self.ctx),
                )
                .catch_unwind()
                .await;
                let _ = result_tx.send(r);
            });

            let wrapped = apply_handler_wrappers(wrappers, inner);

            if let Some(ref token) = cancel_token {
                tokio::select! {
                    biased;
                    _ = wrapped => {},
                    _ = token.cancelled() => {
                        self.ctx.set_cancellation_token(None);
                        return;
                    }
                }
            } else {
                wrapped.await;
            }

            match result_rx.try_recv() {
                Ok(r) => r,
                Err(_) => Err(Box::new(
                    "interceptor wrap_handler did not await the handler future",
                ) as Box<dyn std::any::Any + Send>),
            }
        } else {
            if let Some(ref token) = cancel_token {
                let dispatch_fut =
                    std::panic::AssertUnwindSafe(dispatch.dispatch(&mut self.actor, &mut self.ctx))
                        .catch_unwind();
                tokio::select! {
                    biased;
                    r = dispatch_fut => r,
                    _ = token.cancelled() => {
                        self.ctx.set_cancellation_token(None);
                        return;
                    }
                }
            } else {
                std::panic::AssertUnwindSafe(dispatch.dispatch(&mut self.actor, &mut self.ctx))
                    .catch_unwind()
                    .await
            }
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
                    (Some(reply), SendMode::Ask) => Outcome::AskSuccess {
                        reply: reply.as_ref(),
                    },
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
                        // Actor resumes — do NOT set stop_reason
                    }
                    ErrorAction::Stop | ErrorAction::Escalate => {
                        self.stop_reason = Some("handler panicked".into());
                        coerce_ctx.stop(None);
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
// CoerceActorRef — dactor ActorRef backed by coerce
// ---------------------------------------------------------------------------

/// A dactor `ActorRef` backed by a coerce `LocalActorRef`.
///
/// Messages are delivered through coerce's mailbox as type-erased dispatch
/// envelopes, enabling multiple `Handler<M>` impls per actor.
///
/// When configured with [`MailboxConfig::Bounded`], a bounded `mpsc` channel
/// sits in front of the coerce actor.  A forwarding task drains the channel
/// and delivers messages to coerce, providing backpressure / overflow control
/// at the dactor level while coerce's internal mailbox remains unbounded.
pub struct CoerceActorRef<A: Actor + Send + Sync + 'static> {
    id: ActorId,
    name: String,
    inner: LocalActorRef<CoerceDactorActor<A>>,
    /// When Some, sends go through this bounded channel (a forwarder task
    /// drains it into `inner`).  When None, sends go directly to `inner`.
    bounded_tx: Option<BoundedMailboxSender<DactorMsg<A>>>,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    drop_observer: Option<Arc<dyn DropObserver>>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
}

use dactor::runtime_support::BoundedMailboxSender;

impl<A: Actor + Send + Sync + 'static> Clone for CoerceActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            inner: self.inner.clone(),
            bounded_tx: self.bounded_tx.clone(),
            outbound_interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
        }
    }
}

impl<A: Actor + Send + Sync + 'static> std::fmt::Debug for CoerceActorRef<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CoerceActorRef({}, {:?})", self.name, self.id)
    }
}

impl<A: Actor + Send + Sync> CoerceActorRef<A> {
    fn outbound_pipeline(&self) -> OutboundPipeline {
        OutboundPipeline {
            interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            target_id: self.id.clone(),
            target_name: self.name.clone(),
        }
    }

    fn notify_dead_letter(
        &self,
        message_type: &'static str,
        send_mode: SendMode,
        reason: DeadLetterReason,
    ) {
        if let Some(ref handler) = *self.dead_letter_handler {
            let event = DeadLetterEvent {
                target_id: self.id.clone(),
                target_name: Some(self.name.clone()),
                message_type,
                send_mode,
                reason,
                message: None,
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler.on_dead_letter(event);
            }));
        }
    }

    /// Send a dispatch envelope through the bounded channel (if configured)
    /// or directly to the coerce actor.
    fn send_dispatch(&self, dispatch: Box<dyn Dispatch<A>>) -> Result<(), ActorSendError> {
        let dactor_msg = DactorMsg::new(dispatch);
        if let Some(ref btx) = self.bounded_tx {
            btx.try_send(dactor_msg)
        } else {
            self.inner
                .notify(dactor_msg)
                .map_err(|e| ActorSendError(e.to_string()))
        }
    }
}

impl<A: Actor + Send + Sync + 'static> ActorRef<A> for CoerceActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        if let Some(ref btx) = self.bounded_tx {
            !btx.is_closed() && self.inner.is_valid()
        } else {
            self.inner.is_valid()
        }
    }

    fn pending_messages(&self) -> usize {
        if let Some(ref btx) = self.bounded_tx {
            btx.pending()
        } else {
            0 // unbounded — no depth info
        }
    }

    fn stop(&self) {
        let _ = self.inner.notify_stop();
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Tell, &msg);
        match result.disposition {
            Disposition::Continue => {}
            Disposition::Drop | Disposition::Reject(_) | Disposition::Retry(_) => return Ok(()),
            Disposition::Delay(_) => {} // Not supported in sync tell
        }

        let dispatch: Box<dyn Dispatch<A>> = Box::new(TypedDispatch { msg });
        self.send_dispatch(dispatch).map_err(|e| {
            let reason = if e.0.contains("full") {
                DeadLetterReason::MailboxFull
            } else {
                DeadLetterReason::ActorStopped
            };
            self.notify_dead_letter(
                std::any::type_name::<M>(),
                SendMode::Tell,
                reason,
            );
            e
        })
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
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Ask, &msg);
        match result.disposition {
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
                let _ = tx.send(Err(RuntimeError::Rejected {
                    interceptor: result.interceptor_name.to_string(),
                    reason,
                }));
                return Ok(AskReply::new(rx));
            }
            Disposition::Retry(retry_after) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(Err(RuntimeError::RetryAfter {
                    interceptor: result.interceptor_name.to_string(),
                    retry_after,
                }));
                return Ok(AskReply::new(rx));
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(AskDispatch {
            msg,
            reply_tx: tx,
            cancel,
        });
        self.send_dispatch(dispatch).map_err(|e| {
            let reason = if e.0.contains("full") {
                DeadLetterReason::MailboxFull
            } else {
                DeadLetterReason::ActorStopped
            };
            self.notify_dead_letter(
                std::any::type_name::<M>(),
                SendMode::Ask,
                reason,
            );
            e
        })?;
        Ok(AskReply::new(rx))
    }

    fn expand<M, OutputItem>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<OutputItem>, ActorSendError>
    where
        A: ExpandHandler<M, OutputItem>,
        M: Send + 'static,
        OutputItem: Send + 'static,
    {
        let pipeline = self.outbound_pipeline();
        let result = pipeline.run_on_send(SendMode::Expand, &msg);
        match result.disposition {
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

        let buffer = buffer.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(buffer);
        let sender = StreamSender::new(tx);
        let dispatch: Box<dyn Dispatch<A>> = Box::new(ExpandDispatch {
            msg,
            sender,
            cancel,
        });
        self.send_dispatch(dispatch)?;

        match batch_config {
            Some(batch_config) => {
                let (batch_tx, batch_rx) = tokio::sync::mpsc::channel::<Vec<OutputItem>>(buffer);
                let reader = BatchReader::new(batch_rx);
                let batch_delay = batch_config.max_delay;
                tokio::spawn(async move {
                    let mut writer = BatchWriter::new(batch_tx, batch_config);
                    let mut rx = rx;
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
                                    if writer.push(item).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                    let _ = writer.flush().await;
                });
                Ok(wrap_batched_stream_with_interception(
                    reader,
                    buffer,
                    pipeline,
                    std::any::type_name::<M>(),
                    SendMode::Expand,
                ))
            }
            None => Ok(wrap_stream_with_interception(
                    rx,
                    buffer,
                    pipeline,
                    std::any::type_name::<M>(),
                    SendMode::Expand,
                )),
        }
    }

    fn reduce<InputItem, Reply>(
        &self,
        input: BoxStream<InputItem>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: ReduceHandler<InputItem, Reply>,
        InputItem: Send + 'static,
        Reply: Send + 'static,
    {
        let buffer = buffer.max(1);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let dispatch: Box<dyn Dispatch<A>> = Box::new(ReduceDispatch {
            receiver,
            reply_tx,
            cancel: cancel.clone(),
        });
        self.send_dispatch(dispatch)?;

        let pipeline = self.outbound_pipeline();
        match batch_config {
            Some(batch_config) => {
                spawn_reduce_batched_drain(
                    input,
                    item_tx,
                    buffer,
                    batch_config,
                    cancel,
                    pipeline,
                    std::any::type_name::<InputItem>(),
                );
            }
            None => {
                spawn_reduce_drain(
                    input,
                    item_tx,
                    cancel,
                    pipeline,
                    std::any::type_name::<InputItem>(),
                );
            }
        }

        Ok(AskReply::new(reply_rx))
    }

    fn transform<InputItem, OutputItem>(
        &self,
        input: BoxStream<InputItem>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<OutputItem>, ActorSendError>
    where
        A: TransformHandler<InputItem, OutputItem>,
        InputItem: Send + 'static,
        OutputItem: Send + 'static,
    {
        let buffer = buffer.max(1);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(buffer);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(buffer);
        let receiver = StreamReceiver::new(item_rx);
        let sender = StreamSender::new(output_tx);
        let dispatch: Box<dyn Dispatch<A>> = Box::new(TransformDispatch::new(
            receiver,
            sender,
            cancel.clone(),
        ));
        self.send_dispatch(dispatch)?;

        let pipeline = self.outbound_pipeline();
        spawn_transform_drain(
            input,
            item_tx,
            cancel,
            pipeline.clone(),
            std::any::type_name::<InputItem>(),
        );

        match batch_config {
            Some(batch_config) => {
                let (batch_tx, batch_rx) =
                    tokio::sync::mpsc::channel::<Vec<OutputItem>>(buffer);
                let reader = BatchReader::new(batch_rx);
                let batch_delay = batch_config.max_delay;
                tokio::spawn(async move {
                    let mut writer = BatchWriter::new(batch_tx, batch_config);
                    loop {
                        if writer.buffered_count() > 0 {
                            let deadline = tokio::time::Instant::now() + batch_delay;
                            tokio::select! {
                                biased;
                                item = output_rx.recv() => match item {
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
                            match output_rx.recv().await {
                                Some(item) => {
                                    if writer.push(item).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                    let _ = writer.flush().await;
                });
                Ok(wrap_batched_stream_with_interception(
                    reader,
                    buffer,
                    pipeline,
                    std::any::type_name::<OutputItem>(),
                    SendMode::Transform,
                ))
            }
            None => Ok(wrap_stream_with_interception(
                output_rx,
                buffer,
                pipeline,
                std::any::type_name::<OutputItem>(),
                SendMode::Transform,
            )),
        }
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
    /// When [`Bounded`](MailboxConfig::Bounded), a bounded `mpsc` channel is
    /// placed in front of the coerce actor to enforce backpressure.  The
    /// [`OverflowStrategy`](dactor::mailbox::OverflowStrategy) controls what
    /// happens when the channel is full.
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
// CoerceRuntime
// ---------------------------------------------------------------------------

/// References to the native coerce system actors spawned by the runtime.
pub struct CoerceSystemActorRefs {
    pub spawn_managers: Vec<LocalActorRef<crate::system_actors::SpawnManagerActor>>,
    spawn_manager_counter: std::sync::atomic::AtomicU64,
    pub watch_manager: LocalActorRef<crate::system_actors::WatchManagerActor>,
    pub cancel_manager: LocalActorRef<crate::system_actors::CancelManagerActor>,
    pub node_directory: LocalActorRef<crate::system_actors::NodeDirectoryActor>,
}

impl CoerceSystemActorRefs {
    /// Get the spawn manager ref (round-robin if pooled).
    pub fn spawn_manager(&self) -> &LocalActorRef<crate::system_actors::SpawnManagerActor> {
        let idx = self.spawn_manager_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            as usize % self.spawn_managers.len();
        &self.spawn_managers[idx]
    }
}

/// A dactor v0.2 runtime backed by coerce-rs.
///
/// Actors are spawned as real coerce actors. Messages are delivered through
/// coerce's mailbox as type-erased dispatch envelopes, supporting multiple
/// `Handler<M>` impls per actor.
///
/// **Note:** `CoerceRuntime::new()` creates a `coerce::actor::system::ActorSystem`
/// internally, which requires a tokio runtime context. All `spawn()` methods
/// also require a tokio context.
///
/// ## System Actors
///
/// The runtime includes struct-based system actors for remote operations:
/// - [`SpawnManager`] — handles remote actor spawn requests
/// - [`WatchManager`] — handles remote watch/unwatch subscriptions
/// - [`CancelManager`] — handles remote cancellation requests
/// - [`NodeDirectory`] — tracks peer node connection state
///
/// Native coerce system actors are accessible via `system_actor_refs()` for
/// transport routing after calling `start_system_actors()`.
pub struct CoerceRuntime {
    node_id: NodeId,
    next_local: Arc<AtomicU64>,
    system: ActorSystem,
    cluster_events: CoerceClusterEvents,
    outbound_interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    drop_observer: Option<Arc<dyn DropObserver>>,
    dead_letter_handler: Arc<Option<Arc<dyn DeadLetterHandler>>>,
    watchers: WatcherMap,
    /// Plain struct system actors (backward-compatible sync API).
    spawn_manager: SpawnManager,
    watch_manager: WatchManager,
    cancel_manager: CancelManager,
    node_directory: NodeDirectory,
    /// Native coerce system actor refs (lazily started via `start_system_actors()`).
    system_actors: Option<CoerceSystemActorRefs>,
    /// Stop notification receivers for await_stop(), keyed by ActorId.
    #[allow(clippy::type_complexity)]
    stop_receivers: Arc<Mutex<HashMap<ActorId, tokio::sync::oneshot::Receiver<Result<(), String>>>>>,
    /// Application version for this node (informational, used in handshake).
    app_version: Option<String>,
}

impl CoerceRuntime {
    /// Create a new `CoerceRuntime`.
    ///
    /// **Requires a tokio runtime context** — `ActorSystem::new()` spawns
    /// the coerce scheduler as a tokio task.
    pub fn new() -> Self {
        Self::create(NodeId("coerce-node".into()))
    }

    /// Create a new `CoerceRuntime` with a specific node ID.
    ///
    /// **Requires a tokio runtime context.**
    pub fn with_node_id(node_id: NodeId) -> Self {
        Self::create(node_id)
    }

    fn create(node_id: NodeId) -> Self {
        Self {
            node_id,
            next_local: Arc::new(AtomicU64::new(1)),
            system: ActorSystem::new(),
            cluster_events: CoerceClusterEvents::new(),
            outbound_interceptors: Arc::new(Vec::new()),
            drop_observer: None,
            dead_letter_handler: Arc::new(None),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            spawn_manager: SpawnManager::new(TypeRegistry::new()),
            watch_manager: WatchManager::new(),
            cancel_manager: CancelManager::new(),
            node_directory: NodeDirectory::new(),
            system_actors: None,
            stop_receivers: Arc::new(Mutex::new(HashMap::new())),
            app_version: None,
        }
    }

    /// The adapter name for this runtime, used in version handshakes.
    pub const ADAPTER_NAME: &'static str = "coerce";

    /// Set the application version for this node.
    ///
    /// This is your application's release version (e.g., "2.3.1"), not the
    /// dactor framework version. It is included in handshake requests for
    /// operational visibility during rolling upgrades.
    pub fn with_app_version(mut self, version: impl Into<String>) -> Self {
        self.app_version = Some(version.into());
        self
    }

    /// Returns the configured application version, if any.
    pub fn app_version(&self) -> Option<&str> {
        self.app_version.as_deref()
    }

    /// Build a [`HandshakeRequest`](dactor::HandshakeRequest) from this
    /// runtime's current configuration.
    pub fn handshake_request(&self) -> dactor::HandshakeRequest {
        dactor::HandshakeRequest::from_runtime(
            self.node_id.clone(),
            self.app_version.clone(),
            Self::ADAPTER_NAME,
        )
    }

    /// Returns the node ID of this runtime.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Returns a reference to the underlying coerce `ActorSystem`.
    pub fn system(&self) -> &ActorSystem {
        &self.system
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

    /// Set the drop observer for outbound interceptor pipelines.
    ///
    /// **Must be called before any actors are spawned.**
    pub fn set_drop_observer(&mut self, observer: Arc<dyn DropObserver>) {
        self.drop_observer = Some(observer);
    }

    /// Set a global dead letter handler. Called whenever a message cannot be
    /// delivered (actor stopped, mailbox full, dropped by inbound interceptor).
    pub fn set_dead_letter_handler(&mut self, handler: Arc<dyn DeadLetterHandler>) {
        self.dead_letter_handler = Arc::new(Some(handler));
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events_handle(&self) -> &CoerceClusterEvents {
        &self.cluster_events
    }

    /// Access the cluster events subsystem.
    pub fn cluster_events(&self) -> &CoerceClusterEvents {
        &self.cluster_events
    }

    /// Spawn native coerce system actors for transport routing.
    ///
    /// Must be called from within a tokio runtime context. After this call,
    /// `system_actor_refs()` returns the native actor references.
    ///
    /// Uses default configuration (unbounded mailboxes, no pooling).
    /// For custom configuration, use [`start_system_actors_with_config()`].
    pub fn start_system_actors(&mut self) {
        self.start_system_actors_with_config(dactor::SystemActorConfig::default());
    }

    /// Spawn native coerce system actors with custom configuration.
    ///
    /// Allows configuring SpawnManager pooling for high-throughput scenarios.
    /// See [`SystemActorConfig`](dactor::SystemActorConfig) for details.
    pub fn start_system_actors_with_config(&mut self, config: dactor::SystemActorConfig) {
        use crate::system_actors::*;

        let pool_size = config.spawn_manager_pool_size.unwrap_or(1).max(1);
        let system = self.system.clone();
        let mut spawn_refs = Vec::with_capacity(pool_size);

        for i in 0..pool_size {
            let spawn_mgr = SpawnManagerActor::new(
                self.node_id.clone(),
                TypeRegistry::new(),
                self.next_local.clone(),
            );
            let spawn_mgr_ref = coerce::actor::scheduler::start_actor(
                spawn_mgr,
                coerce::actor::new_actor_id(),
                coerce::actor::scheduler::ActorType::Tracked,
                None,
                Some(system.clone()),
                None,
                format!("dactor-spawn-manager-{i}").into(),
            );
            spawn_refs.push(spawn_mgr_ref);
        }

        let watch_mgr = WatchManagerActor::new();
        let cancel_mgr = CancelManagerActor::new();
        let node_dir = NodeDirectoryActor::new();

        let watch_mgr_ref = coerce::actor::scheduler::start_actor(
            watch_mgr,
            coerce::actor::new_actor_id(),
            coerce::actor::scheduler::ActorType::Tracked,
            None,
            Some(system.clone()),
            None,
            "dactor-watch-manager".to_string().into(),
        );
        let cancel_mgr_ref = coerce::actor::scheduler::start_actor(
            cancel_mgr,
            coerce::actor::new_actor_id(),
            coerce::actor::scheduler::ActorType::Tracked,
            None,
            Some(system.clone()),
            None,
            "dactor-cancel-manager".to_string().into(),
        );
        let node_dir_ref = coerce::actor::scheduler::start_actor(
            node_dir,
            coerce::actor::new_actor_id(),
            coerce::actor::scheduler::ActorType::Tracked,
            None,
            Some(system),
            None,
            "dactor-node-directory".to_string().into(),
        );

        self.system_actors = Some(CoerceSystemActorRefs {
            spawn_managers: spawn_refs,
            spawn_manager_counter: std::sync::atomic::AtomicU64::new(0),
            watch_manager: watch_mgr_ref,
            cancel_manager: cancel_mgr_ref,
            node_directory: node_dir_ref,
        });
    }

    /// Access the native system actor references for transport routing.
    ///
    /// Returns `None` if `start_system_actors()` has not been called yet.
    pub fn system_actor_refs(&self) -> Option<&CoerceSystemActorRefs> {
        self.system_actors.as_ref()
    }

    /// Spawn an actor with `Deps = ()`.
    pub async fn spawn<A>(&self, name: &str, args: A::Args) -> Result<CoerceActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor<Deps = ()> + Send + Sync + 'static,
    {
        Ok(self.spawn_internal::<A>(name, args, (), Vec::new(), MailboxConfig::Unbounded))
    }

    /// Spawn an actor with explicit dependencies.
    pub async fn spawn_with_deps<A>(&self, name: &str, args: A::Args, deps: A::Deps) -> Result<CoerceActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor + Send + Sync + 'static,
    {
        Ok(self.spawn_internal::<A>(name, args, deps, Vec::new(), MailboxConfig::Unbounded))
    }

    /// Spawn an actor with spawn options (including inbound interceptors and mailbox config).
    pub async fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> Result<CoerceActorRef<A>, dactor::errors::RuntimeError>
    where
        A: Actor<Deps = ()> + Send + Sync + 'static,
    {
        Ok(self.spawn_internal::<A>(name, args, (), options.interceptors, options.mailbox))
    }

    fn spawn_internal<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
        interceptors: Vec<Box<dyn InboundInterceptor>>,
        mailbox: MailboxConfig,
    ) -> CoerceActorRef<A>
    where
        A: Actor + Send + Sync + 'static,
    {
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();

        let actor = A::create(args, deps);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

        let wrapper = CoerceDactorActor {
            actor,
            ctx: ActorContext::new(actor_id.clone(), actor_name.clone()),
            interceptors,
            watchers: self.watchers.clone(),
            stop_reason: None,
            dead_letter_handler: self.dead_letter_handler.clone(),
            stop_notifier: Mutex::new(Some(stop_tx)),
        };

        // Use coerce's synchronous start_actor — it internally calls
        // tokio::spawn (non-blocking) to run the actor loop, and returns
        // the LocalActorRef immediately. No async bridge needed.
        let system = self.system.clone();
        let coerce_ref = coerce::actor::scheduler::start_actor(
            wrapper,
            coerce::actor::new_actor_id(),
            coerce::actor::scheduler::ActorType::Tracked,
            None,
            Some(system),
            None,
            name.to_string().into(),
        );

        // Set up optional bounded mailbox channel
        let bounded_tx = match mailbox {
            MailboxConfig::Bounded { capacity, overflow } => {
                let (btx, mut brx) =
                    tokio::sync::mpsc::channel::<DactorMsg<A>>(capacity);
                let fwd_ref = coerce_ref.clone();
                tokio::spawn(async move {
                    while let Some(msg) = brx.recv().await {
                        if fwd_ref.notify(msg).is_err() {
                            break; // actor stopped
                        }
                    }
                });
                Some(BoundedMailboxSender::new(btx, overflow))
            }
            MailboxConfig::Unbounded => None,
        };

        // Store stop receiver for await_stop()
        self.stop_receivers
            .lock()
            .unwrap()
            .insert(actor_id.clone(), stop_rx);

        CoerceActorRef {
            id: actor_id,
            name: actor_name,
            inner: coerce_ref,
            bounded_tx,
            outbound_interceptors: self.outbound_interceptors.clone(),
            drop_observer: self.drop_observer.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
        }
    }

    /// Register actor `watcher` to be notified when `target_id` terminates.
    ///
    /// The watcher must implement `Handler<ChildTerminated>`. When the target
    /// stops, the runtime delivers a [`ChildTerminated`] message to the watcher.
    pub fn watch<W>(&self, watcher: &CoerceActorRef<W>, target_id: ActorId)
    where
        W: Actor + Handler<ChildTerminated> + Send + Sync + 'static,
    {
        let watcher_id = watcher.id();
        let watcher_inner = watcher.inner.clone();

        let entry = WatchEntry {
            watcher_id,
            notify: Box::new(move |msg: ChildTerminated| {
                let dispatch: Box<dyn Dispatch<W>> = Box::new(TypedDispatch { msg });
                if watcher_inner.notify(DactorMsg::new(dispatch)).is_err() {
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

    // -----------------------------------------------------------------------
    // SpawnManager wiring
    // -----------------------------------------------------------------------

    /// Access the spawn manager.
    pub fn spawn_manager(&self) -> &SpawnManager {
        &self.spawn_manager
    }

    /// Access the spawn manager mutably (for registering actor factories).
    pub fn spawn_manager_mut(&mut self) -> &mut SpawnManager {
        &mut self.spawn_manager
    }

    /// Register an actor type for remote spawning on this node.
    ///
    /// Registers the factory in both the struct-based SpawnManager and the
    /// native SpawnManagerActor (if started via `start_system_actors()`).
    pub fn register_factory(
        &mut self,
        type_name: impl Into<String>,
        factory: impl Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
            + Send
            + Sync
            + 'static,
    ) {
        let type_name = type_name.into();
        let factory = Arc::new(factory);

        // Register in struct-based manager
        self.spawn_manager
            .type_registry_mut()
            .register_factory(type_name.clone(), {
                let f = factory.clone();
                move |bytes: &[u8]| f(bytes)
            });

        // Forward to native actor(s) if started
        if let Some(ref actors) = self.system_actors {
            for worker in &actors.spawn_managers {
                let f = factory.clone();
                let _ = worker.notify(
                    crate::system_actors::RegisterFactory {
                        type_name: type_name.clone(),
                        factory: Box::new(move |bytes: &[u8]| f(bytes)),
                    },
                );
            }
        }
    }

    /// Process a remote spawn request.
    ///
    /// Looks up the actor type in the registry, deserializes Args from bytes,
    /// and returns the constructed actor along with its assigned [`ActorId`].
    ///
    /// The returned `ActorId` is pre-assigned for the remote spawn flow where
    /// the runtime controls ID assignment. The caller must use this ID when
    /// registering the spawned actor (not via the regular `spawn()` path,
    /// which assigns its own IDs).
    ///
    /// **Note:** Currently uses the simple factory API (`TypeRegistry::create_actor`).
    /// For actors with non-trivial `Deps`, use `spawn_manager_mut()` to access
    /// `TypeRegistry::create_actor_with_deps()` directly.
    ///
    /// Returns `Ok((actor_id, actor))` on success, or `Err(SpawnResponse::Failure)`
    /// if the type is not found or deserialization fails.
    pub fn handle_spawn_request(
        &mut self,
        request: &SpawnRequest,
    ) -> Result<(ActorId, Box<dyn std::any::Any + Send>), SpawnResponse> {
        match self.spawn_manager.create_actor(request) {
            Ok(actor) => {
                let local = self.next_local.fetch_add(1, Ordering::SeqCst);
                let actor_id = ActorId {
                    node: self.node_id.clone(),
                    local,
                };
                self.spawn_manager.record_spawn(actor_id.clone());
                Ok((actor_id, actor))
            }
            Err(e) => Err(SpawnResponse::Failure {
                request_id: request.request_id.clone(),
                error: e.to_string(),
            }),
        }
    }

    // -----------------------------------------------------------------------
    // WatchManager wiring
    // -----------------------------------------------------------------------

    /// Access the watch manager (for remote watch subscriptions).
    pub fn watch_manager(&self) -> &WatchManager {
        &self.watch_manager
    }

    /// Access the watch manager mutably.
    pub fn watch_manager_mut(&mut self) -> &mut WatchManager {
        &mut self.watch_manager
    }

    /// Register a remote watch: a remote watcher wants to know when a local
    /// actor terminates.
    pub fn remote_watch(&mut self, target: ActorId, watcher: ActorId) {
        self.watch_manager.watch(target, watcher);
    }

    /// Remove a remote watch subscription.
    pub fn remote_unwatch(&mut self, target: &ActorId, watcher: &ActorId) {
        self.watch_manager.unwatch(target, watcher);
    }

    /// Called when a local actor terminates. Returns notifications for all
    /// remote watchers that should be sent to their respective nodes.
    ///
    /// **Note:** This must be called explicitly by the integration layer.
    /// It is not yet automatically wired into coerce's actor stop lifecycle.
    pub fn notify_terminated(&mut self, terminated: &ActorId) -> Vec<WatchNotification> {
        self.watch_manager.on_terminated(terminated)
    }

    // -----------------------------------------------------------------------
    // CancelManager wiring
    // -----------------------------------------------------------------------

    /// Access the cancel manager.
    pub fn cancel_manager(&self) -> &CancelManager {
        &self.cancel_manager
    }

    /// Access the cancel manager mutably.
    pub fn cancel_manager_mut(&mut self) -> &mut CancelManager {
        &mut self.cancel_manager
    }

    /// Register a cancellation token for a request (for remote cancel support).
    pub fn register_cancel(&mut self, request_id: String, token: CancellationToken) {
        self.cancel_manager.register(request_id, token);
    }

    /// Process a remote cancellation request.
    pub fn cancel_request(&mut self, request_id: &str) -> CancelResponse {
        self.cancel_manager.cancel(request_id)
    }

    /// Clean up a cancellation token after its request completes normally.
    ///
    /// Should be called when a remote ask/stream/feed completes successfully
    /// to prevent stale tokens from accumulating.
    pub fn complete_request(&mut self, request_id: &str) {
        self.cancel_manager.remove(request_id);
    }

    // -----------------------------------------------------------------------
    // NodeDirectory wiring
    // -----------------------------------------------------------------------

    /// Access the node directory.
    pub fn node_directory(&self) -> &NodeDirectory {
        &self.node_directory
    }

    /// Access the node directory mutably.
    pub fn node_directory_mut(&mut self) -> &mut NodeDirectory {
        &mut self.node_directory
    }

    /// Register a peer node as connected in the directory.
    ///
    /// **Post-validation only:** this method assumes the peer has already
    /// passed the version handshake. Callers should validate compatibility
    /// (via [`handshake_request`](Self::handshake_request) +
    /// [`validate_handshake`](dactor::validate_handshake) +
    /// [`verify_peer_identity`](dactor::verify_peer_identity)) before calling
    /// this method. Direct calls bypass compatibility checks.
    ///
    /// If the peer already exists, updates its status to `Connected` and
    /// preserves the existing address when `address` is `None`.
    /// Emits a `ClusterEvent::NodeJoined` if the peer was not previously connected.
    pub fn connect_peer(&mut self, peer_id: NodeId, address: Option<String>) {
        let was_connected = self.node_directory.is_connected(&peer_id);
        if let Some(existing) = self.node_directory.get_peer(&peer_id) {
            let resolved_address = address.or_else(|| existing.address.clone());
            self.node_directory.remove_peer(&peer_id);
            self.node_directory
                .add_peer(peer_id.clone(), resolved_address);
        } else {
            self.node_directory.add_peer(peer_id.clone(), address);
        }
        self.node_directory
            .set_status(&peer_id, PeerStatus::Connected);
        if !was_connected {
            self.cluster_events
                .emit(dactor::ClusterEvent::NodeJoined(peer_id));
        }
    }

    /// Mark a peer as disconnected.
    ///
    /// Emits a `ClusterEvent::NodeLeft` if the peer was previously connected.
    pub fn disconnect_peer(&mut self, peer_id: &NodeId) {
        let was_connected = self.node_directory.is_connected(peer_id);
        self.node_directory
            .set_status(peer_id, PeerStatus::Disconnected);
        if was_connected {
            self.cluster_events
                .emit(dactor::ClusterEvent::NodeLeft(peer_id.clone()));
        }
    }

    /// Check if a peer node is connected.
    pub fn is_peer_connected(&self, peer_id: &NodeId) -> bool {
        self.node_directory.is_connected(peer_id)
    }

    // -----------------------------------------------------------------------
    // Actor lifecycle handles
    // -----------------------------------------------------------------------

    /// Wait for an actor to stop.
    ///
    /// Returns `Ok(())` when the actor finishes cleanly, or `Err` if the
    /// actor panicked in `on_stop`. The stop receiver is consumed and removed
    /// from the map.
    ///
    /// Returns `Ok(())` immediately if no stop receiver is stored for this ID.
    pub async fn await_stop(&self, actor_id: &ActorId) -> Result<(), String> {
        let rx = {
            let mut receivers = self.stop_receivers.lock().unwrap();
            receivers.remove(actor_id)
        };
        match rx {
            Some(rx) => rx
                .await
                .map_err(|_| "stop notifier dropped".to_string())
                .and_then(|r| r),
            None => Ok(()),
        }
    }

    /// Wait for all spawned actors to stop.
    ///
    /// Drains all stored stop receivers and awaits them all. Returns the first
    /// error encountered, but always waits for every actor to finish.
    pub async fn await_all(&self) -> Result<(), String> {
        let receivers: Vec<_> = {
            let mut map = self.stop_receivers.lock().unwrap();
            map.drain().collect()
        };
        let mut first_error = None;
        for (_, rx) in receivers {
            let result = rx.await.map_err(|e| format!("stop notifier dropped: {e}")).and_then(|r| r);
            if let Err(e) = result {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Remove completed stop receivers from the map.
    ///
    /// Call periodically to prevent stale entries from accumulating
    /// for actors that stopped without being awaited.
    pub fn cleanup_finished(&self) {
        let mut receivers = self.stop_receivers.lock().unwrap();
        receivers.retain(|_, rx| {
            matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            )
        });
    }

    /// Number of actors with stored stop receivers.
    ///
    /// Note: includes receivers for actors that have already stopped but
    /// haven't been awaited or cleaned up. Call `cleanup_finished()` first
    /// for an accurate count of running actors.
    pub fn active_handle_count(&self) -> usize {
        self.stop_receivers.lock().unwrap().len()
    }
}

impl Default for CoerceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// NA10: SystemMessageRouter for coerce
// ---------------------------------------------------------------------------

// NOTE: System messages routed here update the **native system actors** (the
// mailbox-based coerce actors spawned by `start_system_actors()`). The runtime
// also keeps plain struct system actors (`self.watch_manager`, etc.) for the
// backward-compatible sync API. This dual-state pattern is intentional — see
// progress.md "Dual struct+actor pattern" design decision.

#[async_trait::async_trait]
impl dactor::system_router::SystemMessageRouter for CoerceRuntime {
    async fn route_system_envelope(
        &self,
        envelope: dactor::remote::WireEnvelope,
    ) -> Result<dactor::system_router::RoutingOutcome, dactor::system_router::RoutingError> {
        use dactor::system_actors::*;
        use dactor::system_router::{RoutingError, RoutingOutcome};

        dactor::system_router::validate_system_message_type(&envelope.message_type)?;

        let refs = self
            .system_actors
            .as_ref()
            .ok_or_else(|| RoutingError::new("system actors not started"))?;

        match envelope.message_type.as_str() {
            SYSTEM_MSG_TYPE_SPAWN => {
                let request = dactor::proto::decode_spawn_request(&envelope.body)
                    .map_err(|e| RoutingError::new(format!("decode SpawnRequest: {e}")))?;

                let req_id = request.request_id.clone();
                let outcome = refs
                    .spawn_manager()
                    .send(crate::system_actors::HandleSpawnRequest(request))
                    .await
                    .map_err(|e| RoutingError::new(format!("SpawnManager send: {e}")))?;

                match outcome {
                    crate::system_actors::SpawnOutcome::Success { actor_id, .. } => {
                        Ok(RoutingOutcome::SpawnCompleted {
                            request_id: req_id,
                            actor_id,
                        })
                    }
                    crate::system_actors::SpawnOutcome::Failure(SpawnResponse::Failure {
                        request_id,
                        error,
                    }) => Ok(RoutingOutcome::SpawnFailed { request_id, error }),
                    crate::system_actors::SpawnOutcome::Failure(SpawnResponse::Success {
                        ..
                    }) => {
                        unreachable!("SpawnOutcome::Failure always wraps SpawnResponse::Failure")
                    }
                }
            }

            SYSTEM_MSG_TYPE_WATCH => {
                let request = dactor::proto::decode_watch_request(&envelope.body)
                    .map_err(|e| RoutingError::new(format!("decode WatchRequest: {e}")))?;

                refs.watch_manager
                    .notify(crate::system_actors::RemoteWatch {
                        target: request.target,
                        watcher: request.watcher,
                    })
                    .map_err(|e| RoutingError::new(format!("WatchManager notify: {e}")))?;

                Ok(RoutingOutcome::Acknowledged)
            }

            SYSTEM_MSG_TYPE_UNWATCH => {
                let request = dactor::proto::decode_unwatch_request(&envelope.body)
                    .map_err(|e| RoutingError::new(format!("decode UnwatchRequest: {e}")))?;

                refs.watch_manager
                    .notify(crate::system_actors::RemoteUnwatch {
                        target: request.target,
                        watcher: request.watcher,
                    })
                    .map_err(|e| RoutingError::new(format!("WatchManager notify: {e}")))?;

                Ok(RoutingOutcome::Acknowledged)
            }

            SYSTEM_MSG_TYPE_CANCEL => {
                let request = dactor::proto::decode_cancel_request(&envelope.body)
                    .map_err(|e| RoutingError::new(format!("decode CancelRequest: {e}")))?;

                let request_id = request
                    .request_id
                    .ok_or_else(|| RoutingError::new("CancelRequest missing request_id"))?;

                let outcome = refs
                    .cancel_manager
                    .send(crate::system_actors::CancelById(request_id))
                    .await
                    .map_err(|e| RoutingError::new(format!("CancelManager send: {e}")))?;

                match outcome.0 {
                    CancelResponse::Acknowledged => Ok(RoutingOutcome::CancelAcknowledged),
                    CancelResponse::NotFound { reason } => {
                        Ok(RoutingOutcome::CancelNotFound { reason })
                    }
                }
            }

            SYSTEM_MSG_TYPE_CONNECT_PEER => {
                let (peer_id, address) = dactor::proto::decode_connect_peer(&envelope.body)
                    .map_err(|e| RoutingError::new(format!("decode ConnectPeer: {e}")))?;

                refs.node_directory
                    .notify(crate::system_actors::ConnectPeer {
                        peer_id,
                        address,
                    })
                    .map_err(|e| RoutingError::new(format!("NodeDirectory notify: {e}")))?;

                Ok(RoutingOutcome::Acknowledged)
            }

            SYSTEM_MSG_TYPE_DISCONNECT_PEER => {
                let peer_id = dactor::proto::decode_disconnect_peer(&envelope.body)
                    .map_err(|e| RoutingError::new(format!("decode DisconnectPeer: {e}")))?;

                refs.node_directory
                    .notify(crate::system_actors::DisconnectPeer(peer_id))
                    .map_err(|e| RoutingError::new(format!("NodeDirectory notify: {e}")))?;

                Ok(RoutingOutcome::Acknowledged)
            }

            other => Err(RoutingError::new(format!(
                "unhandled system message type: {other}"
            ))),
        }
    }
}

