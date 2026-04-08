//! Integration tests for persistence: EventSourced, DurableState, InMemoryStorage.
//!
//! These exercise the full lifecycle of persisting, snapshotting, and recovering
//! actors using the persistence traits directly (no TestRuntime spawning needed).

use async_trait::async_trait;
use dactor::actor::Actor;
use dactor::persistence::*;

// ── Counter actor (event-sourced) ───────────────────────────

#[derive(Debug, Clone)]
enum CounterEvent {
    Add(i64),
    Subtract(i64),
}

struct CounterActor {
    id: String,
    value: i64,
    last_seq: SequenceId,
}

impl CounterActor {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            value: 0,
            last_seq: SequenceId(0),
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
}

#[async_trait]
impl EventSourced for CounterActor {
    type Event = CounterEvent;

    fn apply(&mut self, event: &CounterEvent) {
        match event {
            CounterEvent::Add(v) => self.value += v,
            CounterEvent::Subtract(v) => self.value -= v,
        }
    }

    fn serialize_event(&self, event: &CounterEvent) -> Result<Vec<u8>, PersistError> {
        let bytes = match event {
            CounterEvent::Add(v) => [b"A:".as_slice(), &v.to_le_bytes()].concat(),
            CounterEvent::Subtract(v) => [b"S:".as_slice(), &v.to_le_bytes()].concat(),
        };
        Ok(bytes)
    }

    fn deserialize_event(&self, payload: &[u8]) -> Result<CounterEvent, PersistError> {
        if payload.len() != 10 {
            return Err(PersistError::SerializationFailed(
                format!("expected 10 bytes, got {}", payload.len()),
            ));
        }
        let val = i64::from_le_bytes(payload[2..10].try_into().unwrap());
        match &payload[..2] {
            b"A:" => Ok(CounterEvent::Add(val)),
            b"S:" => Ok(CounterEvent::Subtract(val)),
            _ => Err(PersistError::SerializationFailed("unknown tag".into())),
        }
    }

    fn snapshot_payload(&self) -> Result<Vec<u8>, PersistError> {
        Ok(self.value.to_le_bytes().to_vec())
    }

    fn restore_snapshot(&mut self, payload: Vec<u8>) -> Result<(), PersistError> {
        if payload.len() != 8 {
            return Err(PersistError::SerializationFailed("bad snapshot len".into()));
        }
        self.value = i64::from_le_bytes(payload.try_into().unwrap());
        Ok(())
    }

    fn last_sequence_id(&self) -> SequenceId {
        self.last_seq
    }

    fn set_last_sequence_id(&mut self, seq: SequenceId) {
        self.last_seq = seq;
    }
}

// ── Config actor (durable state) ────────────────────────────

struct ConfigActor {
    id: String,
    data: String,
}

impl ConfigActor {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            data: String::new(),
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
}

#[async_trait]
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

// ── Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn test_event_sourced_full_lifecycle() {
    let storage = InMemoryStorage::new();
    let mut actor = CounterActor::new("lifecycle");

    // Persist 5 events, verify state after each.
    let expected_values = [10, 15, 12, 22, 17];
    let events = [
        CounterEvent::Add(10),
        CounterEvent::Add(5),
        CounterEvent::Subtract(3),
        CounterEvent::Add(10),
        CounterEvent::Subtract(5),
    ];
    for (i, event) in events.into_iter().enumerate() {
        actor.persist(event, &storage).await.unwrap();
        assert_eq!(actor.value, expected_values[i], "mismatch after event {}", i);
    }
    assert_eq!(actor.last_sequence_id(), SequenceId(5));

    // Take a snapshot at seq 5.
    actor.snapshot(&storage).await.unwrap();

    // Persist 3 more events after the snapshot.
    actor.persist(CounterEvent::Add(100), &storage).await.unwrap();
    actor.persist(CounterEvent::Subtract(7), &storage).await.unwrap();
    actor.persist(CounterEvent::Add(3), &storage).await.unwrap();

    // Final state = 17 + 100 - 7 + 3 = 113
    assert_eq!(actor.value, 113);
    assert_eq!(actor.last_sequence_id(), SequenceId(8));
}

#[tokio::test]
async fn test_event_sourced_recovery_from_events() {
    let storage = InMemoryStorage::new();

    // Persist 10 events via a first actor.
    let mut actor = CounterActor::new("evonly");
    for i in 1..=10 {
        actor
            .persist(CounterEvent::Add(i as i64), &storage)
            .await
            .unwrap();
    }
    let expected_sum: i64 = (1..=10).sum(); // 55
    assert_eq!(actor.value, expected_sum);

    // Recover into a fresh actor — events only, no snapshot.
    let mut recovered = CounterActor::new("evonly");
    recover_event_sourced(&mut recovered, &storage, &storage)
        .await
        .unwrap();

    assert_eq!(recovered.value, expected_sum);
    assert_eq!(recovered.last_sequence_id(), SequenceId(10));
}

#[tokio::test]
async fn test_event_sourced_recovery_with_snapshot() {
    let storage = InMemoryStorage::new();
    let mut actor = CounterActor::new("snaprec");

    // Persist 5 events.
    for i in 1..=5 {
        actor
            .persist(CounterEvent::Add(i * 10), &storage)
            .await
            .unwrap();
    }
    // value = 10+20+30+40+50 = 150
    assert_eq!(actor.value, 150);

    // Take a snapshot at seq 5.
    actor.snapshot(&storage).await.unwrap();

    // Persist 5 more events.
    for i in 1..=5 {
        actor
            .persist(CounterEvent::Subtract(i), &storage)
            .await
            .unwrap();
    }
    // value = 150 - (1+2+3+4+5) = 135
    assert_eq!(actor.value, 135);

    // Recover into a fresh actor — should load snapshot(150) + replay 5 events.
    let mut recovered = CounterActor::new("snaprec");
    recover_event_sourced(&mut recovered, &storage, &storage)
        .await
        .unwrap();

    assert_eq!(recovered.value, 135);
    assert_eq!(recovered.last_sequence_id(), SequenceId(10));
}

#[tokio::test]
async fn test_durable_state_save_and_recover() {
    let storage = InMemoryStorage::new();

    let mut actor = ConfigActor::new("cfg1");
    actor.data = "persistent-value-42".to_string();
    DurableState::save_state(&actor, &storage as &dyn StateStorage)
        .await
        .unwrap();

    // Recover into a fresh actor.
    let mut recovered = ConfigActor::new("cfg1");
    assert_eq!(recovered.data, "");
    recover_durable_state(&mut recovered, &storage).await.unwrap();

    assert_eq!(recovered.data, "persistent-value-42");
}

#[tokio::test]
async fn test_event_sourced_batch_persist() {
    let storage = InMemoryStorage::new();
    let mut actor = CounterActor::new("batch");

    let events: Vec<CounterEvent> = (1..=10).map(CounterEvent::Add).collect();
    let seq = actor.persist_batch(events, &storage).await.unwrap();

    let expected_sum: i64 = (1..=10).sum(); // 55
    assert_eq!(actor.value, expected_sum);
    assert_eq!(seq, SequenceId(10));
    assert_eq!(actor.last_sequence_id(), SequenceId(10));

    // Verify all events are in the journal.
    let pid = actor.persistence_id();
    let entries = storage.read_events(&pid, SequenceId(1)).await.unwrap();
    assert_eq!(entries.len(), 10);

    // Verify batch content by recovering a fresh actor from the journal.
    let mut recovered = CounterActor::new("batch");
    recover_event_sourced(&mut recovered, &storage, &storage)
        .await
        .unwrap();
    assert_eq!(recovered.value, expected_sum, "recovered state should match batch sum");
    assert_eq!(recovered.last_sequence_id(), SequenceId(10));
}

#[tokio::test]
async fn test_journal_cleanup_after_snapshot() {
    let storage = InMemoryStorage::new();
    let mut actor = CounterActor::new("cleanup");

    // Persist 10 events.
    for i in 1..=10 {
        actor
            .persist(CounterEvent::Add(i * 2), &storage)
            .await
            .unwrap();
    }
    // value = 2+4+6+8+10+12+14+16+18+20 = 110
    assert_eq!(actor.value, 110);

    // Snapshot at seq 10.
    actor.snapshot(&storage).await.unwrap();

    // Delete all journal events up to seq 10.
    let pid = actor.persistence_id();
    storage.delete_events_to(&pid, SequenceId(10)).await.unwrap();

    // Verify no events remain.
    let remaining = storage.read_events(&pid, SequenceId(1)).await.unwrap();
    assert!(remaining.is_empty());

    // Recover — should use snapshot only (no events to replay).
    let mut recovered = CounterActor::new("cleanup");
    recover_event_sourced(&mut recovered, &storage, &storage)
        .await
        .unwrap();

    assert_eq!(recovered.value, 110);
    assert_eq!(recovered.last_sequence_id(), SequenceId(10));
}

#[tokio::test]
async fn test_empty_recovery_creates_fresh_actor() {
    let storage = InMemoryStorage::new();
    let mut actor = CounterActor::new("fresh");

    // No events, no snapshot — recover should succeed with defaults.
    recover_event_sourced(&mut actor, &storage, &storage)
        .await
        .unwrap();

    assert_eq!(actor.value, 0);
    assert_eq!(actor.last_sequence_id(), SequenceId(0));
}

#[tokio::test]
async fn test_multiple_actors_independent_persistence() {
    let storage = InMemoryStorage::new();

    // Actor A: adds 10, 20, 30 → 60
    let mut actor_a = CounterActor::new("alpha");
    actor_a.persist(CounterEvent::Add(10), &storage).await.unwrap();
    actor_a.persist(CounterEvent::Add(20), &storage).await.unwrap();
    actor_a.persist(CounterEvent::Add(30), &storage).await.unwrap();
    assert_eq!(actor_a.value, 60);

    // Actor B: subtracts 5, adds 100 → 95
    let mut actor_b = CounterActor::new("beta");
    actor_b.persist(CounterEvent::Subtract(5), &storage).await.unwrap();
    actor_b.persist(CounterEvent::Add(100), &storage).await.unwrap();
    assert_eq!(actor_b.value, 95);

    // Recover each independently.
    let mut recovered_a = CounterActor::new("alpha");
    recover_event_sourced(&mut recovered_a, &storage, &storage)
        .await
        .unwrap();
    assert_eq!(recovered_a.value, 60);
    assert_eq!(recovered_a.last_sequence_id(), SequenceId(3));

    let mut recovered_b = CounterActor::new("beta");
    recover_event_sourced(&mut recovered_b, &storage, &storage)
        .await
        .unwrap();
    assert_eq!(recovered_b.value, 95);
    assert_eq!(recovered_b.last_sequence_id(), SequenceId(2));
}
