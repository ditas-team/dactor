//! Shared runtime helpers for adapter implementations.
//!
//! Extracts common patterns (outbound interceptor pipeline, stream output
//! interception, feed drain tasks) so adapter crates don't duplicate code.

use std::any::Any;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::interceptor::{
    intercept_outbound_stream_item, notify_drop, Disposition, DropNotice, DropObserver,
    InterceptResult, OutboundContext, OutboundInterceptor, Outcome, SendMode,
};
use crate::message::{Headers, RuntimeHeaders};
use crate::node::ActorId;
use crate::errors::ActorSendError;
use crate::mailbox::OverflowStrategy;
use crate::stream::{BatchConfig, BatchReader, BatchWriter, BoxStream};

// ---------------------------------------------------------------------------
// BoundedMailboxSender
// ---------------------------------------------------------------------------

/// A bounded `mpsc` channel sender with configurable overflow strategy.
///
/// Used by adapter crates to place a capacity-limited front-buffer in front
/// of the underlying actor's unbounded mailbox.  The adapter spawns a
/// forwarding task that drains this channel and delivers messages to the
/// real actor.
///
/// Generic over the message type `T` (typically `DactorMsg<A>`).
pub struct BoundedMailboxSender<T: Send + 'static> {
    tx: tokio::sync::mpsc::Sender<T>,
    overflow: OverflowStrategy,
}

impl<T: Send + 'static> Clone for BoundedMailboxSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            overflow: self.overflow,
        }
    }
}

impl<T: Send + 'static> BoundedMailboxSender<T> {
    /// Create a new bounded mailbox sender.
    pub fn new(tx: tokio::sync::mpsc::Sender<T>, overflow: OverflowStrategy) -> Self {
        Self { tx, overflow }
    }

    /// Try to send a message, respecting the configured overflow strategy.
    pub fn try_send(&self, msg: T) -> Result<(), ActorSendError> {
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => match self.overflow {
                OverflowStrategy::RejectWithError => {
                    Err(ActorSendError("mailbox full".into()))
                }
                OverflowStrategy::DropNewest => Ok(()),
                OverflowStrategy::Block => Err(ActorSendError(
                    "mailbox full (Block not supported in sync tell)".into(),
                )),
            },
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(ActorSendError("actor stopped".into()))
            }
        }
    }

    /// Check if the underlying channel is closed.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Approximate number of messages pending in the bounded channel.
    pub fn pending(&self) -> usize {
        self.tx.max_capacity() - self.tx.capacity()
    }
}

// ---------------------------------------------------------------------------
// OutboundPipeline
// ---------------------------------------------------------------------------

/// Shared context for outbound pipeline operations.
/// Bundle this once per ActorRef and pass to all helper functions.
#[derive(Clone)]
pub struct OutboundPipeline {
    /// The outbound interceptors to run.
    pub interceptors: Arc<Vec<Box<dyn OutboundInterceptor>>>,
    /// Optional observer notified when items are dropped.
    pub drop_observer: Option<Arc<dyn DropObserver>>,
    /// Identity of the target actor.
    pub target_id: ActorId,
    /// Name of the target actor.
    pub target_name: String,
}

impl OutboundPipeline {
    /// Run the outbound `on_send` interceptor pipeline for a message.
    /// Returns the first non-Continue disposition with the interceptor name.
    pub fn run_on_send<M: 'static>(&self, send_mode: SendMode, msg: &M) -> InterceptResult {
        let runtime_headers = RuntimeHeaders::new();
        let mut headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.target_id.clone(),
            target_name: &self.target_name,
            message_type: std::any::type_name::<M>(),
            send_mode,
            remote: false,
        };
        for interceptor in self.interceptors.iter() {
            let d = interceptor.on_send(&octx, &runtime_headers, &mut headers, msg as &dyn Any);
            if !matches!(d, Disposition::Continue) {
                let interception_result = InterceptResult {
                    disposition: d,
                    interceptor_name: interceptor.name(),
                };
                if matches!(interception_result.disposition, Disposition::Drop) {
                    notify_drop(
                        &self.drop_observer.clone(),
                        DropNotice {
                            target_name: self.target_name.clone(),
                            message_type: std::any::type_name::<M>(),
                            interceptor_name: interception_result.interceptor_name,
                            send_mode,
                            context: "outbound on_send",
                            seq: None,
                        },
                    );
                }
                return interception_result;
            }
        }
        InterceptResult::continued()
    }

    fn notify_item_drop(
        &self,
        message_type: &'static str,
        send_mode: SendMode,
        context: &'static str,
        interceptor_name: &'static str,
        seq: u64,
    ) {
        notify_drop(
            &self.drop_observer,
            DropNotice {
                target_name: self.target_name.clone(),
                message_type,
                interceptor_name,
                send_mode,
                context,
                seq: Some(seq),
            },
        );
    }

    /// Run `on_reply` on all outbound interceptors.
    ///
    /// Called by runtimes after an ask() reply is received, so that outbound
    /// interceptors can observe (log, measure, audit) the reply on the sender
    /// side.
    pub fn run_on_reply(&self, message_type: &'static str, outcome: &Outcome<'_>) {
        if self.interceptors.is_empty() {
            return;
        }
        let runtime_headers = RuntimeHeaders::new();
        let headers = Headers::new();
        let octx = OutboundContext {
            target_id: self.target_id.clone(),
            target_name: &self.target_name,
            message_type,
            send_mode: SendMode::Ask,
            remote: false,
        };
        for interceptor in self.interceptors.iter() {
            interceptor.on_reply(&octx, &runtime_headers, &headers, outcome);
        }
    }
}

/// Wrap a stream with per-item outbound interception.
/// Each item goes through `on_expand_item` — Drop skips (notifies observer),
/// Delay sleeps, Reject terminates.
pub fn wrap_stream_with_interception<T: Send + 'static>(
    rx: tokio::sync::mpsc::Receiver<T>,
    buffer: usize,
    pipeline: OutboundPipeline,
    message_type: &'static str,
    send_mode: SendMode,
) -> BoxStream<T> {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<T>(buffer);
    tokio::spawn(async move {
        let mut stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let mut seq: u64 = 0;
        let item_headers = Headers::new();
        while let Some(item) = stream.next().await {
            let octx = OutboundContext {
                target_id: pipeline.target_id.clone(),
                target_name: &pipeline.target_name,
                message_type,
                send_mode,
                remote: false,
            };
            let interception_result = intercept_outbound_stream_item(
                &pipeline.interceptors,
                &octx,
                &item_headers,
                seq,
                &item as &dyn Any,
            );
            seq += 1;
            match interception_result.disposition {
                Disposition::Continue => {
                    if out_tx.send(item).await.is_err() {
                        break;
                    }
                }
                Disposition::Drop | Disposition::Retry(_) => {
                    pipeline.notify_item_drop(
                        message_type,
                        SendMode::Expand,
                        "stream reply",
                        interception_result.interceptor_name,
                        seq - 1,
                    );
                    continue;
                }
                Disposition::Delay(d) => {
                    tokio::time::sleep(d).await;
                    if out_tx.send(item).await.is_err() {
                        break;
                    }
                }
                Disposition::Reject(_) => break,
            }
        }
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(out_rx))
}

/// Wrap a batched stream (from BatchReader) with per-item outbound interception.
pub fn wrap_batched_stream_with_interception<T: Send + 'static>(
    reader: BatchReader<T>,
    buffer: usize,
    pipeline: OutboundPipeline,
    message_type: &'static str,
    send_mode: SendMode,
) -> BoxStream<T> {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<T>(buffer);
    tokio::spawn(async move {
        let mut stream = reader.into_stream();
        let mut seq: u64 = 0;
        let item_headers = Headers::new();
        while let Some(item) = stream.next().await {
            let octx = OutboundContext {
                target_id: pipeline.target_id.clone(),
                target_name: &pipeline.target_name,
                message_type,
                send_mode,
                remote: false,
            };
            let interception_result = intercept_outbound_stream_item(
                &pipeline.interceptors,
                &octx,
                &item_headers,
                seq,
                &item as &dyn Any,
            );
            seq += 1;
            match interception_result.disposition {
                Disposition::Continue => {
                    if out_tx.send(item).await.is_err() {
                        break;
                    }
                }
                Disposition::Drop | Disposition::Retry(_) => {
                    pipeline.notify_item_drop(
                        message_type,
                        send_mode,
                        "stream reply (batched)",
                        interception_result.interceptor_name,
                        seq - 1,
                    );
                    continue;
                }
                Disposition::Delay(d) => {
                    tokio::time::sleep(d).await;
                    if out_tx.send(item).await.is_err() {
                        break;
                    }
                }
                Disposition::Reject(_) => break,
            }
        }
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(out_rx))
}

/// Spawn the feed drain task: reads items from the input stream, runs
/// per-item outbound interception, and forwards to the actor's item channel.
pub fn spawn_reduce_drain<T: Send + 'static>(
    input: BoxStream<T>,
    item_tx: tokio::sync::mpsc::Sender<T>,
    cancel: Option<CancellationToken>,
    pipeline: OutboundPipeline,
    message_type: &'static str,
) {
    tokio::spawn(async move {
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
                            target_id: pipeline.target_id.clone(),
                            target_name: &pipeline.target_name,
                            message_type,
                            send_mode: SendMode::Reduce,
                            remote: false,
                        };
                        let interception_result = intercept_outbound_stream_item(
                            &pipeline.interceptors,
                            &octx,
                            &item_headers,
                            seq,
                            &item as &dyn Any,
                        );
                        seq += 1;
                        match interception_result.disposition {
                            Disposition::Continue => {
                                if item_tx.send(item).await.is_err() {
                                    break;
                                }
                            }
                            Disposition::Drop | Disposition::Retry(_) => {
                                pipeline.notify_item_drop(
                                    message_type,
                                    SendMode::Reduce,
                                    "feed item",
                                    interception_result.interceptor_name,
                                    seq - 1,
                                );
                                continue;
                            }
                            Disposition::Delay(d) => {
                                tokio::time::sleep(d).await;
                                if item_tx.send(item).await.is_err() {
                                    break;
                                }
                            }
                            Disposition::Reject(_) => break,
                        }
                    }
                    None => break,
                }
            }
        })
        .catch_unwind()
        .await;

        if result.is_err() {
            tracing::error!("feed drain task panicked — input stream dropped");
        }
    });
}

/// Spawn the batched feed drain: intercept → batch → unbatch → actor.
/// Returns immediately; the drain runs in the background.
pub fn spawn_reduce_batched_drain<T: Send + 'static>(
    input: BoxStream<T>,
    item_tx: tokio::sync::mpsc::Sender<T>,
    buffer: usize,
    batch_config: BatchConfig,
    cancel: Option<CancellationToken>,
    pipeline: OutboundPipeline,
    message_type: &'static str,
) {
    tokio::spawn(async move {
        // Stage 1: intercept each input item
        let (intercepted_tx, intercepted_rx) = tokio::sync::mpsc::channel::<T>(buffer);
        let intercept_handle = tokio::spawn({
            let cancel = cancel.clone();
            let pipeline = pipeline.clone();
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
                                target_id: pipeline.target_id.clone(),
                                target_name: &pipeline.target_name,
                                message_type,
                                send_mode: SendMode::Reduce,
                                remote: false,
                            };
                            let interception_result = intercept_outbound_stream_item(
                                &pipeline.interceptors,
                                &octx,
                                &item_headers,
                                seq,
                                &item as &dyn Any,
                            );
                            seq += 1;
                            match interception_result.disposition {
                                Disposition::Continue => {
                                    if intercepted_tx.send(item).await.is_err() {
                                        break;
                                    }
                                }
                                Disposition::Drop | Disposition::Retry(_) => {
                                    pipeline.notify_item_drop(
                                        message_type,
                                        SendMode::Reduce,
                                        "feed item (batched)",
                                        interception_result.interceptor_name,
                                        seq - 1,
                                    );
                                    continue;
                                }
                                Disposition::Delay(d) => {
                                    tokio::time::sleep(d).await;
                                    if intercepted_tx.send(item).await.is_err() {
                                        break;
                                    }
                                }
                                Disposition::Reject(_) => break,
                            }
                        }
                        None => break,
                    }
                }
            }
        });

        // Stage 2: batch intercepted items
        let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<Vec<T>>(buffer);
        let writer_handle = tokio::spawn(async move {
            let mut intercepted_rx = intercepted_rx;
            let mut writer = BatchWriter::new(batch_tx, batch_config);
            let result = std::panic::AssertUnwindSafe(async {
                loop {
                    if writer.buffered_count() > 0 {
                        // Use the writer's own flush deadline (set when first item was buffered)
                        let delay = writer.max_delay();
                        tokio::select! {
                            biased;
                            item = intercepted_rx.recv() => match item {
                                Some(item) => {
                                    if writer.push(item).await.is_err() { break; }
                                }
                                None => break,
                            },
                            _ = tokio::time::sleep(delay) => {
                                if writer.check_deadline().await.is_err() { break; }
                            }
                        }
                    } else {
                        match intercepted_rx.recv().await {
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
            })
            .catch_unwind()
            .await;
            if result.is_err() {
                tracing::error!("reduce_batched writer task panicked");
            }
        });

        // Stage 3: unbatch and forward to actor.
        // Drop batch_rx before awaiting handles to prevent deadlock:
        // if item_tx.send fails (actor stopped), we must close batch_rx
        // so the writer task's batch_tx.send unblocks and terminates.
        let mut send_failed = false;
        while let Some(batch) = batch_rx.recv().await {
            for item in batch {
                if item_tx.send(item).await.is_err() {
                    send_failed = true;
                    break;
                }
            }
            if send_failed {
                break;
            }
        }
        drop(batch_rx);
        drop(item_tx);

        let _ = writer_handle.await;
        let _ = intercept_handle.await;
    });
}

/// Spawn the transform input drain task: reads items from the input stream,
/// runs per-item outbound interception, and forwards to the actor's item channel.
pub fn spawn_transform_drain<T: Send + 'static>(
    input: BoxStream<T>,
    item_tx: tokio::sync::mpsc::Sender<T>,
    cancel: Option<CancellationToken>,
    pipeline: OutboundPipeline,
    message_type: &'static str,
) {
    tokio::spawn(async move {
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
                            target_id: pipeline.target_id.clone(),
                            target_name: &pipeline.target_name,
                            message_type,
                            send_mode: SendMode::Transform,
                            remote: false,
                        };
                        let interception_result = intercept_outbound_stream_item(
                            &pipeline.interceptors,
                            &octx,
                            &item_headers,
                            seq,
                            &item as &dyn Any,
                        );
                        seq += 1;
                        match interception_result.disposition {
                            Disposition::Continue => {
                                if item_tx.send(item).await.is_err() {
                                    break;
                                }
                            }
                            Disposition::Drop | Disposition::Retry(_) => {
                                pipeline.notify_item_drop(
                                    message_type,
                                    SendMode::Transform,
                                    "transform input item",
                                    interception_result.interceptor_name,
                                    seq - 1,
                                );
                                continue;
                            }
                            Disposition::Delay(d) => {
                                tokio::time::sleep(d).await;
                                if item_tx.send(item).await.is_err() {
                                    break;
                                }
                            }
                            Disposition::Reject(_) => break,
                        }
                    }
                    None => break,
                }
            }
        })
        .catch_unwind()
        .await;

        if result.is_err() {
            tracing::error!("transform drain task panicked — input stream dropped");
        }
    });
}
