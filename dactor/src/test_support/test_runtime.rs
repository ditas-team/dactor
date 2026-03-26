use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::traits::runtime::{
    ActorRef, ActorRuntime, ActorSendError, ClusterError, ClusterEvent,
    ClusterEvents, GroupError, SubscriptionId, TimerHandle,
};

// ---------------------------------------------------------------------------
// TestActorRef
// ---------------------------------------------------------------------------

/// A test actor reference backed by a channel.
pub struct TestActorRef<M: Send + 'static> {
    sender: tokio::sync::mpsc::UnboundedSender<M>,
    name: String,
}

// Manual Clone — UnboundedSender is Clone without M: Clone
impl<M: Send + 'static> Clone for TestActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            name: self.name.clone(),
        }
    }
}

impl<M: Send + 'static> std::fmt::Debug for TestActorRef<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TestActorRef({})", self.name)
    }
}

impl<M: Send + 'static> ActorRef<M> for TestActorRef<M> {
    fn send(&self, msg: M) -> Result<(), ActorSendError> {
        self.sender
            .send(msg)
            .map_err(|_| ActorSendError("actor stopped".into()))
    }
}

// Safety: UnboundedSender<M> is Send+Sync, String is Send+Sync
unsafe impl<M: Send + 'static> Send for TestActorRef<M> {}
unsafe impl<M: Send + 'static> Sync for TestActorRef<M> {}

// ---------------------------------------------------------------------------
// Group storage internals
// ---------------------------------------------------------------------------

type ErasedSender = Box<dyn std::any::Any + Send + Sync>;

/// Wrapper to erase M from the sender so it can be stored as dyn Any.
struct SenderWrapper<M: Send + 'static>(tokio::sync::mpsc::UnboundedSender<M>);

impl<M: Send + 'static> SenderWrapper<M> {
    fn send(&self, msg: M) -> Result<(), ActorSendError> {
        self.0
            .send(msg)
            .map_err(|_| ActorSendError("actor stopped".into()))
    }
}

// ---------------------------------------------------------------------------
// TestClusterEvents
// ---------------------------------------------------------------------------

type ClusterSubscriberMap = HashMap<SubscriptionId, Box<dyn Fn(ClusterEvent) + Send + Sync>>;

/// Test cluster events that can be triggered manually.
#[derive(Clone)]
pub struct TestClusterEvents {
    subscribers: Arc<Mutex<ClusterSubscriberMap>>,
    next_id: Arc<AtomicU64>,
}

impl TestClusterEvents {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Simulate a cluster event, notifying all subscribers.
    pub fn emit(&self, event: ClusterEvent) {
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.values() {
            sub(event.clone());
        }
    }
}

impl Default for TestClusterEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterEvents for TestClusterEvents {
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<SubscriptionId, ClusterError> {
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::SeqCst));
        let mut subs = self.subscribers.lock().unwrap();
        subs.insert(id, on_event);
        Ok(id)
    }

    fn unsubscribe(&self, id: SubscriptionId) -> Result<(), ClusterError> {
        let mut subs = self.subscribers.lock().unwrap();
        subs.remove(&id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TestTimerHandle
// ---------------------------------------------------------------------------

/// Test timer handle backed by a tokio JoinHandle.
pub struct TestTimerHandle {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl TimerHandle for TestTimerHandle {
    fn cancel(mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// TestRuntime
// ---------------------------------------------------------------------------

/// A mock `ActorRuntime` for testing without any actor framework dependency.
/// Actors are spawned as tokio tasks with channel-based mailboxes.
#[derive(Clone)]
pub struct TestRuntime {
    groups: Arc<Mutex<HashMap<String, Vec<ErasedSender>>>>,
    cluster_events: TestClusterEvents,
}

impl TestRuntime {
    pub fn new() -> Self {
        Self {
            groups: Arc::new(Mutex::new(HashMap::new())),
            cluster_events: TestClusterEvents::new(),
        }
    }

    /// Access the cluster events for test-controlled simulation.
    pub fn test_cluster_events(&self) -> &TestClusterEvents {
        &self.cluster_events
    }
}

impl Default for TestRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ActorRuntime for TestRuntime {
    type Ref<M: Send + 'static> = TestActorRef<M>;
    type Events = TestClusterEvents;
    type Timer = TestTimerHandle;

    fn spawn<M, H>(
        &self,
        name: &str,
        mut handler: H,
    ) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static,
    {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<M>();
        let actor_name = name.to_string();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                handler(msg);
            }
            tracing::debug!(actor = %actor_name, "actor mailbox closed");
        });
        TestActorRef {
            sender: tx,
            name: name.to_string(),
        }
    }

    fn send_interval<M: Clone + Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        interval: Duration,
        msg: M,
    ) -> Self::Timer {
        let sender = target.sender.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // first tick is immediate, skip it
            loop {
                ticker.tick().await;
                if sender.send(msg.clone()).is_err() {
                    break;
                }
            }
        });
        TestTimerHandle {
            handle: Some(handle),
        }
    }

    fn send_after<M: Send + 'static>(
        &self,
        target: &Self::Ref<M>,
        delay: Duration,
        msg: M,
    ) -> Self::Timer {
        let sender = target.sender.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = sender.send(msg);
        });
        TestTimerHandle {
            handle: Some(handle),
        }
    }

    fn join_group<M: Send + 'static>(
        &self,
        group_name: &str,
        actor: &Self::Ref<M>,
    ) -> Result<(), GroupError> {
        let wrapper = SenderWrapper(actor.sender.clone());
        let mut groups = self.groups.lock().unwrap();
        let group = groups.entry(group_name.to_string()).or_default();
        group.push(Box::new(wrapper));
        Ok(())
    }

    fn leave_group<M: Send + 'static>(
        &self,
        _group_name: &str,
        _actor: &Self::Ref<M>,
    ) -> Result<(), GroupError> {
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
                if let Some(wrapper) = member.downcast_ref::<SenderWrapper<M>>() {
                    let _ = wrapper.send(msg.clone());
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
                if let Some(wrapper) = member.downcast_ref::<SenderWrapper<M>>() {
                    result.push(TestActorRef {
                        sender: wrapper.0.clone(),
                        name: format!("group-member-{group_name}"),
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
