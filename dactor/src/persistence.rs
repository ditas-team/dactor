use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::actor::Actor;

/// Uniquely identifies a persistent actor's journal/snapshot in storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PersistenceId(pub String);

impl PersistenceId {
    /// Create a persistence ID from entity type and entity ID (joined by `:`).
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
    /// Return the next sequence ID.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// A single journal entry (event).
#[derive(Debug, Clone)]
pub struct JournalEntry {
    /// Sequence number of this event.
    pub sequence_id: SequenceId,
    /// Type name of the event (for deserialization dispatch).
    pub event_type: String,
    /// Serialized event payload.
    pub payload: Vec<u8>,
}

/// A single snapshot entry.
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    /// Sequence number at which the snapshot was taken.
    pub sequence_id: SequenceId,
    /// Serialized snapshot payload.
    pub payload: Vec<u8>,
}

/// Errors from persistence operations.
#[derive(Debug)]
pub enum PersistError {
    /// The backing store is unreachable or returned an error.
    StorageUnavailable(String),
    /// Serialization or deserialization of the payload failed.
    SerializationFailed(String),
    /// A stored entry is corrupt or cannot be decoded.
    CorruptEntry {
        /// Sequence number of the corrupt entry.
        sequence_id: SequenceId,
        /// Description of the corruption.
        detail: String,
    },
    /// No storage provider has been configured.
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
#[derive(Default)]
pub enum RecoveryFailurePolicy {
    /// Stop the actor on recovery failure (default).
    #[default]
    Stop,
    /// Retry recovery with backoff.
    Retry {
        /// Maximum number of retry attempts (None = unlimited).
        max_attempts: Option<u32>,
        /// Initial delay before the first retry.
        initial_delay: Duration,
    },
    /// Skip recovery and start with default state.
    SkipAndStart,
}


/// What to do when a persist operation fails.
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum PersistFailurePolicy {
    /// Stop the actor on persist failure (default).
    #[default]
    Stop,
    /// Return the error to the caller.
    ReturnError,
    /// Retry the persist operation with backoff.
    Retry {
        /// Maximum number of retry attempts (None = unlimited).
        max_attempts: Option<u32>,
        /// Initial delay before the first retry.
        initial_delay: Duration,
    },
}


/// Controls automatic snapshotting for event-sourced actors.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct SnapshotConfig {
    /// Take a snapshot every N events (None = disabled).
    pub every_n_events: Option<u64>,
    /// Take a snapshot at this interval (None = disabled).
    pub interval: Option<Duration>,
    /// Keep at most N snapshots (None = keep all).
    pub retention_count: Option<u32>,
    /// Whether to delete journal events after a successful snapshot.
    pub delete_events_on_snapshot: bool,
}


/// Controls automatic state saving for durable state actors.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct SaveConfig {
    /// Save state every N messages (None = disabled).
    pub every_n_messages: Option<u64>,
    /// Save state at this interval (None = disabled).
    pub interval: Option<Duration>,
}


// ── Storage Traits ──────────────────────────────────

/// Pluggable journal storage for event-sourced actors.
#[async_trait::async_trait]
pub trait JournalStorage: Send + Sync + 'static {
    /// Append a single event to the journal.
    async fn write_event(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
        event_type: &str,
        payload: &[u8],
    ) -> Result<(), PersistError>;

    /// Append a batch of events to the journal atomically.
    async fn write_event_batch(
        &self,
        persistence_id: &PersistenceId,
        entries: &[(SequenceId, &str, &[u8])],
    ) -> Result<(), PersistError>;

    /// Read all events starting from `from_sequence`.
    async fn read_events(
        &self,
        persistence_id: &PersistenceId,
        from_sequence: SequenceId,
    ) -> Result<Vec<JournalEntry>, PersistError>;

    /// Read the highest sequence number in the journal.
    async fn read_highest_sequence(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<SequenceId>, PersistError>;

    /// Delete all events up to and including `to_sequence`.
    async fn delete_events_to(
        &self,
        persistence_id: &PersistenceId,
        to_sequence: SequenceId,
    ) -> Result<(), PersistError>;
}

/// Pluggable snapshot storage.
#[async_trait::async_trait]
pub trait SnapshotStorage: Send + Sync + 'static {
    /// Save a snapshot at the given sequence number.
    async fn save_snapshot(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
        payload: &[u8],
    ) -> Result<(), PersistError>;

    /// Load the most recent snapshot.
    async fn load_latest_snapshot(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<SnapshotEntry>, PersistError>;

    /// Delete all snapshots before the given sequence number.
    async fn delete_snapshots_before(
        &self,
        persistence_id: &PersistenceId,
        sequence_id: SequenceId,
    ) -> Result<(), PersistError>;
}

/// Pluggable state store for durable state actors.
#[async_trait::async_trait]
pub trait StateStorage: Send + Sync + 'static {
    /// Save the current state.
    async fn save_state(
        &self,
        persistence_id: &PersistenceId,
        payload: &[u8],
    ) -> Result<(), PersistError>;

    /// Load the previously saved state, if any.
    async fn load_state(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<Option<Vec<u8>>, PersistError>;

    /// Delete the saved state.
    async fn delete_state(
        &self,
        persistence_id: &PersistenceId,
    ) -> Result<(), PersistError>;
}

// ── In-Memory Storage (for testing) ─────────────────

/// In-memory storage implementation for testing.
/// Stores journal events, snapshots, and state in HashMaps.
///
/// **Note:** Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) intentionally.
/// All lock-guarded operations are immediate HashMap lookups with no `.await`
/// points while the lock is held. Per tokio docs, `std::sync::Mutex` is
/// preferred for short, non-async critical sections.
pub struct InMemoryStorage {
    journals: Mutex<HashMap<String, Vec<JournalEntry>>>,
    snapshots: Mutex<HashMap<String, Vec<SnapshotEntry>>>,
    states: Mutex<HashMap<String, Vec<u8>>>,
}

impl InMemoryStorage {
    /// Create a new empty in-memory storage.
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

// ── Actor-facing Persistence Traits ──────────────────

/// Marker trait for actors that persist their state.
/// Provides the persistence identity and lifecycle hooks for recovery.
pub trait PersistentActor: Actor {
    /// Unique persistence identity for this actor's journal/snapshots.
    fn persistence_id(&self) -> PersistenceId;

    /// Called before recovery starts. Default: no-op.
    fn pre_recovery(&mut self) {}

    /// Called after recovery completes successfully. Default: no-op.
    fn post_recovery(&mut self) {}

    /// Policy when recovery fails. Default: Stop.
    fn recovery_failure_policy(&self) -> RecoveryFailurePolicy {
        RecoveryFailurePolicy::Stop
    }

    /// Policy when a persist operation fails. Default: Stop.
    fn persist_failure_policy(&self) -> PersistFailurePolicy {
        PersistFailurePolicy::Stop
    }
}

/// Event-sourced actor that rebuilds state by replaying events.
///
/// The actor's state is derived entirely from a sequence of events.
/// `apply()` is called for each event during recovery (replay) and
/// during normal operation (after persist). It must be deterministic.
#[async_trait::async_trait]
pub trait EventSourced: PersistentActor {
    /// The event type for this actor. Must be serializable for storage.
    type Event: Send + Sync + 'static;

    /// Apply an event to the actor's state. Must be **deterministic** and
    /// **infallible** — the same sequence of events must always produce the
    /// same state. Panicking in `apply()` is a programming error.
    fn apply(&mut self, event: &Self::Event);

    /// Deserialize an event from raw bytes (used during replay).
    fn deserialize_event(&self, payload: &[u8]) -> Result<Self::Event, PersistError>;

    /// Serialize an event to raw bytes (used when persisting).
    fn serialize_event(&self, event: &Self::Event) -> Result<Vec<u8>, PersistError>;

    /// Persist an event to the journal, then apply it to the state.
    /// The event is first written to storage, then applied via `apply()`.
    /// Returns Err on storage/serialization failure; the caller (runtime)
    /// should consult `persist_failure_policy()` to decide next action.
    async fn persist(
        &mut self,
        event: Self::Event,
        journal: &dyn JournalStorage,
    ) -> Result<SequenceId, PersistError> {
        let next_seq = self.last_sequence_id().next();
        let payload = self.serialize_event(&event)?;
        let event_type = std::any::type_name::<Self::Event>();
        journal
            .write_event(&self.persistence_id(), next_seq, event_type, &payload)
            .await?;
        self.apply(&event);
        self.set_last_sequence_id(next_seq);
        Ok(next_seq)
    }

    /// Persist multiple events atomically, then apply them in order.
    async fn persist_batch(
        &mut self,
        events: Vec<Self::Event>,
        journal: &dyn JournalStorage,
    ) -> Result<SequenceId, PersistError> {
        if events.is_empty() {
            return Ok(self.last_sequence_id());
        }
        let pid = self.persistence_id();
        let event_type = std::any::type_name::<Self::Event>();
        let mut seq = self.last_sequence_id();
        let mut batch: Vec<(SequenceId, &str, Vec<u8>)> = Vec::with_capacity(events.len());
        for event in &events {
            seq = seq.next();
            let payload = self.serialize_event(event)?;
            batch.push((seq, event_type, payload));
        }
        let refs: Vec<(SequenceId, &str, &[u8])> = batch
            .iter()
            .map(|(s, t, p)| (*s, *t, p.as_slice()))
            .collect();
        journal.write_event_batch(&pid, &refs).await?;
        for event in &events {
            self.apply(event);
        }
        self.set_last_sequence_id(seq);
        Ok(seq)
    }

    /// Create a snapshot of the current state.
    async fn snapshot(
        &self,
        snapshots: &dyn SnapshotStorage,
    ) -> Result<(), PersistError> {
        let payload = self.snapshot_payload()?;
        snapshots
            .save_snapshot(&self.persistence_id(), self.last_sequence_id(), &payload)
            .await
    }

    /// Restore state from a snapshot. Called during recovery before
    /// replaying events that occurred after the snapshot.
    fn restore_snapshot(&mut self, payload: Vec<u8>) -> Result<(), PersistError>;

    /// Serialize the current state for snapshotting.
    fn snapshot_payload(&self) -> Result<Vec<u8>, PersistError>;

    /// Optional snapshot configuration for automatic snapshots.
    fn snapshot_config(&self) -> Option<SnapshotConfig> {
        None
    }

    /// The last applied sequence ID. Used to determine where to start
    /// replaying events during recovery.
    fn last_sequence_id(&self) -> SequenceId;

    /// Set the last sequence ID (called during recovery).
    fn set_last_sequence_id(&mut self, seq: SequenceId);
}

/// Durable state actor that persists the entire state as a snapshot.
///
/// Unlike EventSourced, this stores the full state each time rather
/// than individual events. Simpler but less efficient for large states
/// with small changes.
#[async_trait::async_trait]
pub trait DurableState: PersistentActor {
    /// Serialize the current state for storage.
    fn serialize_state(&self) -> Result<Vec<u8>, PersistError>;

    /// Restore state from stored bytes. Called during recovery.
    fn restore_state(&mut self, payload: Vec<u8>) -> Result<(), PersistError>;

    /// Save the current state to storage.
    async fn save_state(
        &self,
        storage: &dyn StateStorage,
    ) -> Result<(), PersistError> {
        let payload = self.serialize_state()?;
        storage.save_state(&self.persistence_id(), &payload).await
    }

    /// Optional save configuration for automatic saves.
    fn save_config(&self) -> Option<SaveConfig> {
        None
    }
}

/// Provides storage instances for persistent actors.
/// Register on the runtime to enable persistence.
pub trait StorageProvider: Send + Sync + 'static {
    /// Get a journal storage instance for the given persistence ID.
    fn journal(&self, id: &PersistenceId) -> Option<Arc<dyn JournalStorage>>;

    /// Get a snapshot storage instance for the given persistence ID.
    fn snapshots(&self, id: &PersistenceId) -> Option<Arc<dyn SnapshotStorage>>;

    /// Get a state storage instance for the given persistence ID.
    fn state(&self, id: &PersistenceId) -> Option<Arc<dyn StateStorage>>;
}

/// In-memory storage provider for testing. Wraps a single [`InMemoryStorage`]
/// and returns it for every persistence ID.
pub struct InMemoryStorageProvider {
    storage: Arc<InMemoryStorage>,
}

impl InMemoryStorageProvider {
    /// Create a new in-memory storage provider.
    pub fn new() -> Self {
        Self {
            storage: Arc::new(InMemoryStorage::new()),
        }
    }

    /// Create from an existing `Arc<InMemoryStorage>` for shared access.
    pub fn from_storage(storage: Arc<InMemoryStorage>) -> Self {
        Self { storage }
    }
}

impl Default for InMemoryStorageProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageProvider for InMemoryStorageProvider {
    fn journal(&self, _id: &PersistenceId) -> Option<Arc<dyn JournalStorage>> {
        Some(self.storage.clone())
    }

    fn snapshots(&self, _id: &PersistenceId) -> Option<Arc<dyn SnapshotStorage>> {
        Some(self.storage.clone())
    }

    fn state(&self, _id: &PersistenceId) -> Option<Arc<dyn StateStorage>> {
        Some(self.storage.clone())
    }
}

/// Recover an event-sourced actor by loading the latest snapshot and
/// replaying journal events that occurred after it.
///
/// Events are deserialized in full before any are applied. This ensures
/// that a deserialization failure on event N doesn't leave the actor in
/// a partially-recovered state (livelock on retry).
///
/// Consults `actor.recovery_failure_policy()` on error:
/// - `Stop` — propagate the error (default).
/// - `Retry { max_attempts, initial_delay }` — retry with backoff.
/// - `SkipAndStart` — skip recovery and start fresh.
pub async fn recover_event_sourced<A: EventSourced>(
    actor: &mut A,
    journal: &dyn JournalStorage,
    snapshots: &dyn SnapshotStorage,
) -> Result<(), PersistError> {
    let policy = actor.recovery_failure_policy();

    match recover_event_sourced_inner(actor, journal, snapshots).await {
        Ok(()) => Ok(()),
        Err(e) => match policy {
            RecoveryFailurePolicy::Stop => Err(e),
            RecoveryFailurePolicy::Retry { max_attempts, initial_delay } => {
                let attempts = max_attempts.unwrap_or(3);
                let mut delay = initial_delay;
                for _ in 1..attempts {
                    tokio::time::sleep(delay).await;
                    delay = std::time::Duration::from_millis(
                        (delay.as_millis() as u64).saturating_mul(2).min(30_000),
                    );
                    match recover_event_sourced_inner(actor, journal, snapshots).await {
                        Ok(()) => return Ok(()),
                        Err(_) => continue,
                    }
                }
                Err(e)
            }
            RecoveryFailurePolicy::SkipAndStart => {
                actor.post_recovery();
                Ok(())
            }
        },
    }
}

async fn recover_event_sourced_inner<A: EventSourced>(
    actor: &mut A,
    journal: &dyn JournalStorage,
    snapshots: &dyn SnapshotStorage,
) -> Result<(), PersistError> {
    actor.pre_recovery();

    // Load latest snapshot
    let pid = actor.persistence_id();
    if let Some(snap) = snapshots.load_latest_snapshot(&pid).await? {
        actor.restore_snapshot(snap.payload)?;
        actor.set_last_sequence_id(snap.sequence_id);
    }

    // Replay events after the snapshot.
    // Deserialize ALL events first to avoid partial apply on failure.
    let start = actor.last_sequence_id().next();
    let entries = journal.read_events(&pid, start).await?;
    let deserialized: Vec<(SequenceId, A::Event)> = entries
        .iter()
        .map(|entry| {
            let event = actor.deserialize_event(&entry.payload)?;
            Ok((entry.sequence_id, event))
        })
        .collect::<Result<Vec<_>, PersistError>>()?;

    // All deserialized successfully — now apply. apply() is infallible.
    for (seq, event) in &deserialized {
        actor.apply(event);
        actor.set_last_sequence_id(*seq);
    }

    actor.post_recovery();
    Ok(())
}

/// Recover a durable-state actor by loading its stored state.
///
/// Consults `actor.recovery_failure_policy()` on error:
/// - `Stop` — propagate the error (default).
/// - `Retry { max_attempts, initial_delay }` — retry with backoff.
/// - `SkipAndStart` — skip recovery and start fresh.
pub async fn recover_durable_state<A: DurableState>(
    actor: &mut A,
    storage: &dyn StateStorage,
) -> Result<(), PersistError> {
    let policy = actor.recovery_failure_policy();

    match recover_durable_state_inner(actor, storage).await {
        Ok(()) => Ok(()),
        Err(e) => match policy {
            RecoveryFailurePolicy::Stop => Err(e),
            RecoveryFailurePolicy::Retry { max_attempts, initial_delay } => {
                let attempts = max_attempts.unwrap_or(3);
                let mut delay = initial_delay;
                for _ in 1..attempts {
                    tokio::time::sleep(delay).await;
                    delay = std::time::Duration::from_millis(
                        (delay.as_millis() as u64).saturating_mul(2).min(30_000),
                    );
                    match recover_durable_state_inner(actor, storage).await {
                        Ok(()) => return Ok(()),
                        Err(_) => continue,
                    }
                }
                Err(e)
            }
            RecoveryFailurePolicy::SkipAndStart => {
                actor.post_recovery();
                Ok(())
            }
        },
    }
}

async fn recover_durable_state_inner<A: DurableState>(
    actor: &mut A,
    storage: &dyn StateStorage,
) -> Result<(), PersistError> {
    actor.pre_recovery();

    let pid = actor.persistence_id();
    if let Some(payload) = storage.load_state(&pid).await? {
        actor.restore_state(payload)?;
    }

    actor.post_recovery();
    Ok(())
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

    // ── Mock actor for trait tests ──────────────────────

    use crate::actor::Actor;

    /// Minimal event-sourced counter used to test the persistence traits.
    struct CounterActor {
        id: String,
        value: i64,
        last_seq: SequenceId,
        recovered: bool,
    }

    impl CounterActor {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                value: 0,
                last_seq: SequenceId(0),
                recovered: false,
            }
        }
    }

    impl Actor for CounterActor {
        type Args = ();
        type Deps = ();
        fn create(_args: (), _deps: ()) -> Self {
            CounterActor::new("default")
        }
    }

    impl PersistentActor for CounterActor {
        fn persistence_id(&self) -> PersistenceId {
            PersistenceId::new("Counter", &self.id)
        }

        fn post_recovery(&mut self) {
            self.recovered = true;
        }
    }

    /// Simple i64 delta event.
    #[derive(Debug, Clone)]
    struct CounterEvent(i64);

    #[async_trait::async_trait]
    impl EventSourced for CounterActor {
        type Event = CounterEvent;

        fn apply(&mut self, event: &CounterEvent) {
            self.value += event.0;
        }

        fn deserialize_event(&self, payload: &[u8]) -> Result<CounterEvent, PersistError> {
            if payload.len() != 8 {
                return Err(PersistError::SerializationFailed("bad length".into()));
            }
            let bytes: [u8; 8] = payload.try_into().unwrap();
            Ok(CounterEvent(i64::from_le_bytes(bytes)))
        }

        fn serialize_event(&self, event: &CounterEvent) -> Result<Vec<u8>, PersistError> {
            Ok(event.0.to_le_bytes().to_vec())
        }

        fn restore_snapshot(&mut self, payload: Vec<u8>) -> Result<(), PersistError> {
            if payload.len() != 8 {
                return Err(PersistError::SerializationFailed("bad snapshot".into()));
            }
            let bytes: [u8; 8] = payload.try_into().unwrap();
            self.value = i64::from_le_bytes(bytes);
            Ok(())
        }

        fn snapshot_payload(&self) -> Result<Vec<u8>, PersistError> {
            Ok(self.value.to_le_bytes().to_vec())
        }

        fn last_sequence_id(&self) -> SequenceId {
            self.last_seq
        }

        fn set_last_sequence_id(&mut self, seq: SequenceId) {
            self.last_seq = seq;
        }
    }

    #[tokio::test]
    async fn test_event_sourced_persist_and_apply() {
        let storage = InMemoryStorage::new();
        let mut actor = CounterActor::new("1");

        let seq = actor.persist(CounterEvent(10), &storage).await.unwrap();
        assert_eq!(seq, SequenceId(1));
        assert_eq!(actor.value, 10);

        let seq = actor.persist(CounterEvent(-3), &storage).await.unwrap();
        assert_eq!(seq, SequenceId(2));
        assert_eq!(actor.value, 7);

        // Verify journal entries were written
        let pid = actor.persistence_id();
        let entries = storage.read_events(&pid, SequenceId(1)).await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_event_sourced_persist_batch() {
        let storage = InMemoryStorage::new();
        let mut actor = CounterActor::new("batch");

        let events = vec![CounterEvent(5), CounterEvent(3), CounterEvent(2)];
        let seq = actor.persist_batch(events, &storage).await.unwrap();
        assert_eq!(seq, SequenceId(3));
        assert_eq!(actor.value, 10);

        let pid = actor.persistence_id();
        let entries = storage.read_events(&pid, SequenceId(1)).await.unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[tokio::test]
    async fn test_event_sourced_persist_batch_empty() {
        let storage = InMemoryStorage::new();
        let mut actor = CounterActor::new("empty-batch");

        let seq = actor.persist_batch(vec![], &storage).await.unwrap();
        assert_eq!(seq, SequenceId(0));
        assert_eq!(actor.value, 0);
    }

    #[tokio::test]
    async fn test_event_sourced_snapshot() {
        let storage = InMemoryStorage::new();
        let mut actor = CounterActor::new("snap");

        actor.persist(CounterEvent(42), &storage).await.unwrap();
        actor.snapshot(&storage).await.unwrap();

        let pid = actor.persistence_id();
        let snap = storage.load_latest_snapshot(&pid).await.unwrap().unwrap();
        assert_eq!(snap.sequence_id, SequenceId(1));
        let bytes: [u8; 8] = snap.payload.try_into().unwrap();
        assert_eq!(i64::from_le_bytes(bytes), 42);
    }

    #[tokio::test]
    async fn test_recover_event_sourced_events_only() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Counter", "recover");

        // Pre-populate journal
        storage
            .write_event(&pid, SequenceId(1), "CounterEvent", &10i64.to_le_bytes())
            .await
            .unwrap();
        storage
            .write_event(&pid, SequenceId(2), "CounterEvent", &5i64.to_le_bytes())
            .await
            .unwrap();

        let mut actor = CounterActor::new("recover");
        recover_event_sourced(&mut actor, &storage, &storage)
            .await
            .unwrap();

        assert_eq!(actor.value, 15);
        assert_eq!(actor.last_seq, SequenceId(2));
        assert!(actor.recovered);
    }

    #[tokio::test]
    async fn test_recover_event_sourced_with_snapshot() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Counter", "snap-recover");

        // Snapshot at seq 2 with value 15
        storage
            .save_snapshot(&pid, SequenceId(2), &15i64.to_le_bytes())
            .await
            .unwrap();
        // Event after snapshot
        storage
            .write_event(&pid, SequenceId(3), "CounterEvent", &7i64.to_le_bytes())
            .await
            .unwrap();

        let mut actor = CounterActor::new("snap-recover");
        recover_event_sourced(&mut actor, &storage, &storage)
            .await
            .unwrap();

        assert_eq!(actor.value, 22); // 15 + 7
        assert_eq!(actor.last_seq, SequenceId(3));
        assert!(actor.recovered);
    }

    #[tokio::test]
    async fn test_recover_event_sourced_empty() {
        let storage = InMemoryStorage::new();
        let mut actor = CounterActor::new("empty");

        recover_event_sourced(&mut actor, &storage, &storage)
            .await
            .unwrap();

        assert_eq!(actor.value, 0);
        assert_eq!(actor.last_seq, SequenceId(0));
        assert!(actor.recovered);
    }

    // ── Durable state mock ──────────────────────────────

    struct ConfigActor {
        id: String,
        data: String,
        recovered: bool,
    }

    impl ConfigActor {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                data: String::new(),
                recovered: false,
            }
        }
    }

    impl Actor for ConfigActor {
        type Args = ();
        type Deps = ();
        fn create(_args: (), _deps: ()) -> Self {
            ConfigActor::new("default")
        }
    }

    impl PersistentActor for ConfigActor {
        fn persistence_id(&self) -> PersistenceId {
            PersistenceId::new("Config", &self.id)
        }

        fn post_recovery(&mut self) {
            self.recovered = true;
        }
    }

    #[async_trait::async_trait]
    impl DurableState for ConfigActor {
        fn serialize_state(&self) -> Result<Vec<u8>, PersistError> {
            Ok(self.data.as_bytes().to_vec())
        }

        fn restore_state(&mut self, payload: Vec<u8>) -> Result<(), PersistError> {
            self.data = String::from_utf8(payload)
                .map_err(|e| PersistError::SerializationFailed(e.to_string()))?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_durable_state_save_and_recover() {
        let storage = InMemoryStorage::new();
        let pid = PersistenceId::new("Config", "ds");

        // Save state via trait default impl
        let mut actor = ConfigActor::new("ds");
        actor.data = "hello world".to_string();
        DurableState::save_state(&actor, &storage as &dyn StateStorage)
            .await
            .unwrap();

        // Recover into a fresh actor
        let mut actor2 = ConfigActor::new("ds");
        recover_durable_state(&mut actor2, &storage).await.unwrap();
        assert_eq!(actor2.data, "hello world");
        assert!(actor2.recovered);
    }

    #[tokio::test]
    async fn test_recover_durable_state_empty() {
        let storage = InMemoryStorage::new();
        let mut actor = ConfigActor::new("empty");

        recover_durable_state(&mut actor, &storage).await.unwrap();
        assert_eq!(actor.data, "");
        assert!(actor.recovered);
    }

    // ── StorageProvider tests ───────────────────────────

    #[test]
    fn test_in_memory_storage_provider() {
        let provider = InMemoryStorageProvider::new();
        let pid = PersistenceId::new("Test", "1");

        assert!(provider.journal(&pid).is_some());
        assert!(provider.snapshots(&pid).is_some());
        assert!(provider.state(&pid).is_some());
    }

    #[tokio::test]
    async fn test_in_memory_storage_provider_shared() {
        let raw = Arc::new(InMemoryStorage::new());
        let provider = InMemoryStorageProvider::from_storage(raw.clone());
        let pid = PersistenceId::new("Test", "shared");

        // Write via provider, read via raw storage
        let journal = provider.journal(&pid).unwrap();
        journal
            .write_event(&pid, SequenceId(1), "E", b"data")
            .await
            .unwrap();
        let events = raw.read_events(&pid, SequenceId(1)).await.unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_persistent_actor_default_policies() {
        let actor = CounterActor::new("policy");
        assert!(matches!(
            actor.recovery_failure_policy(),
            RecoveryFailurePolicy::Stop
        ));
        assert!(matches!(
            actor.persist_failure_policy(),
            PersistFailurePolicy::Stop
        ));
    }
}
