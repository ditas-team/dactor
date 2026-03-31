//! Basic counter actor demonstrating tell (fire-and-forget) and ask (request-reply).
//!
//! Run with: cargo run --example basic_counter --features test-support

use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, Handler, TypedActorRef};
use dactor::message::Message;
use dactor::V2TestRuntime;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Fire-and-forget: increment the counter by a given amount.
struct Increment(u64);
impl Message for Increment {
    type Reply = ();
}

/// Request-reply: ask the counter for its current value.
struct GetCount;
impl Message for GetCount {
    type Reply = u64;
}

/// Request-reply: reset the counter and return the old value.
struct Reset;
impl Message for Reset {
    type Reply = u64;
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Counter {
    count: u64,
}

impl Actor for Counter {
    type Args = Self;
    type Deps = ();

    fn create(args: Self, _deps: ()) -> Self {
        args
    }
}

#[async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
        println!("  [Counter] incremented by {} → count = {}", msg.0, self.count);
    }
}

#[async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

#[async_trait]
impl Handler<Reset> for Counter {
    async fn handle(&mut self, _msg: Reset, _ctx: &mut ActorContext) -> u64 {
        let old = self.count;
        self.count = 0;
        println!("  [Counter] reset (old value = {})", old);
        old
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Basic Counter Example ===\n");

    // 1. Create the runtime and spawn the actor.
    let runtime = V2TestRuntime::new();
    let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });
    println!("Spawned actor '{}'", counter.name());

    // 2. Send some fire-and-forget increments via tell().
    println!("\nSending tell(Increment) messages...");
    counter.tell(Increment(5)).unwrap();
    counter.tell(Increment(3)).unwrap();
    counter.tell(Increment(2)).unwrap();

    // Give the actor a moment to process the tells.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Ask for the current count via ask().
    let count = counter.ask(GetCount).unwrap().await.unwrap();
    println!("\nask(GetCount) → {}", count);

    // 4. Reset and get the old value back.
    let old = counter.ask(Reset).unwrap().await.unwrap();
    println!("ask(Reset)    → old value was {}", old);

    let count = counter.ask(GetCount).unwrap().await.unwrap();
    println!("ask(GetCount) → {} (after reset)", count);

    // 5. Stop the actor gracefully.
    counter.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    println!("\nActor alive? {}", counter.is_alive());

    println!("\n=== Done ===");
}
