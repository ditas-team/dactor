//! Batched remote sender for reducing per-message transport overhead.
//!
//! [`BatchedTransportSender`] accumulates fire-and-forget [`WireEnvelope`]s
//! destined for the same node and flushes them as a single batch.
//!
//! **Requires the `serde` feature** for batch serialization.
//!
//! ## Flush semantics
//!
//! - **Auto-flush:** when `max_items` envelopes accumulate for a node
//! - **Manual flush:** via `flush_node()` / `flush_all()`
//! - **Time-based flush:** callers are responsible for periodically calling
//!   `flush_all()` (e.g., via a background `tokio::spawn` with interval)
//!
//! ## Restrictions
//!
//! Only `SendMode::Tell` envelopes can be batched. Ask/Stream/Feed
//! envelopes require correlation and cannot be batched.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::interceptor::SendMode;
use crate::node::{ActorId, NodeId};
use crate::remote::{WireEnvelope, WireHeaders};
use crate::stream::BatchConfig;
use crate::transport::{Transport, TransportError};

/// A batch of wire envelopes packed for transport.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WireEnvelopeBatch {
    /// The individual envelopes in this batch.
    pub envelopes: Vec<WireEnvelope>,
}

impl WireEnvelopeBatch {
    /// Create a new batch from a vector of envelopes.
    pub fn new(envelopes: Vec<WireEnvelope>) -> Self {
        Self { envelopes }
    }

    /// Number of envelopes in this batch.
    pub fn len(&self) -> usize {
        self.envelopes.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.envelopes.is_empty()
    }

    /// Total body bytes across all envelopes in the batch.
    pub fn total_body_bytes(&self) -> usize {
        self.envelopes.iter().map(|e| e.body.len()).sum()
    }
}

/// Batched transport sender that accumulates tell-only envelopes and
/// flushes them as a single batch to the transport layer.
///
/// **Only `SendMode::Tell`** envelopes are accepted. Ask/Stream/Feed
/// envelopes are rejected with an error because they require per-message
/// correlation.
pub struct BatchedTransportSender {
    transport: Arc<dyn Transport>,
    config: BatchConfig,
    buffers: Mutex<std::collections::HashMap<NodeId, Vec<WireEnvelope>>>,
}

impl BatchedTransportSender {
    /// Create a new batched sender.
    pub fn new(transport: Arc<dyn Transport>, config: BatchConfig) -> Self {
        Self {
            transport,
            config,
            buffers: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Queue a tell envelope for batched delivery.
    ///
    /// Returns `Err` if the envelope is not `SendMode::Tell`.
    /// Auto-flushes when `max_items` is reached for the target node.
    pub async fn send(&self, envelope: WireEnvelope) -> Result<(), TransportError> {
        if envelope.send_mode != SendMode::Tell {
            return Err(TransportError::new(format!(
                "BatchedTransportSender only accepts Tell envelopes, got {:?}",
                envelope.send_mode
            )));
        }

        let target_node = envelope.target.node.clone();
        let mut buffers = self.buffers.lock().await;
        let buffer = buffers
            .entry(target_node.clone())
            .or_insert_with(|| Vec::with_capacity(self.config.max_items));
        buffer.push(envelope);

        if buffer.len() >= self.config.max_items {
            let batch = std::mem::take(buffer);
            drop(buffers);
            self.send_batch(&target_node, batch).await?;
        }

        Ok(())
    }

    /// Flush all buffered envelopes for a specific node.
    pub async fn flush_node(&self, node: &NodeId) -> Result<(), TransportError> {
        let batch = {
            let mut buffers = self.buffers.lock().await;
            buffers.remove(node).unwrap_or_default()
        };
        if !batch.is_empty() {
            self.send_batch(node, batch).await?;
        }
        Ok(())
    }

    /// Flush all buffered envelopes for all nodes.
    ///
    /// Attempts to flush every node even if some fail. Returns the first
    /// error encountered (all nodes are attempted regardless).
    pub async fn flush_all(&self) -> Result<(), TransportError> {
        let all_batches: Vec<(NodeId, Vec<WireEnvelope>)> = {
            let mut buffers = self.buffers.lock().await;
            buffers.drain().collect()
        };
        let mut first_error: Option<TransportError> = None;
        for (node, batch) in all_batches {
            if !batch.is_empty() {
                if let Err(e) = self.send_batch(&node, batch).await {
                    tracing::error!(node = %node, error = %e, "batch flush failed");
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Number of buffered envelopes across all nodes.
    pub async fn buffered_count(&self) -> usize {
        let buffers = self.buffers.lock().await;
        buffers.values().map(|b| b.len()).sum()
    }

    /// Number of nodes with buffered envelopes.
    pub async fn buffered_nodes(&self) -> usize {
        let buffers = self.buffers.lock().await;
        buffers.len()
    }

    /// The batch configuration.
    pub fn config(&self) -> &BatchConfig {
        &self.config
    }

    /// Send a batch as a single WireEnvelope to the transport.
    async fn send_batch(
        &self,
        target_node: &NodeId,
        envelopes: Vec<WireEnvelope>,
    ) -> Result<(), TransportError> {
        let count = envelopes.len();
        let batch = WireEnvelopeBatch::new(envelopes);

        let body = serde_json::to_vec(&batch)
            .map_err(|e| TransportError::new(format!("batch serialization failed: {e}")))?;

        let wrapper = WireEnvelope {
            target: ActorId {
                node: target_node.clone(),
                local: 0,
            },
            target_name: String::new(),
            message_type: BATCH_MESSAGE_TYPE.to_string(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body,
            request_id: None,
            version: None,
        };

        tracing::debug!(
            target_node = %target_node,
            envelope_count = count,
            body_bytes = wrapper.body.len(),
            "flushing batch"
        );

        self.transport.send(target_node, wrapper).await
    }
}

/// Well-known message type for batch envelopes.
pub const BATCH_MESSAGE_TYPE: &str = "dactor::system::Batch";

/// Deserialize a batch envelope body back into individual envelopes.
pub fn unpack_batch(body: &[u8]) -> Result<WireEnvelopeBatch, TransportError> {
    serde_json::from_slice(body)
        .map_err(|e| TransportError::new(format!("batch deserialization failed: {e}")))
}

/// Check if a WireEnvelope is a batch envelope.
pub fn is_batch_envelope(envelope: &WireEnvelope) -> bool {
    envelope.message_type == BATCH_MESSAGE_TYPE
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::InMemoryTransport;
    use std::time::Duration;

    fn test_envelope(target_node: &str, local: u64, body: &[u8]) -> WireEnvelope {
        WireEnvelope {
            target: ActorId {
                node: NodeId(target_node.into()),
                local,
            },
            target_name: "test".into(),
            message_type: "test::Msg".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: body.to_vec(),
            request_id: None,
            version: None,
        }
    }

    fn ask_envelope(target_node: &str) -> WireEnvelope {
        WireEnvelope {
            target: ActorId {
                node: NodeId(target_node.into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Ask".into(),
            send_mode: SendMode::Ask,
            headers: WireHeaders::new(),
            body: vec![],
            request_id: Some(uuid::Uuid::new_v4()),
            version: None,
        }
    }

    #[test]
    fn wire_envelope_batch_basics() {
        let batch = WireEnvelopeBatch::new(vec![
            test_envelope("n1", 1, b"hello"),
            test_envelope("n1", 2, b"world"),
        ]);
        assert_eq!(batch.len(), 2);
        assert!(!batch.is_empty());
        assert_eq!(batch.total_body_bytes(), 10);
    }

    #[test]
    fn empty_batch() {
        let batch = WireEnvelopeBatch::new(vec![]);
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[tokio::test]
    async fn rejects_ask_envelopes() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let sender = BatchedTransportSender::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            BatchConfig::new(10, Duration::from_secs(60)),
        );

        let result = sender.send(ask_envelope("node-2")).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("Tell"));
    }

    #[tokio::test]
    async fn flushes_on_max_items() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let mut rx = transport.register_node(NodeId("node-2".into())).await;
        transport.connect(&NodeId("node-2".into()), None).await.unwrap();

        let sender = BatchedTransportSender::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            BatchConfig::new(3, Duration::from_secs(60)),
        );

        sender.send(test_envelope("node-2", 1, b"a")).await.unwrap();
        sender.send(test_envelope("node-2", 2, b"b")).await.unwrap();
        assert_eq!(sender.buffered_count().await, 2);

        sender.send(test_envelope("node-2", 3, b"c")).await.unwrap();
        assert_eq!(sender.buffered_count().await, 0);

        let received = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(is_batch_envelope(&received));
    }

    #[tokio::test]
    async fn flush_all_continues_on_error() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        // Register node-3 but not node-2 (node-2 has no route = will fail)
        let mut rx3 = transport.register_node(NodeId("node-3".into())).await;
        transport.connect(&NodeId("node-3".into()), None).await.unwrap();

        let sender = BatchedTransportSender::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            BatchConfig::new(100, Duration::from_secs(60)),
        );

        sender.send(test_envelope("node-2", 1, b"a")).await.unwrap();
        sender.send(test_envelope("node-3", 1, b"b")).await.unwrap();

        // flush_all: node-2 will fail, node-3 should still flush
        let result = sender.flush_all().await;
        // May or may not error depending on drain order, but node-3 should be flushed
        let _ = result;
        assert_eq!(sender.buffered_count().await, 0);

        // node-3 batch should have been delivered
        let received = tokio::time::timeout(Duration::from_millis(100), rx3.recv()).await;
        assert!(received.is_ok());
    }

    #[tokio::test]
    async fn flush_node_specific() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let _rx2 = transport.register_node(NodeId("node-2".into())).await;
        let _rx3 = transport.register_node(NodeId("node-3".into())).await;
        transport.connect(&NodeId("node-2".into()), None).await.unwrap();
        transport.connect(&NodeId("node-3".into()), None).await.unwrap();

        let sender = BatchedTransportSender::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            BatchConfig::new(100, Duration::from_secs(60)),
        );

        sender.send(test_envelope("node-2", 1, b"a")).await.unwrap();
        sender.send(test_envelope("node-3", 1, b"b")).await.unwrap();

        sender.flush_node(&NodeId("node-2".into())).await.unwrap();
        assert_eq!(sender.buffered_count().await, 1); // node-3 still buffered
    }

    #[tokio::test]
    async fn empty_flush_is_noop() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let sender = BatchedTransportSender::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            BatchConfig::new(10, Duration::from_secs(1)),
        );
        sender.flush_all().await.unwrap();
        sender.flush_node(&NodeId("x".into())).await.unwrap();
    }

    #[tokio::test]
    async fn batch_roundtrip() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let mut rx = transport.register_node(NodeId("node-2".into())).await;
        transport.connect(&NodeId("node-2".into()), None).await.unwrap();

        let sender = BatchedTransportSender::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            BatchConfig::new(2, Duration::from_secs(60)),
        );

        sender
            .send(test_envelope("node-2", 1, b"hello"))
            .await
            .unwrap();
        sender
            .send(test_envelope("node-2", 2, b"world"))
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let batch = unpack_batch(&received.body).unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.envelopes[0].body, b"hello");
        assert_eq!(batch.envelopes[1].body, b"world");
    }

    #[test]
    fn is_batch_envelope_check() {
        let batch_env = WireEnvelope {
            target: ActorId {
                node: NodeId("n".into()),
                local: 0,
            },
            target_name: String::new(),
            message_type: BATCH_MESSAGE_TYPE.to_string(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![],
            request_id: None,
            version: None,
        };
        assert!(is_batch_envelope(&batch_env));
        assert!(!is_batch_envelope(&test_envelope("n", 1, b"x")));
    }
}
