//! Cancellation tokens and cancel_after() for early stream termination.
//!
//! Run with: cargo run --example cancellation --features test-support

use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{cancel_after, Actor, ActorContext, ActorRef, Handler, StreamHandler};
use dactor::message::Message;
use dactor::stream::StreamSender;
use dactor::TestRuntime;
use tokio_stream::StreamExt;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct GetNumbers;
impl Message for GetNumbers {
    type Reply = u32;
}

struct Ping;
impl Message for Ping {
    type Reply = String;
}

// ---------------------------------------------------------------------------
// Actor that streams numbers slowly
// ---------------------------------------------------------------------------

struct SlowProducer;

impl Actor for SlowProducer {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        SlowProducer
    }
}

#[async_trait]
impl StreamHandler<GetNumbers> for SlowProducer {
    async fn handle_stream(
        &mut self,
        _msg: GetNumbers,
        sender: StreamSender<u32>,
        _ctx: &mut ActorContext,
    ) {
        for i in 1..=10 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if sender.send(i).await.is_err() {
                println!("  [Producer] consumer disconnected at item {}", i);
                break;
            }
            println!("  [Producer] sent item {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// Actor that checks ctx.cancelled() inside a handler
// ---------------------------------------------------------------------------

struct CancellableWorker;

impl Actor for CancellableWorker {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        CancellableWorker
    }
}

#[async_trait]
impl Handler<Ping> for CancellableWorker {
    async fn handle(&mut self, _msg: Ping, ctx: &mut ActorContext) -> String {
        // Race the cancellation token against a long task
        tokio::select! {
            _ = ctx.cancelled() => {
                println!("  [Worker] cancelled!");
                "cancelled".to_string()
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                "completed".to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Cancellation Example ===\n");

    let runtime = TestRuntime::new();

    // --- Part 1: cancel_after() terminates a stream early ---
    println!("--- Stream with cancel_after(150ms) ---");
    let producer = runtime.spawn::<SlowProducer>("producer", ());

    let token = cancel_after(Duration::from_millis(150));
    let mut stream = producer
        .stream(GetNumbers, 16, None, Some(token))
        .unwrap();

    let mut received = Vec::new();
    while let Some(n) = stream.next().await {
        received.push(n);
        println!("  [Client] received: {}", n);
    }

    println!("  [Client] stream ended — received {} items", received.len());
    assert!(
        received.len() < 10,
        "expected early termination but got all 10 items"
    );
    println!("  ✓ stream terminated early (< 10 items)\n");

    // --- Part 2: ctx.cancelled() inside a handler ---
    println!("--- Handler with ctx.cancelled() ---");
    let worker = runtime.spawn::<CancellableWorker>("worker", ());

    let token = cancel_after(Duration::from_millis(50));
    let result = worker.ask(Ping, Some(token)).unwrap().await.unwrap();
    println!("  [Client] handler returned: {}", result);
    assert_eq!(result, "cancelled");
    println!("  ✓ handler detected cancellation\n");

    println!("=== Done ===");
}
