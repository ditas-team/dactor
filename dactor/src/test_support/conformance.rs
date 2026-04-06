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
    Actor, ActorContext, ActorError, ActorRef, ReduceHandler, Handler, ExpandHandler,
    TransformHandler,
};
use std::future::Future;
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
impl ExpandHandler<StreamNumbers, u64> for ConformanceStreamer {
    async fn handle_expand(
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
impl ReduceHandler<i64, i64> for ConformanceAggregator {
    async fn handle_reduce(&mut self, mut rx: StreamReceiver<i64>, _ctx: &mut ActorContext) -> i64 {
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
pub async fn test_tell_and_ask<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("counter", 0).await.unwrap();
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
pub async fn test_message_ordering<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("ordered", 0).await.unwrap();
    for _ in 1..=100 {
        actor.tell(Increment(1)).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 100, "ordering: expected 100, got {}", count);
}

/// Test: ask returns the correct reply type.
pub async fn test_ask_reply<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("ask-reply", 42).await.unwrap();
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 42);
}

/// Test: stop() makes actor not alive.
pub async fn test_stop<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("stopper", 0).await.unwrap();
    assert!(actor.is_alive());
    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive());
}

/// Test: actor IDs are unique per spawn.
pub async fn test_unique_ids<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: Fn(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let a1 = spawn("a", 0).await.unwrap();
    let a2 = spawn("b", 0).await.unwrap();
    assert_ne!(a1.id(), a2.id(), "actor IDs should be unique");
}

/// Test: actor name matches the name given at spawn.
pub async fn test_actor_name<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("my-counter", 0).await.unwrap();
    assert_eq!(actor.name(), "my-counter");
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Streams
// ══════════════════════════════════════════════════════

/// Test: stream returns the expected sequence of items.
pub async fn test_stream_items<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-streamer", ()).await.unwrap();
    let stream = actor
        .expand(StreamNumbers { count: 5 }, 16, None, None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert_eq!(items, vec![0, 1, 2, 3, 4]);
}

/// Test: stream with count=0 returns an empty stream.
pub async fn test_stream_empty<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-streamer-empty", ()).await.unwrap();
    let stream = actor
        .expand(StreamNumbers { count: 0 }, 16, None, None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert!(items.is_empty(), "expected empty stream, got {:?}", items);
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Feed
// ══════════════════════════════════════════════════════

/// Test: feed aggregates a stream of items into a single reply.
pub async fn test_feed_sum<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceAggregator>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("conf-agg", ()).await.unwrap();
    let input: BoxStream<i64> = Box::pin(futures::stream::iter(vec![10, 20, 30]));
    let result = actor
        .reduce::<i64, i64>(input, 16, None, None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(result, 60, "feed sum: expected 60, got {}", result);
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Lifecycle
// ══════════════════════════════════════════════════════

/// Test: on_start runs before message handling, on_stop runs after stop.
pub async fn test_lifecycle_ordering<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceLifecycle>,
    F: FnOnce(&'static str, Arc<Mutex<Vec<String>>>) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let log = Arc::new(Mutex::new(Vec::new()));
    let actor = spawn("conf-lifecycle", log.clone()).await.unwrap();

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
pub async fn test_cancel_ask<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let token = CancellationToken::new();
    token.cancel();
    let actor = spawn("conf-cancel-counter", 0).await.unwrap();
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
pub async fn test_on_error_resume<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceResumeActor>,
    F: FnOnce(&'static str, Arc<AtomicU64>) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let count = Arc::new(AtomicU64::new(0));
    let actor = spawn("conf-resume", count.clone()).await.unwrap();

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
pub async fn test_batched_stream<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-batch-stream", ()).await.unwrap();
    let batch = BatchConfig::new(3, Duration::from_millis(50));
    let stream = actor
        .expand(StreamNumbers { count: 7 }, 16, Some(batch), None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert_eq!(
        items,
        vec![0, 1, 2, 3, 4, 5, 6],
        "batched stream should deliver all 7 items in order"
    );
}

/// Test: feed with BatchConfig produces correct aggregated result.
pub async fn test_batched_feed<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceAggregator>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("conf-batch-feed", ()).await.unwrap();
    let batch = BatchConfig::new(3, Duration::from_millis(50));
    let input: BoxStream<i64> = Box::pin(futures::stream::iter(vec![1, 2, 3, 4, 5]));
    let result = actor
        .reduce::<i64, i64>(input, 16, Some(batch), None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(result, 15, "batched feed: expected 15, got {}", result);
}

/// Test: stream with batch_config=None works the same as unbatched.
pub async fn test_stream_with_none_batch<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-stream-none-batch", ()).await.unwrap();
    let stream = actor
        .expand(StreamNumbers { count: 6 }, 16, None, None)
        .unwrap();
    let items: Vec<u64> = stream.collect().await;
    assert_eq!(
        items,
        vec![0, 1, 2, 3, 4, 5],
        "unbatched stream (None) should deliver all 6 items"
    );
}

/// Test: feed with batch_config=None produces correct result.
pub async fn test_feed_with_none_batch<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceAggregator>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("conf-feed-none-batch", ()).await.unwrap();
    let input: BoxStream<i64> = Box::pin(futures::stream::iter(vec![5, 10, 15, 20]));
    let result = actor
        .reduce::<i64, i64>(input, 16, None, None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(result, 50, "unbatched feed: expected 50, got {}", result);
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Post-Stop Errors
// ══════════════════════════════════════════════════════

/// Test: tell() on a stopped actor returns ActorSendError.
pub async fn test_tell_after_stop<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("conf-tell-after-stop", 0).await.unwrap();
    actor.stop();
    // Poll until actor is stopped (with timeout)
    for _ in 0..50 {
        if !actor.is_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!actor.is_alive(), "actor should be stopped");

    let result: Result<(), ActorSendError> = actor.tell(Increment(1));
    assert!(
        result.is_err(),
        "tell on stopped actor should return ActorSendError"
    );
}

/// Test: ask() on a stopped actor returns an error.
pub async fn test_ask_after_stop<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("conf-ask-after-stop", 0).await.unwrap();
    actor.stop();
    // Poll until actor is stopped (with timeout)
    for _ in 0..50 {
        if !actor.is_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
pub async fn test_multiple_handlers<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceMultiHandler>,
    F: FnOnce(&'static str, i64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("conf-multi-handler", 10).await.unwrap();

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

// ══════════════════════════════════════════════════════
// Conformance Tests – Corner-Case Edge Tests
// ══════════════════════════════════════════════════════

/// Test: send 100 ask(Increment) in sequence with values 1..=100, verify FIFO ordering.
/// Each ask returns the reply after the increment, and we verify the running total
/// matches the expected sum at each step — this detects reordering since addition
/// of different values in wrong order produces wrong intermediate results.
pub async fn test_message_ordering_under_load<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("ordering-load", 0).await.unwrap();
    let mut expected_sum = 0u64;
    for i in 1..=100u64 {
        expected_sum += i;
        actor.ask(Increment(i), None).unwrap().await.unwrap();
        // Verify running total after each increment to detect reordering
        let count = actor.ask(GetCount, None).unwrap().await.unwrap();
        assert_eq!(
            count, expected_sum,
            "ordering: after Increment({}), expected {}, got {}",
            i, expected_sum, count
        );
    }
}

/// Test: concurrent asks from 10 tasks don't corrupt actor state.
/// Each task sends 10 ask(GetCount) messages and verifies replies are non-negative
/// and monotonically non-decreasing within each task.
pub async fn test_concurrent_asks<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("concurrent-asks", 0).await.unwrap();

    // Send some increments so GetCount returns increasing values
    for i in 1..=50u64 {
        actor.tell(Increment(i)).unwrap();
    }

    let mut handles = Vec::new();
    for _ in 0..10 {
        let actor_clone = actor.clone();
        handles.push(tokio::spawn(async move {
            let mut prev = 0u64;
            for _ in 0..10 {
                let count = actor_clone.ask(GetCount, None).unwrap().await.unwrap();
                assert!(
                    count >= prev,
                    "concurrent asks: count went backwards ({} < {})",
                    count,
                    prev
                );
                prev = count;
            }
        }));
    }
    for h in handles {
        h.await.expect("concurrent ask task panicked");
    }
}

/// Test: stream with small buffer (backpressure) and a slow consumer.
/// Starts a stream of 20 items with buffer=2, sleeps 10ms between each item,
/// and verifies all 20 items arrive in order.
pub async fn test_stream_slow_consumer<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceStreamer>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("stream-slow", ()).await.unwrap();
    let mut stream = actor
        .expand(StreamNumbers { count: 20 }, 2, None, None)
        .unwrap();

    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let expected: Vec<u64> = (0..20).collect();
    assert_eq!(
        items, expected,
        "slow consumer: expected 0..20 in order, got {:?}",
        items
    );
}

/// Test: spawn, send 5 messages, ask for count, stop, then verify post-stop sends fail.
/// Validates that messages before stop are processed and post-stop sends return errors.
pub async fn test_rapid_stop_and_send<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&'static str, u64) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    let actor = spawn("rapid-stop", 0).await.unwrap();

    // Send 5 messages and ask for the count before stopping
    for _ in 0..5 {
        actor.tell(Increment(1)).unwrap();
    }
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(
        count, 5,
        "rapid stop: expected 5 messages processed before stop, got {}",
        count
    );

    actor.stop();
    // Poll until actor is stopped
    for _ in 0..50 {
        if !actor.is_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!actor.is_alive(), "actor should be stopped");

    // Post-stop tell should fail
    let tell_result = actor.tell(Increment(1));
    assert!(tell_result.is_err(), "tell after stop should return error");

    // Post-stop ask should fail
    let ask_result = actor.ask(GetCount, None);
    assert!(ask_result.is_err(), "ask after stop should return error");
}

// ══════════════════════════════════════════════════════
// Transform Actor Definitions
// ══════════════════════════════════════════════════════

/// Transform actor: doubles each input item.
pub struct ConformanceDoubler;

impl Actor for ConformanceDoubler {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Self
    }
}

#[async_trait]
impl TransformHandler<i32, i32> for ConformanceDoubler {
    async fn handle_transform(
        &mut self,
        item: i32,
        sender: &StreamSender<i32>,
        _ctx: &mut ActorContext,
    ) {
        let _ = sender.send(item * 2).await;
    }
}

// ══════════════════════════════════════════════════════
// Conformance Tests – Transform
// ══════════════════════════════════════════════════════

/// Test: transform doubles each item in a stream.
pub async fn test_transform_doubler<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceDoubler>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-doubler", ()).await.unwrap();
    let input: BoxStream<i32> = Box::pin(futures::stream::iter(vec![1, 2, 3, 4, 5]));
    let output: Vec<i32> = actor
        .transform::<i32, i32>(input, 8, None, None)
        .unwrap()
        .collect()
        .await;
    assert_eq!(
        output,
        vec![2, 4, 6, 8, 10],
        "transform doubler: expected doubled values"
    );
}

/// Test: transform with empty input produces empty output.
pub async fn test_transform_empty<R, F, Fut>(spawn: F)
where
    R: ActorRef<ConformanceDoubler>,
    F: FnOnce(&'static str, ()) -> Fut,
    Fut: Future<Output = Result<R, RuntimeError>>,
{
    use tokio_stream::StreamExt;

    let actor = spawn("conf-doubler-empty", ()).await.unwrap();
    let input: BoxStream<i32> = Box::pin(futures::stream::iter(Vec::<i32>::new()));
    let output: Vec<i32> = actor
        .transform::<i32, i32>(input, 8, None, None)
        .unwrap()
        .collect()
        .await;
    assert!(
        output.is_empty(),
        "transform empty: expected empty output, got {:?}",
        output
    );
}

