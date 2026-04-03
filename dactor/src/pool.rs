//! Actor pool with configurable routing strategies.
//!
//! A [`PoolRef`] wraps a set of worker actor references and routes messages
//! to them according to a [`PoolRouting`] strategy. It implements
//! [`ActorRef<A>`] so callers can use it as a drop-in replacement for a
//! single actor reference.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorRef, AskReply, FeedHandler, Handler, StreamHandler};
use crate::errors::ActorSendError;
use crate::message::Message;
use crate::node::{ActorId, NodeId};
use crate::stream::{BatchConfig, BoxStream};

// ---------------------------------------------------------------------------
// PoolRouting
// ---------------------------------------------------------------------------

/// Routing strategy for distributing messages across pool workers.
#[derive(Debug, Clone)]
pub enum PoolRouting {
    /// Each successive message goes to the next worker in order.
    RoundRobin,
    /// Each message goes to a pseudo-randomly chosen worker.
    Random,
    /// Messages implementing [`Keyed`] are routed by their key; others
    /// fall back to round-robin.
    KeyBased,
}

// ---------------------------------------------------------------------------
// PoolConfig
// ---------------------------------------------------------------------------

/// Configuration for creating an actor pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Number of workers in the pool.
    pub pool_size: usize,
    /// How messages are routed to workers.
    pub routing: PoolRouting,
}

impl PoolConfig {
    /// Create a new pool configuration.
    pub fn new(pool_size: usize, routing: PoolRouting) -> Self {
        Self { pool_size, routing }
    }
}

// ---------------------------------------------------------------------------
// Keyed trait
// ---------------------------------------------------------------------------

/// Implemented by messages that carry a routing key for [`PoolRouting::KeyBased`].
pub trait Keyed {
    /// Returns a routing key used to select the target worker.
    fn routing_key(&self) -> u64;
}

// ---------------------------------------------------------------------------
// PoolRef
// ---------------------------------------------------------------------------

/// A pool of actor references that routes messages to workers.
///
/// `PoolRef` is generic over the concrete ref type `R` because [`ActorRef<A>`]
/// has generic methods and is therefore not object-safe.
///
/// # Routing
///
/// - **RoundRobin** — each message goes to the next worker in order.
/// - **Random** — each message goes to a pseudo-randomly chosen worker.
/// - **KeyBased** — messages routed via `tell_keyed()`/`ask_keyed()` use
///   [`Keyed::routing_key()`] for deterministic per-key routing (sticky
///   sessions). Messages sent via the standard `ActorRef` methods
///   (`tell()`/`ask()`/`stream()`/`feed()`) fall back to round-robin,
///   since the trait can't enforce `Keyed` bounds.
pub struct PoolRef<A: Actor, R: ActorRef<A>> {
    workers: Vec<R>,
    routing: PoolRouting,
    counter: Arc<AtomicU64>,
    pool_id: u64,
    name: String,
    _phantom: PhantomData<fn() -> A>,
}

impl<A: Actor, R: ActorRef<A>> Clone for PoolRef<A, R> {
    fn clone(&self) -> Self {
        Self {
            workers: self.workers.clone(),
            routing: self.routing.clone(),
            counter: self.counter.clone(),
            pool_id: self.pool_id,
            name: self.name.clone(),
            _phantom: PhantomData,
        }
    }
}

/// Global counter for unique pool IDs.
static NEXT_POOL_ID: AtomicU64 = AtomicU64::new(1);

impl<A: Actor, R: ActorRef<A>> PoolRef<A, R> {
    /// Create a new pool from a pre-built vector of worker references.
    ///
    /// # Panics
    ///
    /// Panics if `workers` is empty.
    pub fn new(workers: Vec<R>, routing: PoolRouting) -> Self {
        assert!(!workers.is_empty(), "pool must have at least one worker");
        let pool_id = NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("pool({})", workers[0].name());
        Self {
            workers,
            routing,
            counter: Arc::new(AtomicU64::new(0)),
            pool_id,
            name,
            _phantom: PhantomData,
        }
    }

    /// Number of workers in the pool.
    pub fn pool_size(&self) -> usize {
        self.workers.len()
    }

    /// Select a worker index using round-robin.
    fn round_robin_index(&self) -> usize {
        let idx = self.counter.fetch_add(1, Ordering::Relaxed);
        (idx % (self.workers.len() as u64)) as usize
    }

    /// Select a worker index using a pseudo-random strategy.
    ///
    /// Uses a simple hash of the atomic counter combined with a large prime
    /// to provide adequate distribution without requiring the `rand` crate.
    fn random_index(&self) -> usize {
        let raw = self.counter.fetch_add(1, Ordering::Relaxed);
        let mixed = splitmix64(raw);
        (mixed % (self.workers.len() as u64)) as usize
    }

    /// Select the worker for a keyed message.
    fn keyed_index(&self, key: u64) -> usize {
        (key % (self.workers.len() as u64)) as usize
    }

    /// Select a worker reference based on the current routing strategy
    /// (RoundRobin or Random; KeyBased falls back to RoundRobin here).
    fn select_worker(&self) -> &R {
        let idx = match &self.routing {
            PoolRouting::RoundRobin | PoolRouting::KeyBased => self.round_robin_index(),
            PoolRouting::Random => self.random_index(),
        };
        &self.workers[idx]
    }

    /// Fire-and-forget a keyed message, routing by [`Keyed::routing_key`].
    pub fn tell_keyed<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()> + Keyed,
    {
        let idx = self.keyed_index(msg.routing_key());
        self.workers[idx].tell(msg)
    }

    /// Request-reply with a keyed message, routing by [`Keyed::routing_key`].
    pub fn ask_keyed<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message + Keyed,
    {
        let idx = self.keyed_index(msg.routing_key());
        self.workers[idx].ask(msg, cancel)
    }
}

/// splitmix64 finaliser — a fast, well-distributed bijective hash.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

// ---------------------------------------------------------------------------
// ActorRef<A> for PoolRef
// ---------------------------------------------------------------------------

impl<A: Actor, R: ActorRef<A>> ActorRef<A> for PoolRef<A, R> {
    fn id(&self) -> ActorId {
        ActorId {
            node: NodeId("pool".into()),
            local: self.pool_id,
        }
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    /// Returns `true` if **any** worker in the pool is alive.
    /// A partially degraded pool (some workers dead) still reports alive.
    /// Use `pool_size()` and individual worker checks for detailed health.
    fn is_alive(&self) -> bool {
        self.workers.iter().any(|w| w.is_alive())
    }

    fn stop(&self) {
        for w in &self.workers {
            w.stop();
        }
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        self.select_worker().tell(msg)
    }

    fn ask<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message,
    {
        self.select_worker().ask(msg, cancel)
    }

    fn stream<M>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        self.select_worker().stream(msg, buffer, batch_config, cancel)
    }

    fn feed<Item, Reply>(
        &self,
        input: BoxStream<Item>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: FeedHandler<Item, Reply>,
        Item: Send + 'static,
        Reply: Send + 'static,
    {
        self.select_worker().feed(input, buffer, batch_config, cancel)
    }
}

// ---------------------------------------------------------------------------
// spawn_pool helper on TestRuntime
// ---------------------------------------------------------------------------

#[cfg(feature = "test-support")]
impl crate::test_support::test_runtime::TestRuntime {
    /// Spawn a pool of `pool_size` workers of actor `A` and return a [`PoolRef`].
    ///
    /// Each worker is spawned with a cloned copy of `args` and named
    /// `"{name}-{i}"` where `i` is the zero-based worker index.
    pub fn spawn_pool<A>(
        &self,
        name: &str,
        pool_size: usize,
        routing: PoolRouting,
        args: A::Args,
    ) -> PoolRef<A, crate::test_support::test_runtime::TestActorRef<A>>
    where
        A: Actor<Deps = ()> + 'static,
        A::Args: Clone,
    {
        let workers: Vec<_> = (0..pool_size)
            .map(|i| self.spawn::<A>(&format!("{}-{}", name, i), args.clone()))
            .collect();
        PoolRef::new(workers, routing)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::actor::{Actor, ActorContext, Handler};
    use crate::message::Message;
    use crate::test_support::test_runtime::TestRuntime;

    // -- Test actor ----------------------------------------------------------

    struct PoolWorker {
        id: u64,
        call_count: Arc<AtomicU64>,
    }

    impl Actor for PoolWorker {
        type Args = (u64, Arc<AtomicU64>);
        type Deps = ();

        fn create(args: Self::Args, _deps: ()) -> Self {
            Self {
                id: args.0,
                call_count: args.1,
            }
        }
    }

    // -- Messages ------------------------------------------------------------

    struct Ping;
    impl Message for Ping {
        type Reply = ();
    }

    struct WhoAreYou;
    impl Message for WhoAreYou {
        type Reply = u64;
    }

    struct KeyedPing {
        key: u64,
    }
    impl Message for KeyedPing {
        type Reply = u64;
    }
    impl Keyed for KeyedPing {
        fn routing_key(&self) -> u64 {
            self.key
        }
    }

    // -- Handlers ------------------------------------------------------------

    #[async_trait]
    impl Handler<Ping> for PoolWorker {
        async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {
            self.call_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[async_trait]
    impl Handler<WhoAreYou> for PoolWorker {
        async fn handle(&mut self, _msg: WhoAreYou, _ctx: &mut ActorContext) -> u64 {
            self.id
        }
    }

    #[async_trait]
    impl Handler<KeyedPing> for PoolWorker {
        async fn handle(&mut self, _msg: KeyedPing, _ctx: &mut ActorContext) -> u64 {
            self.id
        }
    }

    // -- Helper: spawn a pool of PoolWorkers ---------------------------------

    fn make_pool(
        rt: &TestRuntime,
        size: usize,
        routing: PoolRouting,
    ) -> (
        PoolRef<PoolWorker, crate::test_support::test_runtime::TestActorRef<PoolWorker>>,
        Vec<Arc<AtomicU64>>,
    ) {
        let mut counters = Vec::new();
        let mut workers = Vec::new();
        for i in 0..size {
            let ctr = Arc::new(AtomicU64::new(0));
            counters.push(ctr.clone());
            let r = rt.spawn::<PoolWorker>(&format!("w-{}", i), (i as u64, ctr));
            workers.push(r);
        }
        (PoolRef::new(workers, routing), counters)
    }

    // -- Tests ---------------------------------------------------------------

    #[tokio::test]
    async fn round_robin_distributes_across_workers() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_pool(&rt, 3, PoolRouting::RoundRobin);

        // Send 9 messages — each worker should get exactly 3
        for _ in 0..9 {
            pool.tell(Ping).unwrap();
        }

        // Give the actors a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        for (i, ctr) in counters.iter().enumerate() {
            assert_eq!(
                ctr.load(Ordering::Relaxed),
                3,
                "worker {} should have received 3 messages",
                i
            );
        }
    }

    #[tokio::test]
    async fn random_routing_delivers_to_all() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_pool(&rt, 3, PoolRouting::Random);

        // Send enough messages that every worker should get at least one
        for _ in 0..300 {
            pool.tell(Ping).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let total: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        assert_eq!(total, 300, "all messages should be delivered");

        // Every worker should have received at least one
        for (i, ctr) in counters.iter().enumerate() {
            assert!(
                ctr.load(Ordering::Relaxed) > 0,
                "worker {} should have received at least 1 message with random routing",
                i,
            );
        }
    }

    #[tokio::test]
    async fn key_based_routes_same_key_to_same_worker() {
        let rt = TestRuntime::new();
        let (pool, _counters) = make_pool(&rt, 4, PoolRouting::KeyBased);

        // Same key should always hit the same worker
        for key in [10u64, 42, 99, 1000] {
            let mut results = Vec::new();
            for _ in 0..5 {
                let id = pool
                    .ask_keyed(KeyedPing { key }, None)
                    .unwrap()
                    .await
                    .unwrap();
                results.push(id);
            }
            // All replies for the same key should come from the same worker
            assert!(
                results.windows(2).all(|w| w[0] == w[1]),
                "key {} should always route to the same worker, got {:?}",
                key,
                results,
            );
        }
    }

    #[tokio::test]
    async fn pool_ref_ask_works() {
        let rt = TestRuntime::new();
        let ctr = Arc::new(AtomicU64::new(0));
        let worker = rt.spawn::<PoolWorker>("solo", (42, ctr));
        let pool = PoolRef::new(vec![worker], PoolRouting::RoundRobin);

        let id = pool.ask(WhoAreYou, None).unwrap().await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn pool_size_one_works() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_pool(&rt, 1, PoolRouting::RoundRobin);

        for _ in 0..5 {
            pool.tell(Ping).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(counters[0].load(Ordering::Relaxed), 5);
    }

    #[tokio::test]
    async fn pool_is_alive_and_stop() {
        let rt = TestRuntime::new();
        let (pool, _) = make_pool(&rt, 2, PoolRouting::RoundRobin);

        assert!(pool.is_alive());
        pool.stop();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(!pool.is_alive());
    }

    #[tokio::test]
    async fn spawn_pool_helper() {
        let rt = TestRuntime::new();
        let ctr = Arc::new(AtomicU64::new(0));
        let pool = rt.spawn_pool::<PoolWorker>("sp", 3, PoolRouting::RoundRobin, (0, ctr.clone()));

        assert_eq!(pool.pool_size(), 3);

        // Round-robin tell
        for _ in 0..6 {
            pool.tell(Ping).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Each worker shares the same counter via Clone of (0, Arc<AtomicU64>)
        // so all increments land on the same counter.
        assert_eq!(ctr.load(Ordering::Relaxed), 6);
    }

    #[tokio::test]
    async fn key_based_falls_back_to_round_robin_for_non_keyed_messages() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_pool(&rt, 3, PoolRouting::KeyBased);

        // Non-keyed messages through ActorRef::tell should use round-robin fallback
        for _ in 0..6 {
            pool.tell(Ping).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        for (i, ctr) in counters.iter().enumerate() {
            assert_eq!(
                ctr.load(Ordering::Relaxed),
                2,
                "worker {} should have received 2 messages via round-robin fallback",
                i
            );
        }
    }

    #[test]
    #[should_panic(expected = "pool must have at least one worker")]
    fn empty_pool_panics() {
        let workers: Vec<crate::test_support::test_runtime::TestActorRef<PoolWorker>> = vec![];
        PoolRef::new(workers, PoolRouting::RoundRobin);
    }
}
