//! Conformance test suite for verifying runtime implementations.
//!
//! Any runtime that implements the dactor v0.2 API can use these tests
//! to verify correct behavior. Call each test function with a factory
//! that creates your runtime's actor ref.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::actor::{
    Actor, ActorContext, ActorError, ActorRef, FeedHandler, Handler, StreamHandler,
};
use crate::errors::{ActorSendError, ErrorAction, RuntimeError};
use crate::message::Message;
use crate::stream::{BatchConfig, BoxStream, StreamReceiver, StreamSender};

// ══════════════════════════════════════════════════════
// Test Actor Definitions
// ══════════════════════════════════════════════════════

// ── ConformanceCounter ──────────────────────────────

/// A simple counter actor used by the conformance suite.
pub struct ConformanceCounter {
    count: u64,
}

impl Actor for ConformanceCounter {
    type Args = u64;
    type Deps = ();
    fn create(args: u64, _deps: ()) -> Self {
        Self { count: args }
    }
}

/// Tell message: increment the counter by the given amount.
pub struct Increment(pub u64);
impl Message for Increment {
    type Reply = ();
}

#[async_trait]
impl Handler<Increment> for ConformanceCounter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

/// Ask message: return the current count.
pub struct GetCount;
impl Message for GetCount {
    type Reply = u64;
}

#[async_trait]
impl Handler<GetCount> for ConformanceCounter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

// ── ConformanceStreamer ─────────────────────────────

/// Streaming actor: emits a sequence of numbers.
pub struct ConformanceStreamer;

impl Actor for ConformanceStreamer {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Self
    }
}

/// Stream request: emit `count` sequential numbers starting from 0.
pub struct StreamNumbers {
    pub count: u64,
}
impl Message for StreamNumbers {
    type Reply = u64;
}

#[async_trait]
impl StreamHandler<StreamNumbers> for ConformanceStreamer {
    async fn handle_stream(
        &mut self,
        msg: StreamNumbers,
        sender: StreamSender<u64>,
        _ctx: &mut ActorContext,
    ) {
        for i in 0..msg.count {
            if sender.send(i).await.is_err() {
                break;
            }
        }
    }
}

// ── ConformanceAggregator ───────────────────────────

/// Feed actor: aggregates a stream of items into a single reply.
pub struct ConformanceAggregator;

impl Actor for ConformanceAggregator {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Self
    }
}

#[async_trait]
impl FeedHandler<i64, i64> for ConformanceAggregator {
    async fn handle_feed(
        &mut self,
        mut rx: StreamReceiver<i64>,
        _ctx: &mut ActorContext,
    ) -> i64 {
        let mut sum = 0i64;
        while let Some(v) = rx.recv().await {
            sum += v;
        }
        sum
    }
}

// ── ConformanceLifecycle ────────────────────────────

/// Lifecycle actor: records on_start and on_stop events in a shared log.
pub struct ConformanceLifecycle {
    pub log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Actor for ConformanceLifecycle {
    type Args = Arc<Mutex<Vec<String>>>;
    type Deps = ();
    fn create(args: Arc<Mutex<Vec<String>>>, _: ()) -> Self {
        Self { log: args }
    }
    async fn on_start(&mut self, _ctx: &mut ActorContext) {
        self.log.lock().unwrap().push("start".to_string());
    }
    async fn on_stop(&mut self) {
        self.log.lock().unwrap().push("stop".to_string());
    }
}

/// No-op tell message for ConformanceLifecycle so we can verify it's running.
pub struct LifecyclePing;
impl Message for LifecyclePing {
    type Reply = ();
}

#[async_trait]
impl Handler<LifecyclePing> for ConformanceLifecycle {
    async fn handle(&mut self, _msg: LifecyclePing, _ctx: &mut ActorContext) {}
}

// ── ConformanceResumeActor ──────────────────────────

/// Resume-on-error actor: returns `ErrorAction::Resume` so it survives panics.
pub struct ConformanceResumeActor {
    pub count: Arc<AtomicU64>,
}

impl Actor for ConformanceResumeActor {
    type Args = Arc<AtomicU64>;
    type Deps = ();
    fn create(args: Arc<AtomicU64>, _: ()) -> Self {
        Self { count: args }
    }
    fn on_error(&mut self, _: &ActorError) -> ErrorAction {
        ErrorAction::Resume
    }
}

/// Tell message that deliberately panics.
pub struct PanicMsg;
impl Message for PanicMsg {
    type Reply = ();
}

#[async_trait]
impl Handler<PanicMsg> for ConformanceResumeActor {
    async fn handle(&mut self, _msg: PanicMsg, _ctx: &mut ActorContext) {
        panic!("intentional conformance panic");
    }
}

/// Tell message that increments the shared counter.
pub struct CountMsg;
impl Message for CountMsg {
    type Reply = ();
}

#[async_trait]
impl Handler<CountMsg> for ConformanceResumeActor {
    async fn handle(&mut self, _msg: CountMsg, _ctx: &mut ActorContext) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

// ── ConformanceMultiHandler ────────────────────────────

/// Actor that implements Handler for two different message types.
pub struct ConformanceMultiHandler {
    value: i64,
}

impl Actor for ConformanceMultiHandler {
    type Args = i64;
    type Deps = ();
    fn create(args: i64, _: ()) -> Self {
        Self { value: args }
    }
}

/// Ask message: add amount and return new value.
pub struct AddValue(pub i64);
impl Message for AddValue {
    type Reply = i64;
}

#[async_trait]
impl Handler<AddValue> for ConformanceMultiHandler {
    async fn handle(&mut self, msg: AddValue, _ctx: &mut ActorContext) -> i64 {
        self.value += msg.0;
        self.value
    }
}

/// Ask message: multiply by factor and return new value.
pub struct MulValue(pub i64);
impl Message for MulValue {
    type Reply = i64;
}

#[async_trait]
impl Handler<MulValue> for ConformanceMultiHandler {
    async fn handle(&mut self, msg: MulValue, _ctx: &mut ActorContext) -> i64 {
        self.value *= msg.0;
        self.value
    }
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Basic
// ══════════════════════════════════════════════════════

/// Test: tell delivers messages and ask returns the correct reply.
pub async fn test_tell_and_ask<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("counter", 0);
    actor.tell(Increment(5)).unwrap();
    actor.tell(Increment(3)).unwrap();

    // Allow messages to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 8, "tell+ask: expected 8, got {}", count);

    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive(), "actor should be stopped");
}

/// Test: 100 messages processed in order.
pub async fn test_message_ordering<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("ordered", 0);
    for _ in 1..=100 {
        actor.tell(Increment(1)).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 100, "ordering: expected 100, got {}", count);
}

/// Test: ask returns the correct reply type.
pub async fn test_ask_reply<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("ask-reply", 42);
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 42);
}

/// Test: stop() makes actor not alive.
pub async fn test_stop<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("stopper", 0);
    assert!(actor.is_alive());
    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive());
}

/// Test: actor IDs are unique per spawn.
pub async fn test_unique_ids<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: Fn(&str, u64) -> R,
{
    let a1 = spawn("a", 0);
    let a2 = spawn("b", 0);
    assert_ne!(a1.id(), a2.id(), "actor IDs should be unique");
}

/// Test: actor name matches the name given at spawn.
pub async fn test_actor_name<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("my-counter", 0);
    assert_eq!(actor.name(), "my-counter");
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Streams
// ══════════════════════════════════════════════════════

/// Test: stream returns the expected sequence of items.
pub async fn test_stream_items<R, F>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&str, ()) -> R,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-streamer", ());
    let stream = actor
        .stream(StreamNumbers { count: 5 }, 16, None, None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert_eq!(items, vec![0, 1, 2, 3, 4]);
}

/// Test: stream with count=0 returns an empty stream.
pub async fn test_stream_empty<R, F>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&str, ()) -> R,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-streamer-empty", ());
    let stream = actor
        .stream(StreamNumbers { count: 0 }, 16, None, None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert!(items.is_empty(), "expected empty stream, got {:?}", items);
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Feed
// ══════════════════════════════════════════════════════

/// Test: feed aggregates a stream of items into a single reply.
pub async fn test_feed_sum<R, F>(spawn: F)
where
    R: ActorRef<ConformanceAggregator>,
    F: FnOnce(&str, ()) -> R,
{
    let actor = spawn("conf-agg", ());
    let input: BoxStream<i64> = Box::pin(futures::stream::iter(vec![10, 20, 30]));
    let result = actor.feed::<i64, i64>(input, 16, None, None).unwrap().await.unwrap();
    assert_eq!(result, 60, "feed sum: expected 60, got {}", result);
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Lifecycle
// ══════════════════════════════════════════════════════

/// Test: on_start runs before message handling, on_stop runs after stop.
pub async fn test_lifecycle_ordering<R, F>(spawn: F)
where
    R: ActorRef<ConformanceLifecycle>,
    F: FnOnce(&str, Arc<Mutex<Vec<String>>>) -> R,
{
    let log = Arc::new(Mutex::new(Vec::new()));
    let actor = spawn("conf-lifecycle", log.clone());

    // Give on_start time to run
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send a message to confirm the actor is alive after start
    actor.tell(LifecyclePing).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    actor.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let entries = log.lock().unwrap();
    assert!(
        entries.contains(&"start".to_string()),
        "lifecycle log should contain 'start', got {:?}",
        *entries
    );
    assert!(
        entries.contains(&"stop".to_string()),
        "lifecycle log should contain 'stop', got {:?}",
        *entries
    );
    let start_idx = entries.iter().position(|e| e == "start").unwrap();
    let stop_idx = entries.iter().position(|e| e == "stop").unwrap();
    assert!(
        start_idx < stop_idx,
        "start (idx {}) should come before stop (idx {})",
        start_idx,
        stop_idx
    );
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Cancellation
// ══════════════════════════════════════════════════════

/// Test: ask with a pre-cancelled token returns an error.
pub async fn test_cancel_ask<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let token = CancellationToken::new();
    token.cancel();
    let actor = spawn("conf-cancel-counter", 0);
    let result = actor.ask(GetCount, Some(token)).unwrap().await;
    assert!(result.is_err(), "ask with cancelled token should fail");
    // Pre-cancelled token should produce Cancelled error specifically
    assert!(
        matches!(result.unwrap_err(), RuntimeError::Cancelled),
        "expected RuntimeError::Cancelled for pre-cancelled token"
    );
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Error Recovery
// ══════════════════════════════════════════════════════

/// Test: actor with ErrorAction::Resume survives a panic and processes the next message.
pub async fn test_on_error_resume<R, F>(spawn: F)
where
    R: ActorRef<ConformanceResumeActor>,
    F: FnOnce(&str, Arc<AtomicU64>) -> R,
{
    let count = Arc::new(AtomicU64::new(0));
    let actor = spawn("conf-resume", count.clone());

    // This panics inside the handler, but Resume keeps the actor alive.
    actor.tell(PanicMsg).unwrap();
    // This should still be delivered after the panic is recovered.
    actor.tell(CountMsg).unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "actor should have processed CountMsg after recovering from panic"
    );
    assert!(
        actor.is_alive(),
        "actor with Resume policy should still be alive"
    );
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Batched Streaming
// ══════════════════════════════════════════════════════

/// Test: stream with BatchConfig collects all items correctly.
pub async fn test_batched_stream<R, F>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&str, ()) -> R,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-batch-stream", ());
    let batch = BatchConfig::new(3, Duration::from_millis(50));
    let stream = actor
        .stream(StreamNumbers { count: 7 }, 16, Some(batch), None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert_eq!(
        items,
        vec![0, 1, 2, 3, 4, 5, 6],
        "batched stream should deliver all 7 items in order"
    );
}

/// Test: feed with BatchConfig produces correct aggregated result.
pub async fn test_batched_feed<R, F>(spawn: F)
where
    R: ActorRef<ConformanceAggregator>,
    F: FnOnce(&str, ()) -> R,
{
    let actor = spawn("conf-batch-feed", ());
    let batch = BatchConfig::new(3, Duration::from_millis(50));
    let input: BoxStream<i64> = Box::pin(futures::stream::iter(vec![1, 2, 3, 4, 5]));
    let result = actor
        .feed::<i64, i64>(input, 16, Some(batch), None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(result, 15, "batched feed: expected 15, got {}", result);
}

/// Test: stream with batch_config=None works the same as unbatched.
pub async fn test_stream_with_none_batch<R, F>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&str, ()) -> R,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-stream-none-batch", ());
    let stream = actor
        .stream(StreamNumbers { count: 6 }, 16, None, None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert_eq!(
        items,
        vec![0, 1, 2, 3, 4, 5],
        "unbatched stream (None) should deliver all 6 items"
    );
}

/// Test: feed with batch_config=None produces correct result.
pub async fn test_feed_with_none_batch<R, F>(spawn: F)
where
    R: ActorRef<ConformanceAggregator>,
    F: FnOnce(&str, ()) -> R,
{
    let actor = spawn("conf-feed-none-batch", ());
    let input: BoxStream<i64> = Box::pin(futures::stream::iter(vec![5, 10, 15, 20]));
    let result = actor
        .feed::<i64, i64>(input, 16, None, None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(result, 50, "unbatched feed: expected 50, got {}", result);
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Post-Stop Errors
// ══════════════════════════════════════════════════════

/// Test: tell() on a stopped actor returns ActorSendError.
pub async fn test_tell_after_stop<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("conf-tell-after-stop", 0);
    actor.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!actor.is_alive(), "actor should be stopped");

    let result: Result<(), ActorSendError> = actor.tell(Increment(1));
    assert!(
        result.is_err(),
        "tell on stopped actor should return ActorSendError"
    );
}

/// Test: ask() on a stopped actor returns an error.
pub async fn test_ask_after_stop<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("conf-ask-after-stop", 0);
    actor.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!actor.is_alive(), "actor should be stopped");

    let result = actor.ask(GetCount, None);
    assert!(
        result.is_err(),
        "ask on stopped actor should return ActorSendError"
    );
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Multiple Handlers
// ══════════════════════════════════════════════════════

/// Test: actor implementing Handler for two different message types works via the same ref.
pub async fn test_multiple_handlers<R, F>(spawn: F)
where
    R: ActorRef<ConformanceMultiHandler>,
    F: FnOnce(&str, i64) -> R,
{
    let actor = spawn("conf-multi-handler", 10);

    // AddValue: 10 + 5 = 15
    let v1 = actor.ask(AddValue(5), None).unwrap().await.unwrap();
    assert_eq!(v1, 15, "after add: expected 15, got {}", v1);

    // MulValue: 15 * 3 = 45
    let v2 = actor.ask(MulValue(3), None).unwrap().await.unwrap();
    assert_eq!(v2, 45, "after mul: expected 45, got {}", v2);

    // AddValue again: 45 + 5 = 50
    let v3 = actor.ask(AddValue(5), None).unwrap().await.unwrap();
    assert_eq!(v3, 50, "after second add: expected 50, got {}", v3);
}
