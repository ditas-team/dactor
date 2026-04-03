//! MetricsInterceptor and MetricsStore for actor observability.
//!
//! Run with: cargo run --example metrics --features test-support

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::mailbox::MailboxConfig;
use dactor::message::Message;
use dactor::metrics::{MetricsInterceptor, MetricsStore};
use dactor::{SpawnOptions, TestRuntime};

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct Increment(u64);
impl Message for Increment {
    type Reply = ();
}

struct GetCount;
impl Message for GetCount {
    type Reply = u64;
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Counter {
    count: u64,
}

impl Actor for Counter {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Counter { count: 0 }
    }
}

#[async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

#[async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Metrics Example ===\n");

    let store = MetricsStore::new(100);
    let runtime = TestRuntime::new();

    // Spawn actor with MetricsInterceptor
    let counter = runtime.spawn_with_options::<Counter>(
        "counter",
        (),
        SpawnOptions {
            interceptors: vec![Box::new(MetricsInterceptor::new(store.clone()))],
            mailbox: MailboxConfig::Unbounded,
        },
    );

    // Send several tell messages
    println!("--- Sending 5 Increment tells ---");
    for i in 1..=5 {
        counter.tell(Increment(i)).unwrap();
    }

    // Send ask messages
    println!("--- Sending 2 GetCount asks ---");
    let count = counter.ask(GetCount, None).unwrap().await.unwrap();
    println!("  count after increments: {}", count);
    assert_eq!(count, 15); // 1+2+3+4+5

    let count2 = counter.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count2, 15);

    // Wait briefly for all interceptor callbacks to complete
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Query metrics
    println!("\n--- Metrics Store ---");
    println!("  total messages: {}", store.total_messages());
    println!("  total errors:   {}", store.total_errors());
    println!("  actor count:    {}", store.actor_count());
    assert!(store.total_messages() >= 7); // 5 tells + 2 asks
    assert_eq!(store.total_errors(), 0);
    assert_eq!(store.actor_count(), 1);

    // Per-actor breakdown
    let all = store.all();
    for (actor_id, m) in &all {
        println!("\n  Actor {:?}:", actor_id);
        println!("    message_count: {}", m.message_count);
        println!("    error_count:   {}", m.error_count);
        for (msg_type, count) in &m.message_counts_by_type {
            println!("    {}: {} messages", msg_type, count);
        }
        if let Some(avg) = m.avg_latency() {
            println!("    avg latency:   {:?}", avg);
        }
    }

    println!("\n  ✓ metrics collected successfully");
    println!("\n=== Done ===");
}
