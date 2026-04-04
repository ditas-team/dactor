//! Showcase: multiple dactor features working together.
//!
//! Demonstrates actor pool, metrics, circuit breaker, dead letters,
//! drop observer, streaming, feed, send_after, and cancel_after.
//!
//! Run with: cargo run --example showcase --features test-support

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{
    cancel_after, Actor, ActorContext, ActorRef, FeedHandler, Handler, StreamHandler,
};
use dactor::circuit_breaker::CircuitBreakerInterceptor;
use dactor::dead_letter::CollectingDeadLetterHandler;
use dactor::interceptor::{DropNotice, DropObserver};
use dactor::message::Message;
use dactor::pool::{PoolRef, PoolRouting};
use dactor::stream::{StreamReceiver, StreamSender};
use dactor::timer::send_after;
use dactor::{SpawnOptions, TestActorRef, TestRuntime};
use tokio_stream::StreamExt;

// ── Messages ────────────────────────────────────────────────────────────────

struct ProcessTask(String);
impl Message for ProcessTask {
    type Reply = ();
}

struct GetStatus;
impl Message for GetStatus {
    type Reply = String;
}

struct StreamItems;
impl Message for StreamItems {
    type Reply = u32;
}

// ── Actor ───────────────────────────────────────────────────────────────────

struct TaskProcessor {
    id: usize,
    processed: usize,
}

impl Actor for TaskProcessor {
    type Args = usize;
    type Deps = ();
    fn create(id: usize, _: ()) -> Self {
        Self { id, processed: 0 }
    }
}

#[async_trait]
impl Handler<ProcessTask> for TaskProcessor {
    async fn handle(&mut self, msg: ProcessTask, _ctx: &mut ActorContext) {
        self.processed += 1;
        println!("  [Worker-{}] processed: {}", self.id, msg.0);
    }
}

#[async_trait]
impl Handler<GetStatus> for TaskProcessor {
    async fn handle(&mut self, _msg: GetStatus, _ctx: &mut ActorContext) -> String {
        format!("worker-{}: {} tasks done", self.id, self.processed)
    }
}

#[async_trait]
impl StreamHandler<StreamItems> for TaskProcessor {
    async fn handle_stream(
        &mut self,
        _msg: StreamItems,
        sender: StreamSender<u32>,
        _ctx: &mut ActorContext,
    ) {
        for i in 1..=5 {
            if sender.send(i).await.is_err() {
                break;
            }
        }
    }
}

#[async_trait]
impl FeedHandler<u64, u64> for TaskProcessor {
    async fn handle_feed(&mut self, mut rx: StreamReceiver<u64>, _ctx: &mut ActorContext) -> u64 {
        let mut sum = 0u64;
        while let Some(n) = rx.recv().await {
            sum += n;
        }
        sum
    }
}

// ── DropObserver ────────────────────────────────────────────────────────────

struct LoggingDropObserver {
    drops: Arc<Mutex<Vec<String>>>,
}

impl DropObserver for LoggingDropObserver {
    fn on_drop(&self, notice: DropNotice) {
        let msg = format!(
            "{} dropped by {}",
            notice.message_type, notice.interceptor_name
        );
        self.drops.lock().unwrap().push(msg);
    }
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    println!("=== dactor Showcase ===\n");

    // --- Setup runtime with metrics, dead letters, drop observer ---
    let mut runtime = TestRuntime::new();
    runtime.enable_metrics();

    let dead_letters = Arc::new(CollectingDeadLetterHandler::new());
    runtime.set_dead_letter_handler(dead_letters.clone());

    let drop_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    runtime.set_drop_observer(Arc::new(LoggingDropObserver {
        drops: drop_log.clone(),
    }));

    // --- Spawn 4 workers with CircuitBreakerInterceptor ---
    let mut workers: Vec<TestActorRef<TaskProcessor>> = Vec::new();
    for i in 0..4 {
        let mut opts = SpawnOptions::default();
        opts.interceptors
            .push(Box::new(CircuitBreakerInterceptor::new(
                3,                       // threshold: 3 errors to trip
                Duration::from_secs(60), // within 60s window
                Duration::from_secs(10), // 10s cooldown
            )));
        let w = runtime.spawn_with_options::<TaskProcessor>(&format!("worker-{i}"), i, opts);
        workers.push(w);
    }
    let pool = PoolRef::new(workers.clone(), PoolRouting::RoundRobin);

    // --- 1. Tell (fire-and-forget) ---
    println!("--- Tell: distributing tasks across pool ---");
    for i in 0..8 {
        pool.tell(ProcessTask(format!("task-{i}"))).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    // --- 2. Ask (request-reply) ---
    println!("\n--- Ask: query worker status ---");
    let status = workers[0].ask(GetStatus, None).unwrap().await.unwrap();
    println!("  {status}");

    // --- 3. Stream (server-streaming) ---
    println!("\n--- Stream: server-streaming items ---");
    let mut stream = workers[1].stream(StreamItems, 8, None, None).unwrap();
    let mut items = Vec::new();
    while let Some(n) = stream.next().await {
        items.push(n);
    }
    println!("  Received {} items: {:?}", items.len(), items);

    // --- 4. Feed (client-streaming) ---
    println!("\n--- Feed: client-streaming sum ---");
    let input = futures::stream::iter(vec![10u64, 20, 30]);
    let sum = workers[2]
        .feed::<u64, u64>(Box::pin(input), 8, None, None)
        .unwrap()
        .await
        .unwrap();
    println!("  Feed result (sum): {sum}");

    // --- 5. send_after (delayed message) ---
    println!("\n--- send_after: delayed task ---");
    send_after::<TaskProcessor, _, _>(
        &workers[3],
        ProcessTask("delayed-task".into()),
        Duration::from_millis(100),
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    // --- 6. cancel_after (cancellable ask) ---
    println!("\n--- cancel_after: cancellable ask ---");
    let token = cancel_after(Duration::from_millis(200));
    let result = workers[0].ask(GetStatus, Some(token)).unwrap().await;
    println!("  Cancellable ask result: {:?}", result.unwrap());

    // --- 7. Dead letters: send to stopped actor ---
    println!("\n--- Dead letters: sending to stopped actor ---");
    workers[3].stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = workers[3].tell(ProcessTask("after-stop".into()));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // === Summary ============================================================
    println!("\n========================================");
    println!("            SUMMARY");
    println!("========================================");

    // Metrics
    let registry = runtime.metrics().unwrap();
    let rt_metrics = registry.runtime_metrics();
    println!("\n  Metrics:");
    println!("    Total messages: {}", rt_metrics.total_messages);
    println!("    Total errors:   {}", rt_metrics.total_errors);
    println!("    Message rate:   {:.1}/s", rt_metrics.message_rate);
    println!("    Error rate:     {:.1}/s", rt_metrics.error_rate);
    println!("    Actor count:    {}", rt_metrics.actor_count);

    // Per-actor breakdown
    println!("\n  Per-actor breakdown:");
    for (actor_id, snap) in registry.all() {
        println!(
            "    {:?}: {} msgs, {} errs, {:.1} msg/s",
            actor_id, snap.message_count, snap.error_count, snap.message_rate
        );
    }

    // Dead letters
    println!("\n  Dead letters: {}", dead_letters.count());
    for dl in dead_letters.events() {
        println!(
            "    target={:?} msg={} reason={:?}",
            dl.target_id, dl.message_type, dl.reason
        );
    }

    // Circuit breaker (no errors → all circuits remain Closed)
    println!("\n  Circuit breaker: all Closed (0 errors < threshold 3)");

    // Drop observer
    let drops = drop_log.lock().unwrap();
    println!("\n  Drops observed: {}", drops.len());
    for d in drops.iter() {
        println!("    {d}");
    }

    println!("\n=== Done ===");
}
