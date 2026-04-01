use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Uniquely identifies a persistent actor's journal/snapshot in storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PersistenceId(pub String);

impl PersistenceId {
    pub fn new(entity_type: &str, entity_id: &str) -> Self {
        Self(format!("{entity_type}:{entity_id}"))
    }
}

impl std::fmt::Display for PersistenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Monotonically increasing event sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SequenceId(pub i64);

impl SequenceId {
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// A single journal entry (event).
#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub sequence_id: SequenceId,
    pub event_type: String,
    pub payload: Vec<u8>,
}

/// A single snapshot entry.
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    pub sequence_id: SequenceId,
    pub payload: Vec<u8>,
}

/// Errors from persistence operations.
#[derive(Debug)]
pub enum PersistError {
    StorageUnavailable(String),
    SerializationFailed(String),
    CorruptEntry {
        sequence_id: SequenceId,
        detail: String,
    },
    NotConfigured,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StorageUnavailable(msg) => write!(f, "storage unavailable: {}", msg),
            Self::SerializationFailed(msg) => write!(f, "serialization failed: {}", msg),
            Self::CorruptEntry {
                sequence_id,
                detail,
            } => write!(f, "corrupt entry at {:?}: {}", sequence_id, detail),
            Self::NotConfigured => write!(f, "no storage provider configured"),
        }
    }
}

impl std::error::Error for PersistError {}

/// What to do when recovery fails.
#[derive(Debug, Clone)]
pub enum RecoveryFailurePolicy {
    Stop,
    Retry {
        max_attempts: Option<u32>,
        initial_delay: Duration,
    },
    SkipAndStart,
}

impl Default for RecoveryFailurePolicy {
    fn default() -> Self {
        Self::Stop
    }
}

/// What to do when a persist operation fails.
#[derive(Debug, Clone)]
pub enum PersistFailurePolicy {
    Stop,
    ReturnError,
    Retry {
        max_attempts: Option<u32>,
        initial_delay: Duration,
    },
}

impl Default for PersistFailurePolicy {
    fn default() -> Self {
        Self::Stop
    }
}

/// Controls automatic snapshotting for event-sourced actors.
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    pub every_n_events: Option<u64>,
    pub interval: Option<Duration>,
    pub retention_count: Option<u32>,
    pub delete_events_on_snapshot: bool,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            every_n_events: None,
            interval: None,
            retention_count: None,
            delete_events_on_snapshot: false,
        }
    }
}

/// Controls automatic state saving for durable state actors.
#[derive(Debug, Clone)]
pub struct SaveConfig {
    pub every_n_messages: Option<u64>,
    pub interval: Option<Duration>,
}

impl Default for SaveConfig {
    fn default() -> Self {
        Self {
            every_n_messages: None,
            interval: None,
        }
    }
}

// ── Storage Traits ──────────────────────────────────

/// Pluggable journal storage for event-sourced actors.
#[async_trait::async_trait]
pub trait JournalStorage: Send + Sync + 'static {
    async fn write_event(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
        event_type: &str,
        payload: &[u8],
    ) -> Result<(), PersistError>;

    async fn write_event_batch(
        &self,
        persistence_id: &PersistenceId,
        entries: &[(SequenceId, &str, &[u8])],
    ) -> Result<(), PersistError>;

    async fn read_events(
        &self,
        persistence_id: &PersistenceId,
        from_sequence: SequenceId,
    ) -> Result<Vec<JournalEntry>, PersistError>;

    async fn read_highest_sequence(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<SequenceId>, PersistError>;

    async fn delete_events_to(
        &self,
        persistence_id: &PersistenceId,
        to_sequence: SequenceId,
    ) -> Result<(), PersistError>;
}

/// Pluggable snapshot storage.
#[async_trait::async_trait]
pub trait SnapshotStorage: Send + Sync + 'static {
    async fn save_snapshot(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
        payload: &[u8],
    ) -> Result<(), PersistError>;

    async fn load_latest_snapshot(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<SnapshotEntry>, PersistError>;

    async fn delete_snapshots_before(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
    ) -> Result<(), PersistError>;
}

/// Pluggable state store for durable state actors.
#[async_trait::async_trait]
pub trait StateStorage: Send + Sync + 'static {
    async fn save_state(
        &self,
        persistence_id: &PersistenceId,
        payload: &[u8],
    ) -> Result<(), PersistError>;

    async fn load_state(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<Vec<u8>>, PersistError>;

    async fn delete_state(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<(), PersistError>;
}

// ── In-Memory Storage (for testing) ─────────────────

/// In-memory storage implementation for testing.
/// Stores journal events, snapshots, and state in HashMaps.
pub struct InMemoryStorage {
    journals: Mutex<HashMap<String, Vec<JournalEntry>>>,
    snapshots: Mutex<HashMap<String, Vec<SnapshotEntry>>>,
    states: Mutex<HashMap<String, Vec<u8>>>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            journals: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            states: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl JournalStorage for InMemoryStorage {
    async fn write_event(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
        event_type: &str,
        payload: &[u8],
    ) -> Result<(), PersistError> {
        let mut journals = self.journals.lock().unwrap();
        let entries = journals.entry(persistence_id.0.clone()).or_default();
        entries.push(JournalEntry {
            sequence_id,
            event_type: event_type.to_string(),
            payload: payload.to_vec(),
        });
        Ok(())
    }

    async fn write_event_batch(
        &self,
        persistence_id: &PersistenceId,
        batch: &[(SequenceId, &str, &[u8])],
    ) -> Result<(), PersistError> {
        let mut journals = self.journals.lock().unwrap();
        let entries = journals.entry(persistence_id.0.clone()).or_default();
        for (seq, event_type, payload) in batch {
            entries.push(JournalEntry {
                sequence_id: *seq,
                event_type: event_type.to_string(),
                payload: payload.to_vec(),
            });
        }
        Ok(())
    }

    async fn read_events(
        &self,
        persistence_id: &PersistenceId,
        from_sequence: SequenceId,
    ) -> Result<Vec<JournalEntry>, PersistError> {
        let journals = self.journals.lock().unwrap();
        Ok(journals
            .get(&persistence_id.0)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e.sequence_id >= from_sequence)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn read_highest_sequence(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<SequenceId>, PersistError> {
        let journals = self.journals.lock().unwrap();
        Ok(journals
            .get(&persistence_id.0)
            .and_then(|entries| entries.last().map(|e| e.sequence_id)))
    }

    async fn delete_events_to(
        &self,
        persistence_id: &PersistenceId,
        to_sequence: SequenceId,
    ) -> Result<(), PersistError> {
        let mut journals = self.journals.lock().unwrap();
        if let Some(entries) = journals.get_mut(&persistence_id.0) {
            entries.retain(|e| e.sequence_id > to_sequence);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl SnapshotStorage for InMemoryStorage {
    async fn save_snapshot(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
        payload: &[u8],
    ) -> Result<(), PersistError> {
        let mut snapshots = self.snapshots.lock().unwrap();
        let entries = snapshots.entry(persistence_id.0.clone()).or_default();
        entries.push(SnapshotEntry {
            sequence_id,
            payload: payload.to_vec(),
        });
        Ok(())
    }

    async fn load_latest_snapshot(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<SnapshotEntry>, PersistError> {
        let snapshots = self.snapshots.lock().unwrap();
        Ok(snapshots
            .get(&persistence_id.0)
            .and_then(|entries| entries.last().cloned()))
    }

    async fn delete_snapshots_before(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
    ) -> Result<(), PersistError> {
        let mut snapshots = self.snapshots.lock().unwrap();
        if let Some(entries) = snapshots.get_mut(&persistence_id.0) {
            entries.retain(|e| e.sequence_id >= sequence_id);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl StateStorage for InMemoryStorage {
    async fn save_state(
        &self,
        persistence_id: &PersistenceId,
        payload: &[u8],
    ) -> Result<(), PersistError> {
        let mut states = self.states.lock().unwrap();
        states.insert(persistence_id.0.clone(), payload.to_vec());
        Ok(())
    }

    async fn load_state(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<Vec<u8>>, PersistError> {
        let states = self.states.lock().unwrap();
        Ok(states.get(&persistence_id.0).cloned())
    }

    async fn delete_state(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<(), PersistError> {
        let mut states = self.states.lock().unwrap();
        states.remove(&persistence_id.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persistence_id() {
        let id = PersistenceId::new("BankAccount", "acct-123");
        assert_eq!(id.0, "BankAccount:acct-123");
        assert_eq!(format!("{}", id), "BankAccount:acct-123");
    }

    #[test]
    fn test_sequence_id_ordering() {
        let s1 = SequenceId(1);
        let s2 = SequenceId(2);
        assert!(s1 < s2);
        assert_eq!(s1.next(), s2);
    }

    #[tokio::test]
    async fn test_journal_write_read() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage
            .write_event(&pid, SequenceId(1), "Created", b"data1")
            .await
            .unwrap();
        storage
            .write_event(&pid, SequenceId(2), "Updated", b"data2")
            .await
            .unwrap();

        let events = storage.read_events(&pid, SequenceId(1)).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "Created");
        assert_eq!(events[1].payload, b"data2");
    }

    #[tokio::test]
    async fn test_journal_read_from_sequence() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        for i in 1..=5 {
            storage
                .write_event(&pid, SequenceId(i), "Event", &[i as u8])
                .await
                .unwrap();
        }
        let events = storage.read_events(&pid, SequenceId(3)).await.unwrap();
        assert_eq!(events.len(), 3); // seq 3, 4, 5
    }

    #[tokio::test]
    async fn test_journal_highest_sequence() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        assert_eq!(storage.read_highest_sequence(&pid).await.unwrap(), None);
        storage
            .write_event(&pid, SequenceId(1), "E", b"")
            .await
            .unwrap();
        storage
            .write_event(&pid, SequenceId(5), "E", b"")
            .await
            .unwrap();
        assert_eq!(
            storage.read_highest_sequence(&pid).await.unwrap(),
            Some(SequenceId(5))
        );
    }

    #[tokio::test]
    async fn test_journal_delete_events() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        for i in 1..=5 {
            storage
                .write_event(&pid, SequenceId(i), "E", &[i as u8])
                .await
                .unwrap();
        }
        storage.delete_events_to(&pid, SequenceId(3)).await.unwrap();
        let events = storage.read_events(&pid, SequenceId(1)).await.unwrap();
        assert_eq!(events.len(), 2); // seq 4, 5
    }

    #[tokio::test]
    async fn test_journal_batch_write() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        let batch = vec![
            (SequenceId(1), "A", b"a" as &[u8]),
            (SequenceId(2), "B", b"b"),
            (SequenceId(3), "C", b"c"),
        ];
        storage.write_event_batch(&pid, &batch).await.unwrap();
        let events = storage.read_events(&pid, SequenceId(1)).await.unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn test_snapshot_save_load() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage
            .save_snapshot(&pid, SequenceId(10), b"state-at-10")
            .await
            .unwrap();
        let snap = storage.load_latest_snapshot(&pid).await.unwrap().unwrap();
        assert_eq!(snap.sequence_id, SequenceId(10));
        assert_eq!(snap.payload, b"state-at-10");
    }

    #[tokio::test]
    async fn test_snapshot_latest() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage
            .save_snapshot(&pid, SequenceId(5), b"old")
            .await
            .unwrap();
        storage
            .save_snapshot(&pid, SequenceId(10), b"new")
            .await
            .unwrap();
        let snap = storage.load_latest_snapshot(&pid).await.unwrap().unwrap();
        assert_eq!(snap.payload, b"new");
    }

    #[tokio::test]
    async fn test_snapshot_delete_before() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage
            .save_snapshot(&pid, SequenceId(5), b"s1")
            .await
            .unwrap();
        storage
            .save_snapshot(&pid, SequenceId(10), b"s2")
            .await
            .unwrap();
        storage
            .delete_snapshots_before(&pid, SequenceId(10))
            .await
            .unwrap();
        let snap = storage.load_latest_snapshot(&pid).await.unwrap().unwrap();
        assert_eq!(snap.payload, b"s2");
    }

    #[tokio::test]
    async fn test_state_save_load() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage.save_state(&pid, b"my-state").await.unwrap();
        let state = storage.load_state(&pid).await.unwrap().unwrap();
        assert_eq!(state, b"my-state");
    }

    #[tokio::test]
    async fn test_state_overwrite() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage.save_state(&pid, b"v1").await.unwrap();
        storage.save_state(&pid, b"v2").await.unwrap();
        let state = storage.load_state(&pid).await.unwrap().unwrap();
        assert_eq!(state, b"v2");
    }

    #[tokio::test]
    async fn test_state_delete() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "1");
        storage.save_state(&pid, b"data").await.unwrap();
        storage.delete_state(&pid).await.unwrap();
        assert!(storage.load_state(&pid).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_empty_reads() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Test", "nonexistent");
        assert!(storage
            .read_events(&pid, SequenceId(1))
            .await
            .unwrap()
            .is_empty());
        assert!(storage.load_latest_snapshot(&pid).await.unwrap().is_none());
        assert!(storage.load_state(&pid).await.unwrap().is_none());
    }

    #[test]
    fn test_persist_error_display() {
        let err = PersistError::StorageUnavailable("db down".into());
        assert!(format!("{}", err).contains("db down"));
    }

    #[test]
    fn test_recovery_failure_policy_default() {
        assert!(matches!(
            RecoveryFailurePolicy::default(),
            RecoveryFailurePolicy::Stop
        ));
    }

    #[test]
    fn test_snapshot_config_default() {
        let cfg = SnapshotConfig::default();
        assert!(cfg.every_n_events.is_none());
        assert!(!cfg.delete_events_on_snapshot);
    }
}
