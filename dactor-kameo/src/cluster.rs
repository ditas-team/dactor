use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dactor::{ClusterError, ClusterEvent, ClusterEvents, SubscriptionId};

type SubscriberMap = HashMap<SubscriptionId, Arc<dyn Fn(ClusterEvent) + Send + Sync>>;

/// A dactor `ClusterEvents` implementation for the kameo adapter.
///
/// Provides a callback-based subscription system. In production, an
/// integration with kameo's `ActorSwarm` would feed real membership changes
/// into this subsystem via [`KameoClusterEvents::emit`].
#[derive(Clone)]
pub struct KameoClusterEvents {
    subscribers: Arc<Mutex<SubscriberMap>>,
    next_id: Arc<AtomicU64>,
}

impl KameoClusterEvents {
    /// Create a new cluster events subsystem.
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Emit a cluster event, notifying all subscribers.
    ///
    /// Callbacks are snapshot-cloned before invocation so that subscribers
    /// may safely call `subscribe` or `unsubscribe` from within a callback
    /// without deadlocking.
    ///
    /// In production, this would be driven by kameo `ActorSwarm` membership
    /// change notifications. For testing, call this directly.
    pub fn emit(&self, event: ClusterEvent) {
        let snapshot: Vec<_> = {
            let subs = self.subscribers.lock().unwrap();
            subs.values().cloned().collect()
        };
        for sub in snapshot {
            sub(event.clone());
        }
    }
}

impl Default for KameoClusterEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterEvents for KameoClusterEvents {
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<SubscriptionId, ClusterError> {
        let id = SubscriptionId::from_raw(self.next_id.fetch_add(1, Ordering::SeqCst));
        let mut subs = self.subscribers.lock().unwrap();
        subs.insert(id, Arc::from(on_event));
        Ok(id)
    }

    fn unsubscribe(&self, id: SubscriptionId) -> Result<(), ClusterError> {
        let mut subs = self.subscribers.lock().unwrap();
        subs.remove(&id);
        Ok(())
    }
}
