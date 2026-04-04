//! Persistence: journal storage, snapshots, and state storage.
//!
//! Demonstrates the storage traits and InMemoryStorage for testing.
//!
//! Run with: cargo run --example persistence --features test-support

use dactor::persistence::*;

#[tokio::main]
async fn main() {
    let storage = InMemoryStorage::new();
    let pid = PersistenceId::new("BankAccount", "acct-42");
    println!("=== Persistence Example ===\n");

    // ── Journal (Event Sourcing) ────────────────────────────
    println!("--- Journal Storage ---");

    // Write events
    storage
        .write_event(&pid, SequenceId(1), "Deposited", b"100")
        .await
        .unwrap();
    storage
        .write_event(&pid, SequenceId(2), "Withdrawn", b"30")
        .await
        .unwrap();
    storage
        .write_event(&pid, SequenceId(3), "Deposited", b"50")
        .await
        .unwrap();
    println!("  Wrote 3 events to journal");

    // Read all events
    let events = storage.read_events(&pid, SequenceId(1)).await.unwrap();
    for e in &events {
        println!(
            "  Event seq={}: type={}, payload={:?}",
            e.sequence_id.0,
            e.event_type,
            String::from_utf8_lossy(&e.payload)
        );
    }

    // Highest sequence
    let highest = storage.read_highest_sequence(&pid).await.unwrap();
    println!("  Highest sequence: {:?}", highest.unwrap().0);

    // Read from sequence 2 onwards (recovery after snapshot)
    let recent = storage.read_events(&pid, SequenceId(2)).await.unwrap();
    println!("  Events from seq 2: {} entries", recent.len());

    // ── Snapshots ───────────────────────────────────────────
    println!("\n--- Snapshot Storage ---");

    storage
        .save_snapshot(&pid, SequenceId(2), b"balance=70")
        .await
        .unwrap();
    println!("  Saved snapshot at seq 2: balance=70");

    storage
        .save_snapshot(&pid, SequenceId(3), b"balance=120")
        .await
        .unwrap();
    println!("  Saved snapshot at seq 3: balance=120");

    let latest = storage.load_latest_snapshot(&pid).await.unwrap().unwrap();
    println!(
        "  Latest snapshot: seq={}, state={:?}",
        latest.sequence_id.0,
        String::from_utf8_lossy(&latest.payload)
    );

    // Recovery simulation: load snapshot + replay events after it
    println!("\n--- Recovery Simulation ---");
    let snap = storage.load_latest_snapshot(&pid).await.unwrap();
    match snap {
        Some(s) => {
            println!(
                "  Loaded snapshot at seq {}: {:?}",
                s.sequence_id.0,
                String::from_utf8_lossy(&s.payload)
            );
            let replay = storage
                .read_events(&pid, s.sequence_id.next())
                .await
                .unwrap();
            println!("  Replaying {} events after snapshot", replay.len());
            for e in &replay {
                println!(
                    "    Apply: {} ({:?})",
                    e.event_type,
                    String::from_utf8_lossy(&e.payload)
                );
            }
        }
        None => println!("  No snapshot — replay all events"),
    }

    // ── Durable State ───────────────────────────────────────
    println!("\n--- State Storage (Durable State) ---");

    let config_pid = PersistenceId::new("Config", "app-settings");
    storage
        .save_state(&config_pid, b"{\"theme\": \"dark\", \"lang\": \"en\"}")
        .await
        .unwrap();
    println!("  Saved state: theme=dark, lang=en");

    let state = storage.load_state(&config_pid).await.unwrap().unwrap();
    println!("  Loaded state: {:?}", String::from_utf8_lossy(&state));

    // Overwrite
    storage
        .save_state(&config_pid, b"{\"theme\": \"light\", \"lang\": \"en\"}")
        .await
        .unwrap();
    let updated = storage.load_state(&config_pid).await.unwrap().unwrap();
    println!("  Updated state: {:?}", String::from_utf8_lossy(&updated));

    // ── Cleanup ─────────────────────────────────────────────
    println!("\n--- Cleanup ---");
    storage.delete_events_to(&pid, SequenceId(2)).await.unwrap();
    let remaining = storage.read_events(&pid, SequenceId(1)).await.unwrap();
    println!(
        "  After deleting events to seq 2: {} events remain",
        remaining.len()
    );

    storage.delete_state(&config_pid).await.unwrap();
    let gone = storage.load_state(&config_pid).await.unwrap();
    println!("  After deleting state: exists={}", gone.is_some());

    println!("\n=== Done ===");
}
