use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use futures::Stream;

/// A pinned, boxed, `Send`-safe async stream of items.
///
/// Returned by [`ActorRef::expand`](crate::actor::ActorRef::expand)
/// so callers can consume streamed replies with `StreamExt` combinators.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// A sender handle given to the actor's [`ExpandHandler`](crate::actor::ExpandHandler).
///
/// The actor pushes items into this sender; the caller receives them
/// as a [`BoxStream`] on the other end. When the handler returns,
/// the stream is automatically closed.
pub struct StreamSender<T: Send + 'static> {
    inner: tokio::sync::mpsc::Sender<T>,
}

impl<T: Send + 'static> StreamSender<T> {
    /// Create a new StreamSender wrapping a tokio mpsc sender.
    pub fn new(inner: tokio::sync::mpsc::Sender<T>) -> Self {
        Self { inner }
    }

    /// Send an item to the stream consumer.
    /// Returns Err if the consumer has dropped the stream.
    pub async fn send(&self, item: T) -> Result<(), StreamSendError> {
        self.inner
            .send(item)
            .await
            .map_err(|_| StreamSendError::ConsumerDropped)
    }

    /// Try to send an item without blocking.
    ///
    /// Returns `Err(StreamSendError::Full)` if the channel buffer is at
    /// capacity, or `Err(StreamSendError::ConsumerDropped)` if the
    /// consumer has disconnected.
    pub fn try_send(&self, item: T) -> Result<(), StreamSendError> {
        self.inner.try_send(item).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => StreamSendError::Full,
            tokio::sync::mpsc::error::TrySendError::Closed(_) => StreamSendError::ConsumerDropped,
        })
    }

    /// Check if the consumer has dropped the receiving stream.
    ///
    /// **Note:** This is a point-in-time check — the consumer could drop
    /// between this call and a subsequent `send()`. Prefer checking the
    /// `send()` result for reliable termination detection. Use `is_closed()`
    /// only as a hint for early exit in long-running handlers.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

/// Errors from [`StreamSender`] send operations.
///
/// Indicates either backpressure (buffer full) or that the consumer
/// has disconnected and will never read further items.
#[derive(Debug)]
pub enum StreamSendError {
    /// The consumer dropped the stream (no longer reading).
    ConsumerDropped,
    /// The channel buffer is full (backpressure).
    Full,
}

impl std::fmt::Display for StreamSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConsumerDropped => write!(f, "stream consumer dropped"),
            Self::Full => write!(f, "stream buffer full"),
        }
    }
}

impl std::error::Error for StreamSendError {}

/// A receiver handle given to the actor's [`ReduceHandler`](crate::actor::ReduceHandler).
///
/// The caller pushes items from a `BoxStream` into the channel; the actor
/// consumes them through this receiver. Can be used directly with `recv()`
/// or converted into a `BoxStream` via `into_stream()`.
pub struct StreamReceiver<T: Send + 'static> {
    inner: tokio::sync::mpsc::Receiver<T>,
}

impl<T: Send + 'static> StreamReceiver<T> {
    /// Create a new StreamReceiver wrapping a tokio mpsc receiver.
    pub fn new(inner: tokio::sync::mpsc::Receiver<T>) -> Self {
        Self { inner }
    }

    /// Receive the next item, or `None` when the sender is closed.
    pub async fn recv(&mut self) -> Option<T> {
        self.inner.recv().await
    }

    /// Convert into a `BoxStream` for use with `StreamExt` combinators.
    pub fn into_stream(self) -> BoxStream<T> {
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(self.inner))
    }
}

// ---------------------------------------------------------------------------
// Batching primitives
// ---------------------------------------------------------------------------

/// Controls automatic batching for stream and feed channels.
/// Items are accumulated and flushed as a batch when either condition
/// is met (whichever comes first):
/// - `max_items` items have accumulated
/// - `max_delay` has elapsed since the first item in the current batch
///
/// A batched list of items will be wrapped into a single WireEnvelope
/// at the transport layer, sharing the same MessageId and headers.
/// Byte-level concerns (frame size limits, throttling) are handled by
/// the transport layer, not the batch writer.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Maximum items per batch. When this many items accumulate,
    /// the batch is flushed immediately.
    pub max_items: usize,
    /// Maximum time to wait for more items before flushing.
    /// If fewer than `max_items` are buffered but this duration
    /// elapses since the first item, the batch is flushed.
    pub max_delay: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_items: 64,
            max_delay: Duration::from_millis(5),
        }
    }
}

impl BatchConfig {
    /// Create a new batch configuration with the given limits.
    pub fn new(max_items: usize, max_delay: Duration) -> Self {
        Self {
            max_items: max_items.max(1),
            max_delay,
        }
    }
}

/// Batching writer: accumulates items and flushes as `Vec<T>` batches.
///
/// Flushes when `max_items` is reached or `max_delay` elapses.
/// The flushed batch (a `Vec<T>`) will be wrapped into a single
/// WireEnvelope by the transport layer for remote delivery.
pub struct BatchWriter<T: Send + 'static> {
    sender: tokio::sync::mpsc::Sender<Vec<T>>,
    config: BatchConfig,
    buffer: Vec<T>,
    flush_deadline: Option<tokio::time::Instant>,
}

impl<T: Send + 'static> BatchWriter<T> {
    /// Create a new batch writer with the given sender and configuration.
    pub fn new(sender: tokio::sync::mpsc::Sender<Vec<T>>, config: BatchConfig) -> Self {
        let cap = config.max_items;
        Self {
            sender,
            config,
            buffer: Vec::with_capacity(cap),
            flush_deadline: None,
        }
    }

    /// Add an item to the batch. Flushes when `max_items` is reached.
    pub async fn push(&mut self, item: T) -> Result<(), StreamSendError> {
        self.buffer.push(item);

        if self.flush_deadline.is_none() {
            self.flush_deadline = Some(tokio::time::Instant::now() + self.config.max_delay);
        }

        if self.buffer.len() >= self.config.max_items {
            self.flush().await?;
        }
        Ok(())
    }

    /// Flush the current batch.
    pub async fn flush(&mut self) -> Result<(), StreamSendError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.buffer);
        self.buffer = Vec::with_capacity(self.config.max_items);
        self.flush_deadline = None;
        self.sender
            .send(batch)
            .await
            .map_err(|_| StreamSendError::ConsumerDropped)
    }

    /// Check if the flush deadline has passed and flush if so.
    pub async fn check_deadline(&mut self) -> Result<(), StreamSendError> {
        if let Some(deadline) = self.flush_deadline {
            if tokio::time::Instant::now() >= deadline {
                self.flush().await?;
            }
        }
        Ok(())
    }

    /// Remaining items in buffer.
    pub fn buffered_count(&self) -> usize {
        self.buffer.len()
    }

    /// The configured maximum delay between the first buffered item and flush.
    pub fn max_delay(&self) -> Duration {
        self.config.max_delay
    }
}

/// Batching reader: receives `Vec<T>` batches, yields individual items.
pub struct BatchReader<T: Send + 'static> {
    receiver: tokio::sync::mpsc::Receiver<Vec<T>>,
    current_batch: VecDeque<T>,
}

impl<T: Send + 'static> BatchReader<T> {
    /// Create a new batch reader from a receiver of batches.
    pub fn new(receiver: tokio::sync::mpsc::Receiver<Vec<T>>) -> Self {
        Self {
            receiver,
            current_batch: VecDeque::new(),
        }
    }

    /// Receive the next individual item (transparently unbatching).
    /// Empty batches are skipped automatically.
    pub async fn recv(&mut self) -> Option<T> {
        loop {
            if let Some(item) = self.current_batch.pop_front() {
                return Some(item);
            }
            match self.receiver.recv().await {
                Some(batch) => {
                    self.current_batch = VecDeque::from(batch);
                    // Loop back to pop_front — skips empty batches
                }
                None => return None,
            }
        }
    }

    /// Convert into a `BoxStream` of individual items.
    pub fn into_stream(self) -> BoxStream<T> {
        Box::pin(futures::stream::unfold(self, |mut reader| async move {
            reader.recv().await.map(|item| (item, reader))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn test_batch_config_default() {
        let config = BatchConfig::default();
        assert_eq!(config.max_items, 64);
        assert_eq!(config.max_delay, Duration::from_millis(5));
    }

    #[tokio::test]
    async fn test_batch_config_clamps_to_one() {
        let config = BatchConfig::new(0, Duration::from_millis(1));
        assert_eq!(config.max_items, 1);
    }

    #[tokio::test]
    async fn test_batch_writer_reader_roundtrip() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut writer = BatchWriter::new(tx, BatchConfig::new(3, Duration::from_secs(10)));
        let mut reader = BatchReader::new(rx);

        // Push 3 items → triggers flush (batch full)
        writer.push(1).await.unwrap();
        writer.push(2).await.unwrap();
        writer.push(3).await.unwrap();

        assert_eq!(reader.recv().await, Some(1));
        assert_eq!(reader.recv().await, Some(2));
        assert_eq!(reader.recv().await, Some(3));
    }

    #[tokio::test]
    async fn test_batch_writer_flush_explicit() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut writer = BatchWriter::new(tx, BatchConfig::new(100, Duration::from_secs(10)));
        let mut reader = BatchReader::new(rx);

        writer.push(42).await.unwrap();
        assert_eq!(writer.buffered_count(), 1);

        writer.flush().await.unwrap();
        assert_eq!(writer.buffered_count(), 0);

        assert_eq!(reader.recv().await, Some(42));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_batch_writer_flush_on_deadline() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut writer = BatchWriter::new(tx, BatchConfig::new(100, Duration::from_millis(10)));
        let mut reader = BatchReader::new(rx);

        writer.push(42).await.unwrap();
        assert_eq!(writer.buffered_count(), 1);

        // Advance time past deadline
        tokio::time::sleep(Duration::from_millis(20)).await;
        writer.check_deadline().await.unwrap();

        assert_eq!(reader.recv().await, Some(42));
    }

    #[tokio::test]
    async fn test_batch_reader_as_stream() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut writer = BatchWriter::new(tx, BatchConfig::new(2, Duration::from_secs(10)));

        writer.push(10).await.unwrap();
        writer.push(20).await.unwrap(); // flush (batch=2)
        writer.push(30).await.unwrap();
        writer.push(40).await.unwrap(); // flush
        drop(writer); // close channel

        let reader = BatchReader::new(rx);
        let items: Vec<i32> = reader.into_stream().collect().await;
        assert_eq!(items, vec![10, 20, 30, 40]);
    }

    #[tokio::test]
    async fn test_batch_empty_flush_is_noop() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<i32>>(16);
        let mut writer = BatchWriter::new(tx, BatchConfig::default());
        // Flushing empty buffer should succeed
        writer.flush().await.unwrap();
        assert_eq!(writer.buffered_count(), 0);
    }

    #[tokio::test]
    async fn test_batch_writer_consumer_dropped() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let mut writer = BatchWriter::new(tx, BatchConfig::new(2, Duration::from_secs(10)));
        drop(rx); // consumer dropped

        writer.push(1).await.unwrap(); // buffered, not flushed yet
        let err = writer.push(2).await; // triggers flush → error
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_batch_reader_empty_batch_skipped() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        // Send an empty batch followed by a real batch
        tx.send(vec![]).await.unwrap();
        tx.send(vec![1, 2]).await.unwrap();
        drop(tx);

        let mut reader = BatchReader::new(rx);
        // Empty batch is skipped, reader proceeds to next batch
        assert_eq!(reader.recv().await, Some(1));
        assert_eq!(reader.recv().await, Some(2));
        assert_eq!(reader.recv().await, None); // channel closed
    }
}
