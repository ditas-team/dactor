use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dactor::{
    ActorRef as DactorActorRef, ActorRuntime as DactorActorRuntime, ActorSendError, GroupError,
    TimerHandle,
};

use crate::cluster::KameoClusterEvents;

// ---------------------------------------------------------------------------
// HandlerActor — generic kameo Actor wrapping a FnMut(M) closure
// ---------------------------------------------------------------------------

/// A kameo `Actor` implementation that delegates message handling to a
/// user-supplied `FnMut(M)` closure. The closure is stored in the actor's
/// state (initialized via `on_start`) so that it only needs `Send`, not `Sync`.
struct HandlerActor<M: Send + 'static> {
    handler: Box<dyn FnMut(M) + Send>,
}

impl<M: Send + 'static> kameo::Actor for HandlerActor<M> {
    type Args = Box<dyn FnMut(M) + Send>;
    type Error = kameo::error::Infallible;

    async fn on_start(
        args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self { handler: args })
    }
}

impl<M: Send + 'static> kameo::message::Message<M> for HandlerActor<M> {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: M,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        (self.handler)(msg);
    }
}

// ---------------------------------------------------------------------------
// KameoActorRef — wraps kameo::actor::ActorRef<HandlerActor<M>>
// ---------------------------------------------------------------------------

/// A dactor `ActorRef` backed by a kameo `ActorRef<HandlerActor<M>>`.
///
/// Messages are delivered via kameo's `tell()` with `try_send()` (non-blocking).
pub struct KameoActorRef<M: Send + 'static> {
    inner: kameo::actor::ActorRef<HandlerActor<M>>,
}

impl<M: Send + 'static> KameoActorRef<M> {
    /// Access the kameo `ActorId` for this actor reference.
    pub fn actor_id(&self) -> kameo::actor::ActorId {
        self.inner.id()
    }
}

impl<M: Send + 'static> Clone for KameoActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<M: Send + 'static> std::fmt::Debug for KameoActorRef<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KameoActorRef({:?})", self.inner.id())
    }
}

impl<M: Send + 'static> DactorActorRef<M> for KameoActorRef<M> {
    fn send(&self, msg: M) -> Result<(), ActorSendError> {
        self.inner
            .tell(msg)
            .try_send()
            .map_err(|e| ActorSendError(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// KameoTimerHandle — wraps a tokio JoinHandle
// ---------------------------------------------------------------------------

/// A dactor `TimerHandle` backed by a tokio `JoinHandle`.
///
/// Calling [`cancel()`](TimerHandle::cancel) aborts the background task.
/// Dropping the handle without calling `cancel()` also aborts the task
/// via the [`Drop`] implementation, preventing resource leaks.
pub struct KameoTimerHandle {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl TimerHandle for KameoTimerHandle {
    fn cancel(mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

impl Drop for KameoTimerHandle {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Group storage — type-erased kameo ActorRef storage for groups
// ---------------------------------------------------------------------------

type ErasedRef = Box<dyn std::any::Any + Send + Sync>;

/// Wrapper to store a typed kameo ActorRef in type-erased group storage.
struct RefWrapper<M: Send + 'static>(kameo::actor::ActorRef<HandlerActor<M>>);

// ---------------------------------------------------------------------------
// KameoRuntime
// ---------------------------------------------------------------------------

/// A dactor `ActorRuntime` implementation backed by kameo.
///
/// # Tokio Runtime Requirement
///
/// All methods that interact with the async kameo/tokio layer require a
/// running tokio runtime context. This includes [`spawn`](DactorActorRuntime::spawn),
/// [`send_interval`](DactorActorRuntime::send_interval), and
/// [`send_after`](DactorActorRuntime::send_after). Calling these outside a
/// tokio runtime will panic.
///
/// **Note:** Unlike the ractor adapter, kameo's `Spawn::spawn()` is
/// synchronous (it calls `tokio::spawn` internally), so `spawn()` does not
/// require a sync→async bridge thread and is cheaper than the ractor adapter's
/// `spawn()`.
///
/// # Processing Groups
///
/// Groups are maintained in a local type-erased registry. A single group name
/// can hold actors of different message types; `broadcast_group::<M>` delivers
/// only to members whose message type matches `M`, silently skipping others.
/// Callers should use distinct group names per message type to avoid confusion.
///
/// Cluster events use a callback-based subscription model.
#[derive(Clone)]
pub struct KameoRuntime {
    groups: Arc<Mutex<HashMap<String, Vec<ErasedRef>>>>,
    cluster_events: KameoClusterEvents,
}

impl KameoRuntime {
    /// Create a new `KameoRuntime`.
    pub fn new() -> Self {
        Self {
            groups: Arc::new(Mutex::new(HashMap::new())),
            cluster_events: KameoClusterEvents::new(),
        }
    }

    /// Access the cluster events subsystem for manual event emission.
    pub fn cluster_events_handle(&self) -> &KameoClusterEvents {
        &self.cluster_events
    }
}

impl Default for KameoRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DactorActorRuntime for KameoRuntime {
    type Ref<M: Send + 'static> = KameoActorRef<M>;
    type Events = KameoClusterEvents;
    type Timer = KameoTimerHandle;

    fn spawn<M, H>(&self, _name: &str, handler: H) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static,
    {
        use kameo::actor::Spawn;

        let boxed_handler: Box<dyn FnMut(M) + Send> = Box::new(handler);
        let actor_ref = HandlerActor::<M>::spawn(boxed_handler);
        KameoActorRef { inner: actor_ref }
    }

    fn send_interval<M: Clone + Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        interval: Duration,
        msg: M,
    ) -> Self::Timer {
        let actor_ref = target.inner.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // first tick is immediate — skip it
            loop {
                ticker.tick().await;
                if actor_ref.tell(msg.clone()).try_send().is_err() {
                    break;
                }
            }
        });
        KameoTimerHandle {
            handle: Some(handle),
        }
    }

    fn send_after<M: Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        delay: Duration,
        msg: M,
    ) -> Self::Timer {
        let actor_ref = target.inner.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = actor_ref.tell(msg).try_send();
        });
        KameoTimerHandle {
            handle: Some(handle),
        }
    }

    fn join_group<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError> {
        let wrapper = RefWrapper(actor.inner.clone());
        let mut groups = self.groups.lock().unwrap();
        let group = groups.entry(group_name.to_string()).or_default();
        group.push(Box::new(wrapper));
        Ok(())
    }

    fn leave_group<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError> {
        let target_id = actor.inner.id();
        let mut groups = self.groups.lock().unwrap();
        if let Some(members) = groups.get_mut(group_name) {
            members.retain(|member| {
                if let Some(wrapper) = member.downcast_ref::<RefWrapper<M>>() {
                    wrapper.0.id() != target_id
                } else {
                    true
                }
            });
        }
        Ok(())
    }

    fn broadcast_group<M: Clone + Send + 'static>(
        &self,
        group_name: &str,
        msg: M,
    ) -> Result<(), GroupError> {
        let groups = self.groups.lock().unwrap();
        if let Some(members) = groups.get(group_name) {
            for member in members {
                if let Some(wrapper) = member.downcast_ref::<RefWrapper<M>>() {
                    let _ = wrapper.0.tell(msg.clone()).try_send();
                }
            }
        }
        Ok(())
    }

    fn get_group_members<M: Send + 'static>(
        &self,
        group_name: &str,
    ) -> Result<Vec<Self::Ref<M>>, GroupError> {
        let groups = self.groups.lock().unwrap();
        let mut result = Vec::new();
        if let Some(members) = groups.get(group_name) {
            for member in members {
                if let Some(wrapper) = member.downcast_ref::<RefWrapper<M>>() {
                    result.push(KameoActorRef {
                        inner: wrapper.0.clone(),
                    });
                }
            }
        }
        Ok(result)
    }

    fn cluster_events(&self) -> &Self::Events {
        &self.cluster_events
    }
}
