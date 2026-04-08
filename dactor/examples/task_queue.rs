//! Distributed task queue: workers, priorities, retries, and dead-letter routing.
//!
//! Demonstrates actor pools, tell/ask, interceptors, lifecycle hooks, timers,
//! bounded mailboxes, and metrics — all wired into a realistic task pipeline.
//!
//! Run with:
//!   cargo run --example task_queue -p dactor --features test-support,metrics
//!
//! Architecture:
//!
//! ```text
//! Client ──tell──▶ Dispatcher ──tell──▶ WorkerPool (RoundRobin, 3 workers)
//!                      ▲                     │
//!                      │          success ◀───┘
//!                      │            │
//!                      │            ▼
//!                      │       MetricsActor ◀── GetMetrics (ask)
//!                      │
//!                      │          failure ◀───┘
//!                      │            │
//!                      │     retry? ├──▶ back to WorkerPool
//!                      │            │
//!                      │     max    └──▶ DeadLetterCollector
//!                      │
//!        Timer ────────┘  (periodic metrics report)
//! ```

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::interceptor::{
    Disposition, InboundContext, InboundInterceptor, Outcome,
};
use dactor::mailbox::{MailboxConfig, OverflowStrategy};
use dactor::message::{Headers, Message, RuntimeHeaders};
use dactor::pool::{PoolRef, PoolRouting};
use dactor::{ErrorAction, SpawnOptions, TestActorRef, TestRuntime};

// ═══════════════════════════════════════════════════════════════════════════
// Messages
// ═══════════════════════════════════════════════════════════════════════════

/// Client submits a task to the Dispatcher.
struct SubmitTask {
    id: String,
    payload: String,
    priority: TaskPriority,
    max_retries: u32,
}
impl Message for SubmitTask {
    type Reply = ();
}

/// Dispatcher sends work to a Worker.
#[derive(Clone)]
struct ProcessTask {
    id: String,
    payload: String,
    attempt: u32,
    max_retries: u32,
}
impl Message for ProcessTask {
    type Reply = ();
}

/// Worker reports success back to the Dispatcher.
struct TaskCompleted {
    id: String,
    result: String,
}
impl Message for TaskCompleted {
    type Reply = ();
}

/// Worker reports failure back to the Dispatcher.
struct TaskFailed {
    id: String,
    payload: String,
    error: String,
    attempt: u32,
    max_retries: u32,
}
impl Message for TaskFailed {
    type Reply = ();
}

/// Permanently failed task routed to the DeadLetterCollector.
struct DeadTask {
    id: String,
    payload: String,
    error: String,
    attempts: u32,
}
impl Message for DeadTask {
    type Reply = ();
}

/// Record a metric event.
#[derive(Clone)]
enum MetricEvent {
    Submitted,
    Completed,
    Retried,
    DeadLettered,
}
struct RecordMetric(MetricEvent);
impl Message for RecordMetric {
    type Reply = ();
}

/// Ask the MetricsActor for a snapshot.
struct GetMetrics;
impl Message for GetMetrics {
    type Reply = MetricsSnapshot;
}

/// Trigger periodic metrics report (sent by timer).
struct PrintMetrics;
impl Message for PrintMetrics {
    type Reply = ();
}

#[derive(Debug, Clone)]
struct MetricsSnapshot {
    submitted: u64,
    completed: u64,
    retried: u64,
    dead_lettered: u64,
}

// Priority is included in the task model for structural completeness.
// This example uses RoundRobin routing; for priority-based routing,
// implement the `Keyed` trait and use `PoolRouting::KeyBased`.
#[derive(Clone, Debug)]
enum TaskPriority {
    High,
    Normal,
    Low,
}

// ═══════════════════════════════════════════════════════════════════════════
// MetricsActor
// ═══════════════════════════════════════════════════════════════════════════

struct MetricsActor {
    submitted: u64,
    completed: u64,
    retried: u64,
    dead_lettered: u64,
}

impl Actor for MetricsActor {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Self { submitted: 0, completed: 0, retried: 0, dead_lettered: 0 }
    }
}

#[async_trait]
impl Handler<RecordMetric> for MetricsActor {
    async fn handle(&mut self, msg: RecordMetric, _ctx: &mut ActorContext) {
        match msg.0 {
            MetricEvent::Submitted => self.submitted += 1,
            MetricEvent::Completed => self.completed += 1,
            MetricEvent::Retried => self.retried += 1,
            MetricEvent::DeadLettered => self.dead_lettered += 1,
        }
    }
}

#[async_trait]
impl Handler<GetMetrics> for MetricsActor {
    async fn handle(&mut self, _msg: GetMetrics, _ctx: &mut ActorContext) -> MetricsSnapshot {
        MetricsSnapshot {
            submitted: self.submitted,
            completed: self.completed,
            retried: self.retried,
            dead_lettered: self.dead_lettered,
        }
    }
}

#[async_trait]
impl Handler<PrintMetrics> for MetricsActor {
    async fn handle(&mut self, _msg: PrintMetrics, _ctx: &mut ActorContext) {
        let total = self.completed + self.dead_lettered;
        let in_flight = self.submitted.saturating_sub(total);
        println!(
            "  [Metrics] submitted={} completed={} retried={} dead={} in-flight={}",
            self.submitted, self.completed, self.retried, self.dead_lettered, in_flight
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DeadLetterCollector
// ═══════════════════════════════════════════════════════════════════════════

struct DeadLetterCollector {
    collected: Vec<(String, String, u32)>,
}

#[async_trait]
impl Actor for DeadLetterCollector {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Self { collected: Vec::new() }
    }

    async fn on_start(&mut self, ctx: &mut ActorContext) {
        println!("  [DeadLetterCollector] started (actor={})", ctx.actor_name);
    }

    async fn on_stop(&mut self) {
        println!(
            "  [DeadLetterCollector] stopped — collected {} dead tasks",
            self.collected.len()
        );
    }
}

#[async_trait]
impl Handler<DeadTask> for DeadLetterCollector {
    async fn handle(&mut self, msg: DeadTask, _ctx: &mut ActorContext) {
        println!(
            "  [DeadLetterCollector] ☠ task={} payload=\"{}\" error=\"{}\" after {} attempts",
            msg.id, msg.payload, msg.error, msg.attempts
        );
        self.collected.push((msg.id, msg.error, msg.attempts));
    }
}

/// Ask how many dead tasks were collected.
struct GetDeadCount;
impl Message for GetDeadCount {
    type Reply = usize;
}

#[async_trait]
impl Handler<GetDeadCount> for DeadLetterCollector {
    async fn handle(&mut self, _msg: GetDeadCount, _ctx: &mut ActorContext) -> usize {
        self.collected.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Worker
// ═══════════════════════════════════════════════════════════════════════════

struct Worker {
    id: usize,
    dispatcher: TestActorRef<Dispatcher>,
    processed: u32,
}

#[async_trait]
impl Actor for Worker {
    type Args = (usize, TestActorRef<Dispatcher>);
    type Deps = ();
    fn create(args: Self::Args, _: ()) -> Self {
        Self { id: args.0, dispatcher: args.1, processed: 0 }
    }

    async fn on_start(&mut self, ctx: &mut ActorContext) {
        println!("  [Worker-{}] started (actor={})", self.id, ctx.actor_name);
    }

    async fn on_stop(&mut self) {
        println!("  [Worker-{}] stopped — processed {} tasks", self.id, self.processed);
    }

    fn on_error(&mut self, _error: &dactor::ActorError) -> ErrorAction {
        ErrorAction::Resume
    }
}

#[async_trait]
impl Handler<ProcessTask> for Worker {
    async fn handle(&mut self, msg: ProcessTask, _ctx: &mut ActorContext) {
        println!(
            "  [Worker-{}] processing task={} attempt={}/{}",
            self.id, msg.id, msg.attempt, msg.max_retries + 1
        );

        // Simulate work
        tokio::time::sleep(Duration::from_millis(20)).await;

        let outcome = simulate_task(&msg.payload, msg.attempt);

        match outcome {
            Ok(result) => {
                self.processed += 1;
                println!("  [Worker-{}] ✓ task={} result=\"{}\"", self.id, msg.id, result);
                let _ = self.dispatcher.tell(TaskCompleted {
                    id: msg.id,
                    result,
                });
            }
            Err(error) => {
                println!("  [Worker-{}] ✗ task={} error=\"{}\"", self.id, msg.id, error);
                let _ = self.dispatcher.tell(TaskFailed {
                    id: msg.id,
                    payload: msg.payload,
                    error,
                    attempt: msg.attempt,
                    max_retries: msg.max_retries,
                });
            }
        }
    }
}

/// Simulate task processing:
/// - "easy:*"       → always succeeds
/// - "hard:*"       → fails on first attempt, then succeeds
/// - "impossible:*" → always fails
fn simulate_task(payload: &str, attempt: u32) -> Result<String, String> {
    if let Some(task) = payload.strip_prefix("easy:") {
        Ok(format!("processed-{}", task))
    } else if let Some(task) = payload.strip_prefix("hard:") {
        if attempt <= 1 {
            Err("transient error".into())
        } else {
            Ok(format!("recovered-{}", task))
        }
    } else if payload.starts_with("impossible:") {
        Err("permanent failure".into())
    } else {
        Ok(format!("done-{payload}"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dispatcher
// ═══════════════════════════════════════════════════════════════════════════

struct Dispatcher {
    pool: Option<PoolRef<Worker, TestActorRef<Worker>>>,
    dead_letter_collector: TestActorRef<DeadLetterCollector>,
    metrics: TestActorRef<MetricsActor>,
}

#[async_trait]
impl Actor for Dispatcher {
    type Args = (TestActorRef<DeadLetterCollector>, TestActorRef<MetricsActor>);
    type Deps = ();
    fn create(args: Self::Args, _: ()) -> Self {
        Self {
            pool: None,
            dead_letter_collector: args.0,
            metrics: args.1,
        }
    }

    async fn on_start(&mut self, ctx: &mut ActorContext) {
        println!("  [Dispatcher] started (actor={})", ctx.actor_name);
    }

    async fn on_stop(&mut self) {
        println!("  [Dispatcher] stopped");
    }
}

/// Dispatcher-only message to attach the worker pool after spawning.
struct SetPool(PoolRef<Worker, TestActorRef<Worker>>);
impl Message for SetPool {
    type Reply = ();
}

#[async_trait]
impl Handler<SetPool> for Dispatcher {
    async fn handle(&mut self, msg: SetPool, _ctx: &mut ActorContext) {
        self.pool = Some(msg.0);
        println!("  [Dispatcher] worker pool attached");
    }
}

#[async_trait]
impl Handler<SubmitTask> for Dispatcher {
    async fn handle(&mut self, msg: SubmitTask, _ctx: &mut ActorContext) {
        println!(
            "  [Dispatcher] received task={} priority={:?}",
            msg.id, msg.priority
        );
        let _ = self.metrics.tell(RecordMetric(MetricEvent::Submitted));

        if let Some(pool) = &self.pool {
            let _ = pool.tell(ProcessTask {
                id: msg.id,
                payload: msg.payload,
                attempt: 1,
                max_retries: msg.max_retries,
            });
        }
    }
}

#[async_trait]
impl Handler<TaskCompleted> for Dispatcher {
    async fn handle(&mut self, msg: TaskCompleted, _ctx: &mut ActorContext) {
        println!("  [Dispatcher] task={} completed result=\"{}\"", msg.id, msg.result);
        let _ = self.metrics.tell(RecordMetric(MetricEvent::Completed));
    }
}

#[async_trait]
impl Handler<TaskFailed> for Dispatcher {
    async fn handle(&mut self, msg: TaskFailed, _ctx: &mut ActorContext) {
        if msg.attempt < msg.max_retries + 1 {
            // Retry
            println!(
                "  [Dispatcher] retrying task={} (attempt {}/{})",
                msg.id,
                msg.attempt + 1,
                msg.max_retries + 1
            );
            let _ = self.metrics.tell(RecordMetric(MetricEvent::Retried));
            if let Some(pool) = &self.pool {
                let _ = pool.tell(ProcessTask {
                    id: msg.id,
                    payload: msg.payload,
                    attempt: msg.attempt + 1,
                    max_retries: msg.max_retries,
                });
            }
        } else {
            // Dead-letter
            println!(
                "  [Dispatcher] task={} exhausted retries — routing to dead-letter",
                msg.id
            );
            let _ = self.metrics.tell(RecordMetric(MetricEvent::DeadLettered));
            let _ = self.dead_letter_collector.tell(DeadTask {
                id: msg.id,
                payload: msg.payload,
                error: msg.error,
                attempts: msg.attempt,
            });
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Inbound Interceptor — logs every message arriving at workers
// ═══════════════════════════════════════════════════════════════════════════

struct WorkerLoggingInterceptor {
    log: Arc<Mutex<Vec<String>>>,
}

impl InboundInterceptor for WorkerLoggingInterceptor {
    fn name(&self) -> &'static str {
        "worker-logging"
    }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        let entry = format!("[{}] ← {}", ctx.actor_name, ctx.message_type);
        self.log.lock().unwrap().push(entry);
        Disposition::Continue
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let status = match outcome {
            Outcome::TellSuccess | Outcome::AskSuccess { .. } => "ok",
            _ => "err",
        };
        let entry = format!("[{}] → {} ({})", ctx.actor_name, ctx.message_type, status);
        self.log.lock().unwrap().push(entry);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║        dactor Task Queue — Distributed Work Pipeline       ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ── Setup runtime with metrics ──────────────────────────────────────
    let mut runtime = TestRuntime::new();
    runtime.enable_metrics();

    // ── Spawn support actors ────────────────────────────────────────────
    println!("── Spawning actors ──────────────────────────────────────────\n");

    let metrics_actor = runtime.spawn::<MetricsActor>("metrics", ()).await.unwrap();
    let dead_letter_collector = runtime
        .spawn::<DeadLetterCollector>("dead-letter-collector", ())
        .await
        .unwrap();

    // ── Spawn Dispatcher ────────────────────────────────────────────────
    let dispatcher = runtime
        .spawn::<Dispatcher>(
            "dispatcher",
            (dead_letter_collector.clone(), metrics_actor.clone()),
        )
        .await
        .unwrap();

    // ── Spawn Worker pool with bounded mailbox + logging interceptor ────
    let interceptor_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut workers: Vec<TestActorRef<Worker>> = Vec::new();

    for i in 0..3 {
        let worker = runtime
            .spawn_with_options::<Worker>(
                &format!("worker-{i}"),
                (i, dispatcher.clone()),
                SpawnOptions {
                    interceptors: vec![Box::new(WorkerLoggingInterceptor {
                        log: interceptor_log.clone(),
                    })],
                    mailbox: MailboxConfig::Bounded {
                        capacity: 10,
                        overflow: OverflowStrategy::RejectWithError,
                    },
                },
            )
            .await
            .unwrap();
        workers.push(worker);
    }

    let pool = PoolRef::new(workers.clone(), PoolRouting::RoundRobin);
    dispatcher.tell(SetPool(pool)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ── Set up periodic metrics timer ───────────────────────────────────
    let cancel_token = dactor::CancellationToken::new();
    dactor::timer::send_interval(
        &metrics_actor,
        || PrintMetrics,
        Duration::from_millis(500),
        cancel_token.clone(),
    );

    // ── Submit tasks ────────────────────────────────────────────────────
    println!("\n── Submitting 20 tasks ─────────────────────────────────────\n");

    let tasks: Vec<(&str, &str, TaskPriority, u32)> = vec![
        // Easy tasks — always succeed (10 tasks)
        ("task-01", "easy:send-email",        TaskPriority::High,   2),
        ("task-02", "easy:resize-image",      TaskPriority::Normal, 2),
        ("task-03", "easy:generate-report",   TaskPriority::Low,    2),
        ("task-04", "easy:send-notification", TaskPriority::Normal, 2),
        ("task-05", "easy:update-cache",      TaskPriority::High,   2),
        ("task-06", "easy:sync-data",         TaskPriority::Normal, 2),
        ("task-07", "easy:log-analytics",     TaskPriority::Low,    2),
        ("task-08", "easy:compress-file",     TaskPriority::Normal, 2),
        ("task-09", "easy:validate-input",    TaskPriority::High,   2),
        ("task-10", "easy:render-template",   TaskPriority::Normal, 2),
        // Hard tasks — fail once, then succeed (5 tasks)
        ("task-11", "hard:process-payment",   TaskPriority::High,   3),
        ("task-12", "hard:upload-file",       TaskPriority::Normal, 3),
        ("task-13", "hard:call-api",          TaskPriority::Normal, 3),
        ("task-14", "hard:index-document",    TaskPriority::Low,    3),
        ("task-15", "hard:transform-data",    TaskPriority::High,   3),
        // Impossible tasks — always fail (5 tasks)
        ("task-16", "impossible:bad-endpoint",     TaskPriority::Normal, 2),
        ("task-17", "impossible:corrupt-data",     TaskPriority::High,   2),
        ("task-18", "impossible:missing-resource", TaskPriority::Normal, 1),
        ("task-19", "impossible:auth-expired",     TaskPriority::Low,    2),
        ("task-20", "impossible:quota-exceeded",   TaskPriority::Normal, 1),
    ];

    for (id, payload, priority, max_retries) in &tasks {
        dispatcher
            .tell(SubmitTask {
                id: id.to_string(),
                payload: payload.to_string(),
                priority: priority.clone(),
                max_retries: *max_retries,
            })
            .unwrap();
    }

    // ── Wait for processing ─────────────────────────────────────────────
    println!("\n── Processing ─────────────────────────────────────────────\n");
    // Allow time for all retries and dead-letter routing to complete.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Cancel the periodic timer
    cancel_token.cancel();

    // ── Final metrics ───────────────────────────────────────────────────
    println!("\n── Final Report ───────────────────────────────────────────\n");

    let snap = metrics_actor.ask(GetMetrics, None).unwrap().await.unwrap();
    println!("  Tasks submitted:    {}", snap.submitted);
    println!("  Tasks completed:    {}", snap.completed);
    println!("  Retries performed:  {}", snap.retried);
    println!("  Dead-lettered:      {}", snap.dead_lettered);

    let success_rate = if snap.submitted > 0 {
        (snap.completed as f64 / snap.submitted as f64) * 100.0
    } else {
        0.0
    };
    println!("  Success rate:       {:.0}%", success_rate);

    // Dead letter count
    let dead_count = dead_letter_collector
        .ask(GetDeadCount, None)
        .unwrap()
        .await
        .unwrap();
    println!("  Dead letter queue:  {} tasks", dead_count);

    // Runtime-level metrics
    let registry = runtime.metrics().unwrap();
    let rt_metrics = registry.runtime_metrics();
    println!("\n  Runtime metrics:");
    println!("    Total messages processed: {}", rt_metrics.total_messages);
    println!("    Total errors:             {}", rt_metrics.total_errors);
    println!("    Active actors:            {}", rt_metrics.actor_count);
    println!("    Message rate:             {:.1}/s", rt_metrics.message_rate);

    // Interceptor log summary
    {
        let log = interceptor_log.lock().unwrap();
        println!("\n  Interceptor log: {} entries recorded", log.len());
        if !log.is_empty() {
            println!("    First: {}", log.first().unwrap());
            println!("    Last:  {}", log.last().unwrap());
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────────
    println!("\n── Shutting down ──────────────────────────────────────────\n");

    for w in &workers {
        w.stop();
    }
    dispatcher.stop();
    dead_letter_collector.stop();
    metrics_actor.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  Done! All actors stopped cleanly.");
    println!("══════════════════════════════════════════════════════════════");
}
