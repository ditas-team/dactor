//! Streaming patterns: server-streaming (stream) and client-streaming (feed).
//!
//! Run with: cargo run --example streaming --features test-support

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, ReduceHandler, ExpandHandler};
use dactor::message::Message;
use dactor::stream::{StreamReceiver, StreamSender};
use dactor::TestRuntime;
use tokio_stream::StreamExt;

// ===========================================================================
// Part 1 — Server-streaming with ExpandHandler
// ===========================================================================

/// Request to stream all log entries.
struct GetLogs;
impl Message for GetLogs {
    type Reply = String; // each streamed item is a String
}

struct LogServer {
    logs: Vec<String>,
}

impl Actor for LogServer {
    type Args = Vec<String>;
    type Deps = ();

    fn create(args: Vec<String>, _deps: ()) -> Self {
        LogServer { logs: args }
    }
}

#[async_trait]
impl ExpandHandler<GetLogs, String> for LogServer {
    async fn handle_expand(
        &mut self,
        _msg: GetLogs,
        sender: StreamSender<String>,
        _ctx: &mut ActorContext,
    ) {
        // Push each log entry into the stream; stop if consumer disconnects.
        for log in &self.logs {
            if sender.send(log.clone()).await.is_err() {
                break;
            }
        }
    }
}

// ===========================================================================
// Part 2 — Client-streaming with ReduceHandler
// ===========================================================================

/// Feed message: caller streams u64 items, actor returns the sum.
struct Aggregator;

impl Actor for Aggregator {
    type Args = ();
    type Deps = ();

    fn create(_args: (), _deps: ()) -> Self {
        Aggregator
    }
}

#[async_trait]
impl ReduceHandler<u64, u64> for Aggregator {
    async fn handle_reduce(
        &mut self,
        mut receiver: StreamReceiver<u64>,
        _ctx: &mut ActorContext,
    ) -> u64 {
        let mut total = 0u64;
        while let Some(n) = receiver.recv().await {
            println!("  [Aggregator] received item: {}", n);
            total += n;
        }
        total
    }
}

// ===========================================================================
// Main
// ===========================================================================

#[tokio::main]
async fn main() {
    println!("=== Streaming Example ===\n");

    let runtime = TestRuntime::new();

    // --- Server-streaming (stream) ---
    println!("--- Server-streaming: LogServer ---");
    let server = runtime.spawn::<LogServer>(
        "log-server",
        vec![
            "2025-01-01 INFO  boot".into(),
            "2025-01-01 WARN  slow query".into(),
            "2025-01-01 ERROR disk full".into(),
        ],
    ).await.unwrap();

    let mut stream = server.expand(GetLogs, 16, None, None).unwrap();
    while let Some(entry) = stream.next().await {
        println!("  [Client] log entry: {}", entry);
    }
    println!("  [Client] stream closed\n");

    // --- Client-streaming (feed) ---
    println!("--- Client-streaming: Aggregator ---");
    let aggregator = runtime.spawn::<Aggregator>("aggregator", ()).await.unwrap();

    let input = futures::stream::iter(vec![10u64, 20, 30, 40, 50]);
    let total = aggregator
        .reduce::<u64, u64>(Box::pin(input), 8, None, None)
        .unwrap()
        .await
        .unwrap();
    println!("  [Client] feed result (sum): {}\n", total);

    println!("=== Done ===");
}

