//! Bounded mailbox with overflow strategies.
//!
//! Run with: cargo run --example bounded_mailbox --features test-support

use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::mailbox::{MailboxConfig, OverflowStrategy};
use dactor::message::Message;
use dactor::{SpawnOptions, TestRuntime};

// ---------------------------------------------------------------------------
// A deliberately slow actor that sleeps inside its handler
// ---------------------------------------------------------------------------

struct SlowActor;

impl Actor for SlowActor {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        SlowActor
    }
}

struct SlowMsg(u32);
impl Message for SlowMsg {
    type Reply = ();
}

#[async_trait]
impl Handler<SlowMsg> for SlowActor {
    async fn handle(&mut self, msg: SlowMsg, _ctx: &mut ActorContext) {
        println!("  [SlowActor] processing message #{}", msg.0);
        // Simulate slow processing so the mailbox fills up.
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Bounded Mailbox Example ===\n");

    let runtime = TestRuntime::new();

    // --- RejectWithError strategy ---
    println!("--- Bounded(5) + RejectWithError ---");
    let reject_actor = runtime.spawn_with_options::<SlowActor>(
        "reject-actor",
        (),
        SpawnOptions {
            mailbox: MailboxConfig::Bounded {
                capacity: 5,
                overflow: OverflowStrategy::RejectWithError,
            },
            ..Default::default()
        },
    ).await.unwrap();

    // The first message starts processing (occupies the actor task).
    reject_actor.tell(SlowMsg(0)).unwrap();
    // Give the actor a moment to pick up message #0 from the channel.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Fill the bounded channel (capacity = 5).
    for i in 1..=5 {
        match reject_actor.tell(SlowMsg(i)) {
            Ok(()) => println!("  message #{} accepted", i),
            Err(e) => println!("  message #{} REJECTED: {}", i, e),
        }
    }

    // The next message should be rejected because the mailbox is full.
    match reject_actor.tell(SlowMsg(99)) {
        Ok(()) => println!("  message #99 accepted (unexpected!)"),
        Err(e) => println!("  message #99 REJECTED: {}", e),
    }

    println!();

    // --- DropNewest strategy ---
    println!("--- Bounded(5) + DropNewest ---");
    let drop_actor = runtime.spawn_with_options::<SlowActor>(
        "drop-actor",
        (),
        SpawnOptions {
            mailbox: MailboxConfig::Bounded {
                capacity: 5,
                overflow: OverflowStrategy::DropNewest,
            },
            ..Default::default()
        },
    ).await.unwrap();

    // First message starts processing.
    drop_actor.tell(SlowMsg(0)).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Fill the mailbox.
    for i in 1..=5 {
        match drop_actor.tell(SlowMsg(i)) {
            Ok(()) => println!("  message #{} accepted", i),
            Err(e) => println!("  message #{} error: {}", i, e),
        }
    }

    // With DropNewest the send returns Ok(()) even when the mailbox is full,
    // but the message is silently dropped.
    match drop_actor.tell(SlowMsg(99)) {
        Ok(()) => println!("  message #99 accepted (silently dropped — mailbox full)"),
        Err(e) => println!("  message #99 error: {}", e),
    }

    println!("\n=== Done ===");
}
