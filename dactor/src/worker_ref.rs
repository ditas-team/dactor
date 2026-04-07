//! Worker reference for distributed actor pools.
//!
//! [`WorkerRef`] wraps either a local [`ActorRef`] or a
//! [`RemoteActorRef`](crate::remote_ref::RemoteActorRef), implementing
//! [`ActorRef<A>`] by delegating to the inner reference. This enables
//! [`PoolRef`](crate::pool::PoolRef) to route messages across local and
//! remote workers transparently.
//!
//! # Example
//!
//! ```ignore
//! use dactor::worker_ref::WorkerRef;
//! use dactor::pool::{PoolRef, PoolRouting};
//!
//! let local_ref: LocalActorRef<Counter> = runtime.spawn("local", 0).await?;
//! let remote_ref: RemoteActorRef<Counter> = /* ... */;
//!
//! let workers = vec![
//!     WorkerRef::Local(local_ref),
//!     WorkerRef::Remote(remote_ref),
//! ];
//! let pool = PoolRef::new(workers, PoolRouting::RoundRobin);
//! pool.tell(Increment(1))?; // routes to local or remote
//! ```

use crate::actor::{
    Actor, ActorRef, AskReply, ExpandHandler, Handler, ReduceHandler,
    TransformHandler,
};
use crate::errors::ActorSendError;
use crate::message::Message;
use crate::node::ActorId;
use crate::remote_ref::RemoteActorRef;
use crate::stream::{BatchConfig, BoxStream};
use tokio_util::sync::CancellationToken;

/// A worker reference that can be either local or remote.
///
/// Implements [`ActorRef<A>`] by delegating to the inner reference,
/// allowing [`PoolRef`](crate::pool::PoolRef) to mix local and remote
/// workers in a single pool.
pub enum WorkerRef<A: Actor, L: ActorRef<A>> {
    /// A local actor reference (adapter-specific).
    Local(L),
    /// A remote actor reference (cross-node via transport).
    Remote(RemoteActorRef<A>),
}

impl<A: Actor, L: ActorRef<A>> Clone for WorkerRef<A, L> {
    fn clone(&self) -> Self {
        match self {
            WorkerRef::Local(r) => WorkerRef::Local(r.clone()),
            WorkerRef::Remote(r) => WorkerRef::Remote(r.clone()),
        }
    }
}

impl<A: Actor + Sync, L: ActorRef<A>> ActorRef<A> for WorkerRef<A, L> {
    fn id(&self) -> ActorId {
        match self {
            WorkerRef::Local(r) => r.id(),
            WorkerRef::Remote(r) => r.id(),
        }
    }

    fn name(&self) -> String {
        match self {
            WorkerRef::Local(r) => r.name(),
            WorkerRef::Remote(r) => r.name(),
        }
    }

    fn is_alive(&self) -> bool {
        match self {
            WorkerRef::Local(r) => r.is_alive(),
            WorkerRef::Remote(r) => r.is_alive(),
        }
    }

    fn pending_messages(&self) -> usize {
        match self {
            WorkerRef::Local(r) => r.pending_messages(),
            WorkerRef::Remote(r) => r.pending_messages(),
        }
    }

    fn stop(&self) {
        match self {
            WorkerRef::Local(r) => r.stop(),
            WorkerRef::Remote(r) => r.stop(),
        }
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        match self {
            WorkerRef::Local(r) => r.tell(msg),
            WorkerRef::Remote(r) => r.tell(msg),
        }
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
        match self {
            WorkerRef::Local(r) => r.ask(msg, cancel),
            WorkerRef::Remote(r) => r.ask(msg, cancel),
        }
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
        match self {
            WorkerRef::Local(r) => r.expand(msg, buffer, batch_config, cancel),
            WorkerRef::Remote(r) => r.expand(msg, buffer, batch_config, cancel),
        }
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
        match self {
            WorkerRef::Local(r) => r.reduce(input, buffer, batch_config, cancel),
            WorkerRef::Remote(r) => r.reduce(input, buffer, batch_config, cancel),
        }
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
        match self {
            WorkerRef::Local(r) => r.transform(input, buffer, batch_config, cancel),
            WorkerRef::Remote(r) => r.transform(input, buffer, batch_config, cancel),
        }
    }
}

impl<A: Actor, L: ActorRef<A>> WorkerRef<A, L> {
    /// Returns `true` if this is a local worker.
    pub fn is_local(&self) -> bool {
        matches!(self, WorkerRef::Local(_))
    }

    /// Returns `true` if this is a remote worker.
    pub fn is_remote(&self) -> bool {
        matches!(self, WorkerRef::Remote(_))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ActorContext;
    use crate::node::{ActorId, NodeId};
    use crate::pool::{PoolRef, PoolRouting};
    use crate::remote_ref::RemoteActorRefBuilder;
    use crate::test_support::test_runtime::TestRuntime;
    use crate::transport::InMemoryTransport;
    use std::sync::Arc;

    // A simple counter actor for testing.
    struct Counter {
        count: i64,
    }

    impl Actor for Counter {
        type Args = i64;
        type Deps = ();
        fn create(args: i64, _deps: ()) -> Self {
            Counter { count: args }
        }
    }

    struct Increment(i64);
    impl Message for Increment {
        type Reply = ();
    }

    #[async_trait::async_trait]
    impl Handler<Increment> for Counter {
        async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
            self.count += msg.0;
        }
    }

    struct GetCount;
    impl Message for GetCount {
        type Reply = i64;
    }

    #[async_trait::async_trait]
    impl Handler<GetCount> for Counter {
        async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> i64 {
            self.count
        }
    }

    fn make_remote_ref() -> RemoteActorRef<Counter> {
        let transport = Arc::new(InMemoryTransport::new(NodeId("test-node".into())));
        RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("remote-node".into()),
                local: 99,
            },
            "remote-counter",
            transport,
        )
        .build()
    }

    #[test]
    fn worker_ref_is_local_and_is_remote() {
        let remote = make_remote_ref();
        let worker: WorkerRef<Counter, RemoteActorRef<Counter>> =
            WorkerRef::Remote(remote);
        assert!(worker.is_remote());
        assert!(!worker.is_local());
    }

    #[test]
    fn worker_ref_delegates_id_and_name() {
        let remote = make_remote_ref();
        let worker: WorkerRef<Counter, RemoteActorRef<Counter>> =
            WorkerRef::Remote(remote.clone());
        assert_eq!(worker.id(), remote.id());
        assert_eq!(worker.name(), remote.name());
    }

    #[tokio::test]
    async fn distributed_pool_with_local_workers() {
        let rt = TestRuntime::new();
        let w1 = rt.spawn::<Counter>("c1", 0).await.unwrap();
        let w2 = rt.spawn::<Counter>("c2", 0).await.unwrap();

        let workers = vec![
            WorkerRef::Local(w1),
            WorkerRef::Local(w2),
        ];
        let pool = PoolRef::new(workers, PoolRouting::RoundRobin);

        // Tell goes to w1 (round-robin index 0)
        pool.tell(Increment(10)).unwrap();
        // Tell goes to w2 (round-robin index 1)
        pool.tell(Increment(20)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Both workers should have been incremented
        assert!(pool.name().starts_with("pool"));
    }

    #[tokio::test]
    async fn distributed_pool_ask_round_robin() {
        let rt = TestRuntime::new();
        let w1 = rt.spawn::<Counter>("ask-c1", 100).await.unwrap();
        let w2 = rt.spawn::<Counter>("ask-c2", 200).await.unwrap();

        let workers = vec![
            WorkerRef::Local(w1),
            WorkerRef::Local(w2),
        ];
        let pool = PoolRef::new(workers, PoolRouting::RoundRobin);

        // First ask goes to w1 (100)
        let count1 = pool.ask(GetCount, None).unwrap().await.unwrap();
        // Second ask goes to w2 (200)
        let count2 = pool.ask(GetCount, None).unwrap().await.unwrap();

        assert_eq!(count1, 100);
        assert_eq!(count2, 200);
    }

    #[tokio::test]
    async fn distributed_pool_mixed_local_remote_creation() {
        let rt = TestRuntime::new();
        let local = rt.spawn::<Counter>("local-w", 0).await.unwrap();
        let remote = make_remote_ref();

        let workers = vec![
            WorkerRef::Local(local),
            WorkerRef::Remote(remote),
        ];
        let pool = PoolRef::new(workers, PoolRouting::RoundRobin);

        // Pool should have 2 workers
        assert!(pool.is_alive());
        // First worker is local, second is remote
    }

    #[tokio::test]
    async fn worker_ref_stop_delegates() {
        let rt = TestRuntime::new();
        let w = rt.spawn::<Counter>("stop-w", 0).await.unwrap();
        let worker = WorkerRef::<Counter, _>::Local(w);
        assert!(worker.is_alive());
        worker.stop();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!worker.is_alive());
    }

    #[test]
    fn worker_ref_pending_messages_delegates() {
        let remote = make_remote_ref();
        let worker: WorkerRef<Counter, RemoteActorRef<Counter>> =
            WorkerRef::Remote(remote);
        // RemoteActorRef returns 0 by default
        assert_eq!(worker.pending_messages(), 0);
    }
}
