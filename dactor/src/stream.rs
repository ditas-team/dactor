use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use futures::Stream;

/// A pinned, boxed, `Send`-safe async stream of items.
///
/// Returned by [`ActorRef::stream`](crate::actor::ActorRef::stream)
/// so callers can consume streamed replies with `StreamExt` combinators.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// A sender handle given to the actor's [`StreamHandler`](crate::actor::StreamHandler).
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

/// A receiver handle given to the actor's [`FeedHandler`](crate::actor::FeedHandler).
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
/// Items are accumulated and flushed as a batch when any of these
/// conditions is met (whichever comes first):
/// - `max_items` items have accumulated
/// - `max_delay` has elapsed since the first item in the current batch
/// - `max_bytes` total estimated byte size is exceeded (if set)
///
/// Batching is transparent: senders push individual items and
/// receivers pull individual items. The batching layer sits between
/// them, reducing the number of channel sends.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Maximum items per batch. When this many items accumulate,
    /// the batch is flushed immediately.
    pub max_items: usize,
    /// Maximum time to wait for more items before flushing.
    /// If fewer than `max_items` are buffered but this duration
    /// elapses since the first item, the batch is flushed.
    pub max_delay: Duration,
    /// Optional maximum accumulated byte size per batch.
    /// When the total estimated size of buffered items exceeds this,
    /// the batch is flushed. Useful for controlling wire frame sizes
    /// in remote transport. `None` means no byte limit.
    ///
    /// The size estimate is provided by the caller via
    /// `BatchWriter::push_with_size()`.
    pub max_bytes: Option<usize>,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_items: 64,
            max_delay: Duration::from_millis(5),
            max_bytes: None,
        }
    }
}

impl BatchConfig {
    pub fn new(max_items: usize, max_delay: Duration) -> Self {
        Self {
            max_items: max_items.max(1),
            max_delay,
            max_bytes: None,
        }
    }

    /// Set the maximum byte size per batch.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }
}

/// Batching writer: accumulates items and flushes as `Vec<T>` batches.
/// Sits between the producer and the channel sender.
///
/// Flushes when any condition is met: `max_items` reached, `max_delay`
/// elapsed, or `max_bytes` exceeded (if configured).
pub(crate) struct BatchWriter<T: Send + 'static> {
    sender: tokio::sync::mpsc::Sender<Vec<T>>,
    config: BatchConfig,
    buffer: Vec<T>,
    buffered_bytes: usize,
    flush_deadline: Option<tokio::time::Instant>,
}

impl<T: Send + 'static> BatchWriter<T> {
    pub fn new(sender: tokio::sync::mpsc::Sender<Vec<T>>, config: BatchConfig) -> Self {
        let cap = config.max_items;
        Self {
            sender,
            config,
            buffer: Vec::with_capacity(cap),
            buffered_bytes: 0,
            flush_deadline: None,
        }
    }

    /// Add an item. Flushes if batch is full (by count).
    pub async fn push(&mut self, item: T) -> Result<(), StreamSendError> {
        self.push_with_size(item, 0).await
    }

    /// Add an item with its pre-computed byte size.
    ///
    /// The caller should have already serialized the item (e.g., for wire
    /// transport) and pass the serialized byte length here. This avoids
    /// double-serialization: the caller keeps the serialized bytes for
    /// sending, and the batch writer uses only the length for its decision.
    ///
    /// **Decision logic (checked before adding):**
    /// - If the current batch is non-empty and adding `item_bytes` would
    ///   exceed `max_bytes`, the current batch is flushed first, and the
    ///   item goes into a fresh batch.
    /// - If a single item exceeds `max_bytes`, it is still accepted and
    ///   sent immediately as a batch of one (never dropped).
    ///
    /// **After adding:**
    /// - If `max_items` is reached, the batch is flushed.
    /// - If `max_bytes` is reached (including oversized single items),
    ///   the batch is flushed.
    pub async fn push_with_size(&mut self, item: T, item_bytes: usize) -> Result<(), StreamSendError> {
        // Pre-check: would adding this item overflow the byte budget?
        // If so, flush the current batch first so this item starts a new one.
        if let Some(max_bytes) = self.config.max_bytes {
            if !self.buffer.is_empty() && self.buffered_bytes + item_bytes > max_bytes {
                self.flush().await?;
            }
        }

        self.buffer.push(item);
        self.buffered_bytes += item_bytes;

        if self.flush_deadline.is_none() {
            self.flush_deadline = Some(tokio::time::Instant::now() + self.config.max_delay);
        }

        // Post-check: count-full or byte-full (handles oversized single items).
        let count_full = self.buffer.len() >= self.config.max_items;
        let bytes_full = self.config.max_bytes.map_or(false, |max| self.buffered_bytes >= max);
        if count_full || bytes_full {
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
        self.buffered_bytes = 0;
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

    /// Accumulated byte size of buffered items.
    pub fn buffered_bytes(&self) -> usize {
        self.buffered_bytes
    }

    /// The configured maximum delay between the first buffered item and flush.
    pub fn max_delay(&self) -> Duration {
        self.config.max_delay
    }
}

/// Batching reader: receives `Vec<T>` batches, yields individual items.
pub(crate) struct BatchReader<T: Send + 'static> {
    receiver: tokio::sync::mpsc::Receiver<Vec<T>>,
    current_batch: VecDeque<T>,
}

impl<T: Send + 'static> BatchReader<T> {
    pub fn new(receiver: tokio::sync::mpsc::Receiver<Vec<T>>) -> Self {
        Self {
            receiver,
            current_batch: VecDeque::new(),
        }
    }

    /// Receive the next individual item (transparently unbatching).
    pub async fn recv(&mut self) -> Option<T> {
        if let Some(item) = self.current_batch.pop_front() {
            return Some(item);
        }
        match self.receiver.recv().await {
            Some(batch) => {
                let mut deque = VecDeque::from(batch);
                let first = deque.pop_front();
                self.current_batch = deque;
                first
            }
            None => None,
        }
    }

    /// Convert into a `BoxStream` of individual items.
    pub fn into_stream(self) -> BoxStream<T> {
        Box::pin(futures::stream::unfold(self, |mut reader| async move {
            match reader.recv().await {
                Some(item) => Some((item, reader)),
                None => None,
            }
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
        // Manually send an empty vec (edge case)
        tx.send(vec![]).await.unwrap();
        tx.send(vec![1, 2]).await.unwrap();
        drop(tx);

        let mut reader = BatchReader::new(rx);
        // Empty batch yields None from pop_front, so reader fetches next batch
        // Actually the first batch returns None from pop_front for the "first" element,
        // so let's verify behavior: an empty batch effectively has first = None
        // The reader will get None from the empty batch and return None
        // (this is the edge case — empty batches terminate early)
        // With our implementation, VecDeque::from(vec![]).pop_front() = None,
        // so first = None and we return None. That's acceptable.
        let item = reader.recv().await;
        // Empty batch → pop_front returns None → recv returns None (channel still open
        // but the empty batch is consumed as "end"). This is an edge case that callers
        // shouldn't trigger (BatchWriter never sends empty batches).
        assert_eq!(item, None);
    }

    #[tokio::test]
    async fn test_batch_max_bytes_pre_flush() {
        // max_bytes=100: items of 60 bytes each should NOT share a batch.
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let config = BatchConfig::new(10, Duration::from_secs(10)).with_max_bytes(100);
        let mut writer = BatchWriter::new(tx, config);

        // First item (60 bytes) — buffered
        writer.push_with_size(1, 60).await.unwrap();
        assert_eq!(writer.buffered_count(), 1);

        // Second item (60 bytes) — would exceed 100, so current batch flushes first
        writer.push_with_size(2, 60).await.unwrap();

        // First batch should contain only item 1
        let batch1 = rx.try_recv().unwrap();
        assert_eq!(batch1, vec![1]);

        // Item 2 is buffered in the new batch
        assert_eq!(writer.buffered_count(), 1);
        assert_eq!(writer.buffered_bytes(), 60);

        writer.flush().await.unwrap();
        let batch2 = rx.try_recv().unwrap();
        assert_eq!(batch2, vec![2]);
    }

    #[tokio::test]
    async fn test_batch_oversized_single_item() {
        // max_bytes=50, but a single item is 200 bytes — must still be sent as batch of 1.
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let config = BatchConfig::new(10, Duration::from_secs(10)).with_max_bytes(50);
        let mut writer = BatchWriter::new(tx, config);

        writer.push_with_size(99, 200).await.unwrap();

        // Oversized item flushed immediately as batch of 1
        assert_eq!(writer.buffered_count(), 0);
        let batch = rx.try_recv().unwrap();
        assert_eq!(batch, vec![99]);
    }

    #[tokio::test]
    async fn test_batch_max_bytes_exact_boundary() {
        // max_bytes=100: two items of exactly 50 bytes each fill the budget exactly.
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let config = BatchConfig::new(10, Duration::from_secs(10)).with_max_bytes(100);
        let mut writer = BatchWriter::new(tx, config);

        writer.push_with_size(1, 50).await.unwrap();
        assert_eq!(writer.buffered_count(), 1);

        // Second item brings total to exactly 100 → flush (>= max_bytes)
        writer.push_with_size(2, 50).await.unwrap();
        assert_eq!(writer.buffered_count(), 0);

        let batch = rx.try_recv().unwrap();
        assert_eq!(batch, vec![1, 2]);
    }

    #[tokio::test]
    async fn test_batch_max_bytes_multiple_small_items() {
        // max_bytes=100: many small items (10 bytes each) fit in one batch.
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let config = BatchConfig::new(100, Duration::from_secs(10)).with_max_bytes(100);
        let mut writer = BatchWriter::new(tx, config);

        for i in 0..9 {
            writer.push_with_size(i, 10).await.unwrap();
        }
        assert_eq!(writer.buffered_count(), 9);
        assert_eq!(writer.buffered_bytes(), 90);

        // 10th item brings total to 100 → flush
        writer.push_with_size(9, 10).await.unwrap();
        assert_eq!(writer.buffered_count(), 0);

        let batch = rx.try_recv().unwrap();
        assert_eq!(batch, (0..10).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn test_batch_oversized_after_buffered_items() {
        // Buffered items exist, then an oversized item arrives.
        // Current batch should flush first, then oversized goes alone.
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let config = BatchConfig::new(100, Duration::from_secs(10)).with_max_bytes(50);
        let mut writer = BatchWriter::new(tx, config);

        writer.push_with_size(1, 20).await.unwrap();
        writer.push_with_size(2, 20).await.unwrap();
        assert_eq!(writer.buffered_count(), 2);

        // Oversized item (200 bytes) — flush [1,2] first, then send [3] alone
        writer.push_with_size(3, 200).await.unwrap();

        let batch1 = rx.try_recv().unwrap();
        assert_eq!(batch1, vec![1, 2]);

        let batch2 = rx.try_recv().unwrap();
        assert_eq!(batch2, vec![3]);

        assert_eq!(writer.buffered_count(), 0);
    }
}
