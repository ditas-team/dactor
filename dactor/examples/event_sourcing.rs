//! Event-sourced persistence: persist events, snapshot, and recover.
//!
//! Demonstrates the `EventSourced` and `PersistentActor` traits with
//! `InMemoryStorage`, including snapshot-based recovery.
//!
//! Run with: cargo run --example event_sourcing --features test-support

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::persistence::*;
use dactor::TestRuntime;

use std::time::Duration;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum BankEvent {
    Deposited(u64),
    Withdrawn(u64),
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct BankAccount {
    account_id: String,
    balance: u64,
    seq: SequenceId,
}

impl BankAccount {
    fn new(account_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            balance: 0,
            seq: SequenceId(0),
        }
    }
}

impl Actor for BankAccount {
    type Args = String;
    type Deps = ();
    fn create(args: String, _deps: ()) -> Self {
        Self::new(&args)
    }
}

impl PersistentActor for BankAccount {
    fn persistence_id(&self) -> PersistenceId {
        PersistenceId::new("BankAccount", &self.account_id)
    }
}

#[async_trait]
impl EventSourced for BankAccount {
    type Event = BankEvent;

    fn apply(&mut self, event: &BankEvent) {
        match event {
            BankEvent::Deposited(amt) => self.balance += amt,
            BankEvent::Withdrawn(amt) => self.balance -= amt,
        }
    }

    fn serialize_event(&self, event: &BankEvent) -> Result<Vec<u8>, PersistError> {
        let bytes = match event {
            BankEvent::Deposited(a) => [b"D:".as_slice(), &a.to_le_bytes()].concat(),
            BankEvent::Withdrawn(a) => [b"W:".as_slice(), &a.to_le_bytes()].concat(),
        };
        Ok(bytes)
    }

    fn deserialize_event(&self, payload: &[u8]) -> Result<BankEvent, PersistError> {
        if payload.len() < 10 {
            return Err(PersistError::SerializationFailed("too short".into()));
        }
        let amt = u64::from_le_bytes(payload[2..10].try_into().unwrap());
        match &payload[..2] {
            b"D:" => Ok(BankEvent::Deposited(amt)),
            b"W:" => Ok(BankEvent::Withdrawn(amt)),
            _ => Err(PersistError::SerializationFailed("unknown tag".into())),
        }
    }

    fn snapshot_payload(&self) -> Result<Vec<u8>, PersistError> {
        Ok(self.balance.to_le_bytes().to_vec())
    }

    fn restore_snapshot(&mut self, payload: Vec<u8>) -> Result<(), PersistError> {
        self.balance = u64::from_le_bytes(
            payload
                .try_into()
                .map_err(|_| PersistError::SerializationFailed("bad snapshot".into()))?,
        );
        Ok(())
    }

    fn last_sequence_id(&self) -> SequenceId {
        self.seq
    }
    fn set_last_sequence_id(&mut self, seq: SequenceId) {
        self.seq = seq;
    }
}

// ---------------------------------------------------------------------------
// Messages (used with TestRuntime to show actor + persistence together)
// ---------------------------------------------------------------------------

struct GetBalance;
impl Message for GetBalance {
    type Reply = u64;
}

#[async_trait]
impl Handler<GetBalance> for BankAccount {
    async fn handle(&mut self, _msg: GetBalance, _ctx: &mut ActorContext) -> u64 {
        self.balance
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Event Sourcing Example ===\n");

    let storage = InMemoryStorage::new();

    // 1. Create an account and persist events directly.
    println!("--- Persist events ---");
    let mut account = BankAccount::new("acct-1");
    account
        .persist(BankEvent::Deposited(100), &storage)
        .await
        .unwrap();
    account
        .persist(BankEvent::Deposited(50), &storage)
        .await
        .unwrap();
    account
        .persist(BankEvent::Withdrawn(30), &storage)
        .await
        .unwrap();
    println!("  Balance after 3 events: {}", account.balance);
    assert_eq!(account.balance, 120);

    // 2. Take a snapshot at the current point.
    println!("\n--- Snapshot ---");
    account.snapshot(&storage).await.unwrap();
    println!("  Snapshot saved at seq {}", account.last_sequence_id().0);

    // 3. More events after the snapshot.
    account
        .persist(BankEvent::Deposited(80), &storage)
        .await
        .unwrap();
    println!("  Balance after 1 more event: {}", account.balance);
    assert_eq!(account.balance, 200);

    // 4. Recover from scratch — snapshot + replay.
    println!("\n--- Recovery ---");
    let mut recovered = BankAccount::new("acct-1");
    recover_event_sourced(&mut recovered, &storage, &storage)
        .await
        .unwrap();
    println!("  Recovered balance: {}", recovered.balance);
    assert_eq!(recovered.balance, 200);
    assert_eq!(recovered.last_sequence_id().0, 4);

    // 5. Use the actor via TestRuntime to show it works as a normal actor.
    println!("\n--- Actor via TestRuntime ---");
    let rt = TestRuntime::new();
    let actor_ref = rt.spawn::<BankAccount>("bank", "acct-2".to_string());
    let bal = actor_ref.ask(GetBalance, None).unwrap().await.unwrap();
    println!("  Fresh actor balance: {}", bal);
    assert_eq!(bal, 0);

    actor_ref.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor_ref.is_alive());

    println!("\n=== Done ===");
}
