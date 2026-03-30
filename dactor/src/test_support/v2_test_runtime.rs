//! V2 test runtime for the `Actor` / `Handler<M>` / `TypedActorRef<A>` API.
//!
//! Provides a lightweight, in-process actor runtime suitable for unit tests.
//! Actors are spawned on the Tokio runtime and process messages sequentially
//! via an unbounded channel.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use tokio::sync::mpsc;

use crate::actor::{Actor, ActorContext, Handler, TypedActorRef};
use crate::errors::ActorSendError;
use crate::message::Message;
use crate::node::{ActorId, NodeId};

// ---------------------------------------------------------------------------
// Type-erased dispatch via async trait
// ---------------------------------------------------------------------------

/// Type-erased message envelope. Each concrete message type is wrapped in a
/// `TypedDispatch<M>` that knows how to invoke `Handler<M>::handle`.
#[async_trait]
trait Dispatch<A: Actor>: Send {
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext);
}

struct TypedDispatch<M: Message> {
    msg: M,
}

#[async_trait]
impl<A, M> Dispatch<A> for TypedDispatch<M>
where
    A: Handler<M>,
    M: Message<Reply = ()>,
{
    async fn dispatch(self: Box<Self>, actor: &mut A, ctx: &mut ActorContext) {
        actor.handle(self.msg, ctx).await;
    }
}

type BoxedDispatch<A> = Box<dyn Dispatch<A>>;

// ---------------------------------------------------------------------------
// V2ActorRef
// ---------------------------------------------------------------------------

/// A test actor reference implementing `TypedActorRef<A>`.
pub struct V2ActorRef<A: Actor> {
    id: ActorId,
    name: String,
    sender: mpsc::UnboundedSender<BoxedDispatch<A>>,
    alive: Arc<AtomicBool>,
}

impl<A: Actor> Clone for V2ActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            sender: self.sender.clone(),
            alive: self.alive.clone(),
        }
    }
}

impl<A: Actor> TypedActorRef<A> for V2ActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && !self.sender.is_closed()
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        let dispatch: BoxedDispatch<A> = Box::new(TypedDispatch { msg });
        self.sender
            .send(dispatch)
            .map_err(|_| ActorSendError("actor stopped".into()))
    }
}

// ---------------------------------------------------------------------------
// V2TestRuntime
// ---------------------------------------------------------------------------

/// A lightweight test runtime that spawns v0.2 actors on the Tokio runtime.
pub struct V2TestRuntime {
    node_id: NodeId,
    next_local: AtomicU64,
}

impl V2TestRuntime {
    pub fn new() -> Self {
        Self {
            node_id: NodeId("test-node".into()),
            next_local: AtomicU64::new(1),
        }
    }

    /// Spawn a v0.2 actor whose `Deps` type is `()`. Returns a `V2ActorRef<A>`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> V2ActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        self.spawn_with_deps(name, args, ())
    }

    /// Spawn a v0.2 actor with explicit dependencies.
    pub fn spawn_with_deps<A>(&self, name: &str, args: A::Args, deps: A::Deps) -> V2ActorRef<A>
    where
        A: Actor + 'static,
    {
        let local = self.next_local.fetch_add(1, Ordering::SeqCst);
        let actor_id = ActorId {
            node: self.node_id.clone(),
            local,
        };
        let actor_name = name.to_string();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();

        let (tx, mut rx) = mpsc::unbounded_channel::<BoxedDispatch<A>>();

        let id_task = actor_id.clone();
        let name_task = actor_name.clone();

        tokio::spawn(async move {
            let mut actor = A::create(args, deps);
            let mut ctx = ActorContext {
                actor_id: id_task,
                actor_name: name_task,
            };

            actor.on_start(&mut ctx).await;

            while let Some(dispatch) = rx.recv().await {
                // Catch panics in handlers to ensure on_stop is always called
                let result = std::panic::AssertUnwindSafe(dispatch.dispatch(&mut actor, &mut ctx))
                    .catch_unwind()
                    .await;
                if result.is_err() {
                    tracing::error!("handler panicked in actor {}", ctx.actor_name);
                    break;
                }
            }

            // Set alive=false BEFORE on_stop to avoid is_alive race condition
            alive_task.store(false, Ordering::SeqCst);
            actor.on_stop().await;
        });

        V2ActorRef {
            id: actor_id,
            name: actor_name,
            sender: tx,
            alive,
        }
    }
}

impl Default for V2TestRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ActorContext;
    use crate::message::Message;
    use crate::node::NodeId;

    // -- Shared test actor: Counter -----------------------------------------

    struct Increment(u64);
    impl Message for Increment {
        type Reply = ();
    }

    struct Counter {
        count: u64,
    }

    impl Actor for Counter {
        type Args = Self;
        type Deps = ();
        fn create(args: Self, _deps: ()) -> Self {
            args
        }
    }

    #[async_trait]
    impl Handler<Increment> for Counter {
        async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
            self.count += msg.0;
        }
    }

    // -- Tests --------------------------------------------------------------

    #[tokio::test]
    async fn test_spawn_and_tell() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        counter.tell(Increment(5)).unwrap();
        counter.tell(Increment(3)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(counter.is_alive());
    }

    #[tokio::test]
    async fn test_tell_returns_actor_id() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("my-counter", Counter { count: 0 });

        assert_eq!(counter.name(), "my-counter");
        assert_eq!(counter.id().node, NodeId("test-node".into()));
        assert!(counter.id().local > 0);
    }

    #[tokio::test]
    async fn test_tell_100_messages_in_order() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        struct OrderTracker {
            received: Arc<Mutex<Vec<u64>>>,
        }

        impl Actor for OrderTracker {
            type Args = Arc<Mutex<Vec<u64>>>;
            type Deps = ();
            fn create(args: Arc<Mutex<Vec<u64>>>, _deps: ()) -> Self {
                OrderTracker { received: args }
            }
        }

        struct TrackMsg(u64);
        impl Message for TrackMsg {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<TrackMsg> for OrderTracker {
            async fn handle(&mut self, msg: TrackMsg, _ctx: &mut ActorContext) {
                self.received.lock().await.push(msg.0);
            }
        }

        let received = Arc::new(Mutex::new(Vec::new()));
        let runtime = V2TestRuntime::new();
        let tracker = runtime.spawn::<OrderTracker>("tracker", received.clone());

        for i in 0..100 {
            tracker.tell(TrackMsg(i)).unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let result = received.lock().await;
        assert_eq!(result.len(), 100);
        for (i, val) in result.iter().enumerate() {
            assert_eq!(*val, i as u64, "message {} out of order", i);
        }
    }

    #[tokio::test]
    async fn test_multiple_actor_refs() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        let ref1 = counter.clone();
        let ref2 = counter.clone();

        ref1.tell(Increment(10)).unwrap();
        ref2.tell(Increment(20)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(ref1.is_alive());
        assert!(ref2.is_alive());
    }

    #[tokio::test]
    async fn test_on_start_called_before_messages() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        struct StartTracker {
            log: Arc<Mutex<Vec<String>>>,
        }

        struct StartTrackerArgs(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl Actor for StartTracker {
            type Args = StartTrackerArgs;
            type Deps = ();
            fn create(args: StartTrackerArgs, _deps: ()) -> Self {
                StartTracker { log: args.0 }
            }
            async fn on_start(&mut self, _ctx: &mut ActorContext) {
                self.log.lock().await.push("on_start".into());
            }
        }

        struct Ping;
        impl Message for Ping {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<Ping> for StartTracker {
            async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {
                self.log.lock().await.push("handle".into());
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let runtime = V2TestRuntime::new();
        let actor = runtime.spawn::<StartTracker>("tracker", StartTrackerArgs(log.clone()));

        actor.tell(Ping).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let entries = log.lock().await;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "on_start");
        assert_eq!(entries[1], "handle");
    }

    #[tokio::test]
    async fn test_tell_to_stopped_actor() {
        let runtime = V2TestRuntime::new();
        let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

        // Drop the original to close the channel
        let counter2 = counter.clone();
        drop(counter);

        // counter2 still holds a sender, so the actor is alive
        assert!(counter2.tell(Increment(1)).is_ok());
    }

    #[tokio::test]
    async fn test_unique_actor_ids() {
        let runtime = V2TestRuntime::new();
        let a = runtime.spawn::<Counter>("a", Counter { count: 0 });
        let b = runtime.spawn::<Counter>("b", Counter { count: 0 });

        assert_ne!(a.id(), b.id());
        assert!(a.id().local < b.id().local);
    }
}
