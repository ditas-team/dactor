use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dactor::{
    ActorRef as DactorActorRef, ActorRuntime as DactorActorRuntime, ActorSendError, GroupError,
    TimerHandle,
};

use crate::cluster::RactorClusterEvents;

// ---------------------------------------------------------------------------
// HandlerActor — generic ractor::Actor wrapping a FnMut(M) closure
// ---------------------------------------------------------------------------

/// A ractor `Actor` implementation that delegates message handling to a
/// user-supplied `FnMut(M)` closure. The closure is stored in the actor's
/// `State` (not in the actor struct) so that it only needs `Send`, not `Sync`.
struct HandlerActor<M: Send + 'static> {
    _phantom: PhantomData<fn() -> M>,
}

impl<M: Send + 'static> HandlerActor<M> {
    fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<M: Send + 'static> ractor::Actor for HandlerActor<M> {
    type Msg = M;
    type State = Box<dyn FnMut(M) + Send>;
    type Arguments = Box<dyn FnMut(M) + Send>;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        Ok(args)
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        (state)(message);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RactorActorRef — wraps ractor::ActorRef<M>
// ---------------------------------------------------------------------------

/// A dactor `ActorRef` backed by a ractor `ActorRef<M>`.
///
/// Messages are delivered via ractor's `cast()` (fire-and-forget).
pub struct RactorActorRef<M: Send + 'static> {
    inner: ractor::ActorRef<M>,
}

impl<M: Send + 'static> RactorActorRef<M> {
    /// Access the underlying ractor `ActorRef`.
    pub fn ractor_ref(&self) -> &ractor::ActorRef<M> {
        &self.inner
    }
}

impl<M: Send + 'static> Clone for RactorActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<M: Send + 'static> std::fmt::Debug for RactorActorRef<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RactorActorRef({:?})", self.inner)
    }
}

impl<M: Send + 'static> DactorActorRef<M> for RactorActorRef<M> {
    fn send(&self, msg: M) -> Result<(), ActorSendError> {
        self.inner
            .cast(msg)
            .map_err(|e| ActorSendError(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// RactorTimerHandle — wraps a tokio JoinHandle
// ---------------------------------------------------------------------------

/// A dactor `TimerHandle` backed by a tokio `JoinHandle`.
///
/// Calling [`cancel()`](TimerHandle::cancel) aborts the background task.
/// Dropping the handle without calling `cancel()` also aborts the task
/// via the [`Drop`] implementation, preventing resource leaks.
pub struct RactorTimerHandle {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl TimerHandle for RactorTimerHandle {
    fn cancel(mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

impl Drop for RactorTimerHandle {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Group storage — type-erased ractor ActorRef storage for groups
// ---------------------------------------------------------------------------

type ErasedRef = Box<dyn std::any::Any + Send + Sync>;

/// Wrapper to store a typed ractor::ActorRef in type-erased group storage.
struct RefWrapper<M: Send + 'static>(ractor::ActorRef<M>);

// ---------------------------------------------------------------------------
// RactorRuntime
// ---------------------------------------------------------------------------

/// A dactor `ActorRuntime` implementation backed by ractor.
///
/// # Tokio Runtime Requirement
///
/// All methods that interact with the async ractor/tokio layer require a
/// running tokio runtime context. This includes [`spawn`](DactorActorRuntime::spawn),
/// [`send_interval`](DactorActorRuntime::send_interval), and
/// [`send_after`](DactorActorRuntime::send_after). Calling these outside a
/// tokio runtime will panic (`Handle::current()` / `tokio::spawn`).
///
/// **Performance note:** Each `spawn()` call creates a short-lived OS thread
/// to bridge the sync/async boundary. This is heavier than native ractor
/// spawning but is required by the synchronous `ActorRuntime::spawn` API.
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
pub struct RactorRuntime {
    groups: Arc<Mutex<HashMap<String, Vec<ErasedRef>>>>,
    cluster_events: RactorClusterEvents,
}

impl RactorRuntime {
    /// Create a new `RactorRuntime`.
    pub fn new() -> Self {
        Self {
            groups: Arc::new(Mutex::new(HashMap::new())),
            cluster_events: RactorClusterEvents::new(),
        }
    }

    /// Access the cluster events subsystem for manual event emission.
    pub fn cluster_events_handle(&self) -> &RactorClusterEvents {
        &self.cluster_events
    }
}

impl Default for RactorRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DactorActorRuntime for RactorRuntime {
    type Ref<M: Send + 'static> = RactorActorRef<M>;
    type Events = RactorClusterEvents;
    type Timer = RactorTimerHandle;

    fn spawn<M, H>(&self, name: &str, handler: H) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static,
    {
        let actor = HandlerActor::<M>::new();
        let boxed_handler: Box<dyn FnMut(M) + Send> = Box::new(handler);
        let actor_name = Some(name.to_string());

        // Bridge sync → async: spawn a std thread to run the async Actor::spawn,
        // then block on a channel to receive the resulting ActorRef.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            handle.block_on(async move {
                let result =
                    ractor::Actor::spawn(actor_name, actor, boxed_handler)
                        .await;
                match result {
                    Ok((actor_ref, _join)) => {
                        let _ = tx.send(Ok(actor_ref));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                    }
                }
            });
        });

        match rx.recv() {
            Ok(Ok(actor_ref)) => RactorActorRef { inner: actor_ref },
            Ok(Err(e)) => panic!("failed to spawn ractor actor: {e}"),
            Err(_) => panic!("ractor actor spawn channel closed unexpectedly"),
        }
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
                if actor_ref.cast(msg.clone()).is_err() {
                    break;
                }
            }
        });
        RactorTimerHandle {
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
            let _ = actor_ref.cast(msg);
        });
        RactorTimerHandle {
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
        let target_id = actor.inner.get_id();
        let mut groups = self.groups.lock().unwrap();
        if let Some(members) = groups.get_mut(group_name) {
            members.retain(|member| {
                if let Some(wrapper) = member.downcast_ref::<RefWrapper<M>>() {
                    wrapper.0.get_id() != target_id
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
                    let _ = wrapper.0.cast(msg.clone());
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
                    result.push(RactorActorRef {
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
