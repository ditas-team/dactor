//! MetricsInterceptor and MetricsRegistry for actor observability.
//!
//! Run with: cargo run --example metrics --features test-support

use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::TestRuntime;

use async_trait::async_trait;

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

    let mut runtime = TestRuntime::new();
    runtime.enable_metrics();

    // Spawn actor — MetricsInterceptor is automatically attached
    let counter = runtime.spawn::<Counter>("counter", ()).await.unwrap();

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

    // Query metrics via the registry
    let registry = runtime.metrics().unwrap();
    println!("\n--- Metrics Registry ---");
    println!("  total messages: {}", registry.total_messages());
    println!("  total errors:   {}", registry.total_errors());
    println!("  actor count:    {}", registry.actor_count());
    assert!(registry.total_messages() >= 7); // 5 tells + 2 asks
    assert_eq!(registry.total_errors(), 0);
    assert_eq!(registry.actor_count(), 1);

    // Per-actor breakdown via snapshots
    let all = registry.all();
    for (actor_id, snap) in &all {
        println!("\n  Actor {:?}:", actor_id);
        println!("    message_count: {}", snap.message_count);
        println!("    error_count:   {}", snap.error_count);
        println!("    message_rate:  {:.2}/s", snap.message_rate);
        for (msg_type, count) in &snap.message_counts_by_type {
            println!("    {}: {} messages", msg_type, count);
        }
        if let Some(avg) = snap.avg_latency {
            println!("    avg latency:   {:?}", avg);
        }
    }

    println!("\n  ✓ metrics collected successfully");

    // ── Periodic metrics reporting ──────────────────────────
    // Applications can spawn a background task that periodically
    // queries the MetricsRegistry and logs/emits the windowed metrics.
    println!("\n--- Periodic Metrics Reporting (3 intervals) ---");

    let report_registry = registry.clone();
    let report_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        for tick in 1..=3 {
            interval.tick().await;
            let snapshot = report_registry.runtime_metrics();
            println!(
                "  [tick {}] actors={} msgs={} errs={} msg_rate={:.1}/s err_rate={:.1}/s window={:?}",
                tick,
                snapshot.actor_count,
                snapshot.total_messages,
                snapshot.total_errors,
                snapshot.message_rate,
                snapshot.error_rate,
                snapshot.window,
            );
        }
    });

    // Meanwhile, send more messages to show the windowed view updating
    for i in 1..=3 {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        counter.tell(Increment(i)).unwrap();
    }

    report_handle.await.unwrap();
    println!("  ✓ periodic reporting complete");

    println!("\n=== Done ===");
}
