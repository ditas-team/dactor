//! Supervision: watching actors and handling failures.
//!
//! Run with: cargo run --example supervision --features test-support

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorError, ActorRef, Handler};
use dactor::errors::ErrorAction;
use dactor::message::Message;
use dactor::supervision::ChildTerminated;
use dactor::TestRuntime;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Tell the worker to do some work (fire-and-forget).
struct DoWork(String);
impl Message for DoWork {
    type Reply = ();
}

/// Tell the worker to panic (simulates a failure).
struct CrashNow;
impl Message for CrashNow {
    type Reply = ();
}

/// Ask the worker how many tasks it has completed.
struct GetCompleted;
impl Message for GetCompleted {
    type Reply = u64;
}

// ---------------------------------------------------------------------------
// Worker — uses ErrorAction::Resume so it survives panics
// ---------------------------------------------------------------------------

struct Worker {
    completed: u64,
}

impl Actor for Worker {
    type Args = ();
    type Deps = ();

    fn create(_: (), _: ()) -> Self {
        Worker { completed: 0 }
    }

    fn on_error(&mut self, error: &ActorError) -> ErrorAction {
        println!("  [Worker] on_error: {} — resuming", error.message);
        ErrorAction::Resume
    }
}

#[async_trait]
impl Handler<DoWork> for Worker {
    async fn handle(&mut self, msg: DoWork, _ctx: &mut ActorContext) {
        self.completed += 1;
        println!(
            "  [Worker] completed task '{}' (total: {})",
            msg.0, self.completed
        );
    }
}

#[async_trait]
impl Handler<CrashNow> for Worker {
    async fn handle(&mut self, _msg: CrashNow, _ctx: &mut ActorContext) {
        panic!("intentional crash!");
    }
}

#[async_trait]
impl Handler<GetCompleted> for Worker {
    async fn handle(&mut self, _msg: GetCompleted, _ctx: &mut ActorContext) -> u64 {
        self.completed
    }
}

// ---------------------------------------------------------------------------
// Supervisor — watches workers and records ChildTerminated events
// ---------------------------------------------------------------------------

struct Supervisor {
    events: Arc<Mutex<Vec<String>>>,
}

impl Actor for Supervisor {
    type Args = Arc<Mutex<Vec<String>>>;
    type Deps = ();

    fn create(args: Arc<Mutex<Vec<String>>>, _: ()) -> Self {
        Supervisor { events: args }
    }
}

#[async_trait]
impl Handler<ChildTerminated> for Supervisor {
    async fn handle(&mut self, msg: ChildTerminated, _ctx: &mut ActorContext) {
        let reason = msg.reason.as_deref().unwrap_or("graceful shutdown");
        let entry = format!("child '{}' terminated: {}", msg.child_name, reason);
        println!("  [Supervisor] {}", entry);
        self.events.lock().unwrap().push(entry);
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Supervision Example ===\n");

    let runtime = TestRuntime::new();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));

    // Spawn supervisor and worker.
    let supervisor = runtime.spawn::<Supervisor>("supervisor", events.clone()).await.unwrap();
    let worker = runtime.spawn::<Worker>("worker", ()).await.unwrap();

    // Register a death-watch: supervisor watches worker.
    let worker_id = worker.id();
    runtime.watch(&supervisor, worker_id);
    println!("Supervisor is watching worker\n");

    // 1. Worker processes some tasks normally.
    println!("--- Normal work ---");
    worker.tell(DoWork("task-A".into())).unwrap();
    worker.tell(DoWork("task-B".into())).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 2. Worker panics — but ErrorAction::Resume keeps it alive.
    println!("\n--- Worker panic (Resume) ---");
    worker.tell(CrashNow).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 3. Worker still alive — can handle more messages after the panic.
    println!("\n--- Post-panic work ---");
    worker.tell(DoWork("task-C".into())).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let completed = worker.ask(GetCompleted, None).unwrap().await.unwrap();
    println!("  [Main] worker completed {} tasks", completed);

    // 4. Gracefully stop the worker — supervisor receives ChildTerminated.
    println!("\n--- Stopping worker ---");
    worker.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 5. Print supervisor events.
    println!("\n--- Supervisor events ---");
    for e in events.lock().unwrap().iter() {
        println!("  {}", e);
    }

    println!("\n=== Done ===");
}
