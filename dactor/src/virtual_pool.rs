//! Virtual actor pool with single-threaded routing.
//!
//! A [`VirtualPoolRef`] serializes all routing decisions through a single
//! tokio task, providing zero-contention metrics and feed affinity.
//! Callers interact with it via the [`ActorRef<A>`] trait, just like a
//! regular actor reference or [`PoolRef`](crate::pool::PoolRef).
//!
//! # Architecture
//!
//! ```text
//! Caller → [mpsc channel] → RouterTask → Worker-N → reply direct to caller
//! ```
//!
//! The router task receives type-erased closures ("ops") through an unbounded
//! mpsc channel, selects a worker using the configured [`PoolRouting`] strategy,
//! and invokes the closure with the chosen worker reference.
//!
//! For `tell`, the closure is fire-and-forget.
//! For `ask`, a oneshot channel carries the `AskReply` back to the caller.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::actor::{
    Actor, ActorRef, AskReply, ExpandHandler, Handler, ReduceHandler, TransformHandler,
};
use crate::errors::{ActorSendError, RuntimeError};
use crate::message::Message;
use crate::node::{ActorId, NodeId};
use crate::pool::PoolRouting;
use crate::stream::{BatchConfig, BoxStream};

// ---------------------------------------------------------------------------
// Type-erased operation
// ---------------------------------------------------------------------------

/// A boxed closure that captures a concrete operation (tell/ask/expand/etc.)
/// and invokes it on the selected worker.
type PoolOp<R> = Box<dyn FnOnce(&R) + Send>;

/// Command sent from the `VirtualPoolRef` to the router task.
enum RouterCommand<R: Send + 'static> {
    /// A routing operation — the router selects a worker and invokes the closure.
    Op(PoolOp<R>),
    /// Stop the router task.
    Stop,
}

// ---------------------------------------------------------------------------
// VirtualPoolRef
// ---------------------------------------------------------------------------

/// A virtual actor pool that routes through a single-threaded router task.
///
/// Unlike [`PoolRef`](crate::pool::PoolRef) which selects a worker atomically
/// in the caller's thread, `VirtualPoolRef` funnels all routing decisions
/// through a dedicated tokio task. This provides:
///
/// - **Single-threaded routing** — no atomic contention on the counter.
/// - **Feed affinity** — the router can maintain stateful routing (future).
/// - **Zero-contention metrics** — only the router task updates counters.
///
/// `VirtualPoolRef` implements [`ActorRef<A>`] and can be used as a drop-in
/// replacement for `PoolRef` or a direct actor reference.
pub struct VirtualPoolRef<A: Actor, R: ActorRef<A>> {
    ops_tx: mpsc::UnboundedSender<RouterCommand<R>>,
    pool_id: ActorId,
    name: String,
    alive: Arc<AtomicBool>,
    _phantom: PhantomData<fn() -> A>,
}

impl<A: Actor, R: ActorRef<A>> Clone for VirtualPoolRef<A, R> {
    fn clone(&self) -> Self {
        Self {
            ops_tx: self.ops_tx.clone(),
            pool_id: self.pool_id.clone(),
            name: self.name.clone(),
            alive: self.alive.clone(),
            _phantom: PhantomData,
        }
    }
}

/// Global counter for unique virtual-pool IDs.
static NEXT_VPOOL_ID: AtomicU64 = AtomicU64::new(1);

impl<A: Actor, R: ActorRef<A>> VirtualPoolRef<A, R> {
    /// Create a new virtual pool from a set of worker references.
    ///
    /// Spawns a background tokio task that receives operations and routes
    /// them to workers according to the given [`PoolRouting`] strategy.
    ///
    /// # Panics
    ///
    /// Panics if `workers` is empty.
    pub fn new(workers: Vec<R>, routing: PoolRouting) -> Self {
        assert!(!workers.is_empty(), "pool must have at least one worker");

        let pool_local = NEXT_VPOOL_ID.fetch_add(1, Ordering::Relaxed);
        let pool_id = ActorId {
            node: NodeId("vpool".into()),
            local: pool_local,
        };
        let name = format!("vpool({})", workers[0].name());
        let alive = Arc::new(AtomicBool::new(true));

        let (ops_tx, ops_rx) = mpsc::unbounded_channel();

        let alive_clone = alive.clone();
        tokio::spawn(router_loop(ops_rx, workers, routing, alive_clone));

        Self {
            ops_tx,
            pool_id,
            name,
            alive,
            _phantom: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Router task
// ---------------------------------------------------------------------------

async fn router_loop<A: Actor, R: ActorRef<A>>(
    mut rx: mpsc::UnboundedReceiver<RouterCommand<R>>,
    workers: Vec<R>,
    routing: PoolRouting,
    alive: Arc<AtomicBool>,
) {
    let mut counter: u64 = 0;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            RouterCommand::Op(op) => {
                let idx = select_worker_index(&workers, &routing, &mut counter);
                op(&workers[idx]);
            }
            RouterCommand::Stop => {
                for w in &workers {
                    w.stop();
                }
                break;
            }
        }
    }

    alive.store(false, Ordering::Release);
}

/// Select a worker index based on the routing strategy.
fn select_worker_index<A: Actor, R: ActorRef<A>>(
    workers: &[R],
    routing: &PoolRouting,
    counter: &mut u64,
) -> usize {
    let len = workers.len() as u64;
    match routing {
        PoolRouting::RoundRobin | PoolRouting::KeyBased => {
            let idx = *counter % len;
            *counter = counter.wrapping_add(1);
            idx as usize
        }
        PoolRouting::Random => {
            let raw = *counter;
            *counter = counter.wrapping_add(1);
            (splitmix64(raw) % len) as usize
        }
        PoolRouting::LeastLoaded => {
            let min_load = workers.iter().map(|w| w.pending_messages()).min().unwrap_or(0);
            let candidates: Vec<usize> = workers
                .iter()
                .enumerate()
                .filter(|(_, w)| w.pending_messages() == min_load)
                .map(|(i, _)| i)
                .collect();
            if candidates.len() == 1 {
                candidates[0]
            } else {
                let idx = *counter;
                *counter = counter.wrapping_add(1);
                candidates[(idx as usize) % candidates.len()]
            }
        }
    }
}

/// splitmix64 finaliser — same as used in PoolRef.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

// ---------------------------------------------------------------------------
// ActorRef<A> implementation
// ---------------------------------------------------------------------------

impl<A: Actor, R: ActorRef<A>> ActorRef<A> for VirtualPoolRef<A, R> {
    fn id(&self) -> ActorId {
        self.pool_id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn stop(&self) {
        let _ = self.ops_tx.send(RouterCommand::Stop);
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        let op: PoolOp<R> = Box::new(move |worker: &R| {
            let _ = worker.tell(msg);
        });
        self.ops_tx
            .send(RouterCommand::Op(op))
            .map_err(|_| ActorSendError("virtual pool router stopped".into()))
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
        // Channel to carry the AskReply from the router task back to the caller.
        let (bridge_tx, bridge_rx) =
            tokio::sync::oneshot::channel::<Result<AskReply<M::Reply>, ActorSendError>>();

        let op: PoolOp<R> = Box::new(move |worker: &R| {
            let result = worker.ask(msg, cancel);
            let _ = bridge_tx.send(result);
        });

        self.ops_tx
            .send(RouterCommand::Op(op))
            .map_err(|_| ActorSendError("virtual pool router stopped".into()))?;

        // Build a final AskReply that flattens the two layers:
        //   bridge_rx → Result<AskReply<M::Reply>, ActorSendError>
        //   ask_reply → Result<M::Reply, RuntimeError>
        let (final_tx, final_rx) =
            tokio::sync::oneshot::channel::<Result<M::Reply, RuntimeError>>();

        tokio::spawn(async move {
            match bridge_rx.await {
                Ok(Ok(ask_reply)) => match ask_reply.await {
                    Ok(reply) => {
                        let _ = final_tx.send(Ok(reply));
                    }
                    Err(e) => {
                        let _ = final_tx.send(Err(e));
                    }
                },
                Ok(Err(send_err)) => {
                    let _ = final_tx.send(Err(RuntimeError::Send(send_err)));
                }
                Err(_) => {
                    let _ = final_tx.send(Err(RuntimeError::ActorNotFound(
                        "virtual pool router dropped before forwarding ask".into(),
                    )));
                }
            }
        });

        Ok(AskReply::new(final_rx))
    }

    fn expand<M, OutputItem>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<OutputItem>, ActorSendError>
    where
        A: ExpandHandler<M, OutputItem>,
        M: Send + 'static,
        OutputItem: Send + 'static,
    {
        let (bridge_tx, bridge_rx) =
            tokio::sync::oneshot::channel::<Result<BoxStream<OutputItem>, ActorSendError>>();

        let op: PoolOp<R> = Box::new(move |worker: &R| {
            let result = worker.expand(msg, buffer, batch_config, cancel);
            let _ = bridge_tx.send(result);
        });

        self.ops_tx
            .send(RouterCommand::Op(op))
            .map_err(|_| ActorSendError("virtual pool router stopped".into()))?;

        // Block-wait via a oneshot. Since we need a sync return of
        // Result<BoxStream, ActorSendError>, we rely on the router task
        // processing this quickly (it just forwards to the worker).
        // We use a small spawned task + channel to stay async-compatible.
        //
        // However, expand returns a BoxStream synchronously. We wrap the
        // bridge in a stream that first awaits the inner stream, then yields.
        let stream = futures::stream::once(async move {
            match bridge_rx.await {
                Ok(Ok(inner_stream)) => inner_stream,
                Ok(Err(_)) => Box::pin(futures::stream::empty()) as BoxStream<OutputItem>,
                Err(_) => Box::pin(futures::stream::empty()) as BoxStream<OutputItem>,
            }
        });

        use futures::StreamExt;
        Ok(Box::pin(stream.flatten()))
    }

    fn reduce<InputItem, Reply>(
        &self,
        input: BoxStream<InputItem>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: ReduceHandler<InputItem, Reply>,
        InputItem: Send + 'static,
        Reply: Send + 'static,
    {
        let (bridge_tx, bridge_rx) =
            tokio::sync::oneshot::channel::<Result<AskReply<Reply>, ActorSendError>>();

        let op: PoolOp<R> = Box::new(move |worker: &R| {
            let result = worker.reduce(input, buffer, batch_config, cancel);
            let _ = bridge_tx.send(result);
        });

        self.ops_tx
            .send(RouterCommand::Op(op))
            .map_err(|_| ActorSendError("virtual pool router stopped".into()))?;

        let (final_tx, final_rx) =
            tokio::sync::oneshot::channel::<Result<Reply, RuntimeError>>();

        tokio::spawn(async move {
            match bridge_rx.await {
                Ok(Ok(ask_reply)) => match ask_reply.await {
                    Ok(reply) => {
                        let _ = final_tx.send(Ok(reply));
                    }
                    Err(e) => {
                        let _ = final_tx.send(Err(e));
                    }
                },
                Ok(Err(send_err)) => {
                    let _ = final_tx.send(Err(RuntimeError::Send(send_err)));
                }
                Err(_) => {
                    let _ = final_tx.send(Err(RuntimeError::ActorNotFound(
                        "virtual pool router dropped before forwarding reduce".into(),
                    )));
                }
            }
        });

        Ok(AskReply::new(final_rx))
    }

    fn transform<InputItem, OutputItem>(
        &self,
        input: BoxStream<InputItem>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<OutputItem>, ActorSendError>
    where
        A: TransformHandler<InputItem, OutputItem>,
        InputItem: Send + 'static,
        OutputItem: Send + 'static,
    {
        let (bridge_tx, bridge_rx) =
            tokio::sync::oneshot::channel::<Result<BoxStream<OutputItem>, ActorSendError>>();

        let op: PoolOp<R> = Box::new(move |worker: &R| {
            let result = worker.transform(input, buffer, batch_config, cancel);
            let _ = bridge_tx.send(result);
        });

        self.ops_tx
            .send(RouterCommand::Op(op))
            .map_err(|_| ActorSendError("virtual pool router stopped".into()))?;

        let stream = futures::stream::once(async move {
            match bridge_rx.await {
                Ok(Ok(inner_stream)) => inner_stream,
                Ok(Err(_)) => Box::pin(futures::stream::empty()) as BoxStream<OutputItem>,
                Err(_) => Box::pin(futures::stream::empty()) as BoxStream<OutputItem>,
            }
        });

        use futures::StreamExt;
        Ok(Box::pin(stream.flatten()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::actor::{Actor, ActorContext, Handler};
    use crate::message::Message;
    use crate::test_support::test_runtime::TestRuntime;

    // -- Test actor ----------------------------------------------------------

    struct VPoolWorker {
        id: u64,
        call_count: Arc<AtomicU64>,
    }

    impl Actor for VPoolWorker {
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

    // -- Handlers ------------------------------------------------------------

    #[async_trait]
    impl Handler<Ping> for VPoolWorker {
        async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {
            self.call_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[async_trait]
    impl Handler<WhoAreYou> for VPoolWorker {
        async fn handle(&mut self, _msg: WhoAreYou, _ctx: &mut ActorContext) -> u64 {
            self.id
        }
    }

    // -- Helper --------------------------------------------------------------

    async fn make_vpool(
        rt: &TestRuntime,
        size: usize,
        routing: PoolRouting,
    ) -> (
        VirtualPoolRef<VPoolWorker, crate::test_support::test_runtime::TestActorRef<VPoolWorker>>,
        Vec<Arc<AtomicU64>>,
    ) {
        let mut counters = Vec::new();
        let mut workers = Vec::new();
        for i in 0..size {
            let ctr = Arc::new(AtomicU64::new(0));
            counters.push(ctr.clone());
            let r = rt
                .spawn::<VPoolWorker>(&format!("vw-{}", i), (i as u64, ctr))
                .await
                .unwrap();
            workers.push(r);
        }
        (VirtualPoolRef::new(workers, routing), counters)
    }

    // -- Tests ---------------------------------------------------------------

    #[tokio::test]
    async fn round_robin_distributes_evenly() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_vpool(&rt, 3, PoolRouting::RoundRobin).await;

        for _ in 0..9 {
            pool.tell(Ping).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

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
    async fn random_routing_hits_all_workers() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_vpool(&rt, 3, PoolRouting::Random).await;

        for _ in 0..300 {
            pool.tell(Ping).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let total: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        assert_eq!(total, 300, "all messages should be delivered");

        for (i, ctr) in counters.iter().enumerate() {
            assert!(
                ctr.load(Ordering::Relaxed) > 0,
                "worker {} should have received at least 1 message",
                i,
            );
        }
    }

    #[tokio::test]
    async fn ask_returns_correct_reply() {
        let rt = TestRuntime::new();
        let ctr = Arc::new(AtomicU64::new(0));
        let worker = rt
            .spawn::<VPoolWorker>("solo", (42, ctr))
            .await
            .unwrap();
        let pool = VirtualPoolRef::new(vec![worker], PoolRouting::RoundRobin);

        let id = pool.ask(WhoAreYou, None).unwrap().await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn ask_distributes_across_workers() {
        let rt = TestRuntime::new();
        let (pool, _counters) = make_vpool(&rt, 3, PoolRouting::RoundRobin).await;

        let mut ids = Vec::new();
        for _ in 0..6 {
            let id = pool.ask(WhoAreYou, None).unwrap().await.unwrap();
            ids.push(id);
        }

        // Round-robin: should cycle through workers 0, 1, 2, 0, 1, 2
        assert_eq!(ids, vec![0, 1, 2, 0, 1, 2]);
    }

    #[tokio::test]
    async fn pool_is_alive_and_stop() {
        let rt = TestRuntime::new();
        let (pool, _) = make_vpool(&rt, 2, PoolRouting::RoundRobin).await;

        assert!(pool.is_alive());
        pool.stop();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(!pool.is_alive());
    }

    #[tokio::test]
    async fn tell_after_stop_returns_error() {
        let rt = TestRuntime::new();
        let (pool, _) = make_vpool(&rt, 1, PoolRouting::RoundRobin).await;

        pool.stop();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The channel is closed after router exits
        let result = pool.tell(Ping);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "pool must have at least one worker")]
    fn empty_pool_panics() {
        // Use a runtime to make the panic the only failure mode
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let workers: Vec<crate::test_support::test_runtime::TestActorRef<VPoolWorker>> =
                vec![];
            VirtualPoolRef::new(workers, PoolRouting::RoundRobin);
        });
    }

    #[tokio::test]
    async fn clone_shares_router() {
        let rt = TestRuntime::new();
        let (pool, counters) = make_vpool(&rt, 2, PoolRouting::RoundRobin).await;

        let pool2 = pool.clone();

        // Sends through both clones share the same routing counter
        pool.tell(Ping).unwrap();
        pool2.tell(Ping).unwrap();
        pool.tell(Ping).unwrap();
        pool2.tell(Ping).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 4 messages, round-robin across 2 workers → 2 each
        for (i, ctr) in counters.iter().enumerate() {
            assert_eq!(
                ctr.load(Ordering::Relaxed),
                2,
                "worker {} should have received 2 messages",
                i
            );
        }
    }
}
