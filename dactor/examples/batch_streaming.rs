//! Batched streaming: transparent batching for stream and feed channels.
//!
//! Run with: cargo run --example batch_streaming --features test-support

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, FeedHandler, FeedMessage, StreamHandler};
use dactor::message::Message;
use dactor::stream::{BatchConfig, StreamReceiver, StreamSender};
use dactor::TestRuntime;
use tokio_stream::StreamExt;

// ===========================================================================
// Server-streaming actor (unchanged handler — batching is transparent)
// ===========================================================================

struct GetNumbers;
impl Message for GetNumbers {
    type Reply = u64;
}

struct NumberServer {
    count: u64,
}

impl Actor for NumberServer {
    type Args = u64;
    type Deps = ();
    fn create(count: u64, _deps: ()) -> Self {
        NumberServer { count }
    }
}

#[async_trait]
impl StreamHandler<GetNumbers> for NumberServer {
    async fn handle_stream(
        &mut self,
        _msg: GetNumbers,
        sender: StreamSender<u64>,
        _ctx: &mut ActorContext,
    ) {
        for i in 1..=self.count {
            if sender.send(i).await.is_err() {
                break;
            }
        }
    }
}

// ===========================================================================
// Client-streaming actor (unchanged handler)
// ===========================================================================

struct SumItems;
impl FeedMessage for SumItems {
    type Item = u64;
    type Reply = u64;
}

struct Aggregator;

impl Actor for Aggregator {
    type Args = ();
    type Deps = ();
    fn create(_args: (), _deps: ()) -> Self {
        Aggregator
    }
}

#[async_trait]
impl FeedHandler<SumItems> for Aggregator {
    async fn handle_feed(
        &mut self,
        _msg: SumItems,
        mut receiver: StreamReceiver<u64>,
        _ctx: &mut ActorContext,
    ) -> u64 {
        let mut total = 0u64;
        while let Some(n) = receiver.recv().await {
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
    println!("=== Batch Streaming Example ===\n");

    let runtime = TestRuntime::new();
    let batch_config = BatchConfig::new(4, std::time::Duration::from_millis(5));

    // --- Batched server-streaming ---
    println!("--- stream_batched: NumberServer (10 items, batch=4) ---");
    let server = runtime.spawn::<NumberServer>("numbers", 10);

    let mut stream = server
        .stream_batched(GetNumbers, 16, batch_config.clone(), None)
        .unwrap();

    let mut items = Vec::new();
    while let Some(n) = stream.next().await {
        items.push(n);
    }
    println!("  Received {} items: {:?}", items.len(), items);
    assert_eq!(items, (1..=10).collect::<Vec<_>>());

    // --- Batched client-streaming ---
    println!("\n--- feed_batched: Aggregator (5 items, batch=4) ---");
    let aggregator = runtime.spawn::<Aggregator>("aggregator", ());

    let input = futures::stream::iter(vec![10u64, 20, 30, 40, 50]);
    let batch_config = BatchConfig::new(4, std::time::Duration::from_millis(5));
    let total = aggregator
        .feed_batched(SumItems, Box::pin(input), 8, batch_config, None)
        .unwrap()
        .await
        .unwrap();
    println!("  Feed result (sum): {}", total);
    assert_eq!(total, 150);

    println!("\n=== Done ===");
}
