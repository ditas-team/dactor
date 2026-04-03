//! Actor pool: distribute work across workers with routing strategies.
//!
//! Demonstrates `PoolRef` with `RoundRobin` and `KeyBased` routing,
//! including `tell_keyed` / `ask_keyed` for sticky routing.
//!
//! Run with: cargo run --example actor_pool --features test-support

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::pool::{Keyed, PoolRef, PoolRouting};
use dactor::TestRuntime;

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Worker {
    id: u64,
    processed: Arc<AtomicU64>,
}

impl Actor for Worker {
    type Args = (u64, Arc<AtomicU64>);
    type Deps = ();
    fn create(args: Self::Args, _deps: ()) -> Self {
        Self { id: args.0, processed: args.1 }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct Task;
impl Message for Task { type Reply = (); }

struct WhoHandled;
impl Message for WhoHandled { type Reply = u64; }

struct KeyedTask { key: u64 }
impl Message for KeyedTask { type Reply = u64; }
impl Keyed for KeyedTask {
    fn routing_key(&self) -> u64 { self.key }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[async_trait]
impl Handler<Task> for Worker {
    async fn handle(&mut self, _msg: Task, _ctx: &mut ActorContext) {
        self.processed.fetch_add(1, Ordering::Relaxed);
    }
}

#[async_trait]
impl Handler<WhoHandled> for Worker {
    async fn handle(&mut self, _msg: WhoHandled, _ctx: &mut ActorContext) -> u64 {
        self.id
    }
}

#[async_trait]
impl Handler<KeyedTask> for Worker {
    async fn handle(&mut self, _msg: KeyedTask, _ctx: &mut ActorContext) -> u64 {
        self.processed.fetch_add(1, Ordering::Relaxed);
        self.id
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_pool(
    rt: &TestRuntime,
    size: usize,
    routing: PoolRouting,
) -> (PoolRef<Worker, dactor::TestActorRef<Worker>>, Vec<Arc<AtomicU64>>) {
    let mut counters = Vec::new();
    let mut workers = Vec::new();
    for i in 0..size {
        let ctr = Arc::new(AtomicU64::new(0));
        counters.push(ctr.clone());
        workers.push(rt.spawn::<Worker>(&format!("w-{i}"), (i as u64, ctr)));
    }
    (PoolRef::new(workers, routing), counters)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Actor Pool Example ===\n");
    let rt = TestRuntime::new();

    // ── RoundRobin ─────────────────────────────────────────
    println!("--- RoundRobin (4 workers, 12 tasks) ---");
    let (pool, counters) = make_pool(&rt, 4, PoolRouting::RoundRobin);
    for _ in 0..12 {
        pool.tell(Task).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    for (i, ctr) in counters.iter().enumerate() {
        let n = ctr.load(Ordering::Relaxed);
        println!("  Worker {i}: handled {n} tasks");
        assert_eq!(n, 3, "each worker should get exactly 3 tasks");
    }
    println!("  Pool size: {}", pool.pool_size());
    assert!(pool.is_alive());
    pool.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!pool.is_alive());
    println!("  Pool stopped.");

    // ── KeyBased ───────────────────────────────────────────
    println!("\n--- KeyBased (4 workers, sticky routing) ---");
    let (pool, _counters) = make_pool(&rt, 4, PoolRouting::KeyBased);

    let mut key_to_worker: HashMap<u64, u64> = HashMap::new();
    for key in [10, 42, 99, 1000] {
        let mut worker_ids = Vec::new();
        for _ in 0..4 {
            let wid = pool.ask_keyed(KeyedTask { key }, None).unwrap().await.unwrap();
            worker_ids.push(wid);
        }
        let first = worker_ids[0];
        assert!(
            worker_ids.iter().all(|&w| w == first),
            "key {key} must always route to the same worker"
        );
        key_to_worker.insert(key, first);
        println!("  Key {key:>4} → Worker {first}");
    }

    pool.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;

    println!("\n=== Done ===");
}
