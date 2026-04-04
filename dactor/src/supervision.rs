//! Supervision primitives for the actor system.
//!
//! Provides [`ChildTerminated`], the notification delivered to watchers when a
//! watched actor stops.  Actors that call `watch()` on the runtime must
//! implement `Handler<ChildTerminated>` to receive these notifications.
//!
//! Also provides [`SupervisionStrategy`] and built-in strategies
//! ([`OneForOne`], [`AllForOne`], [`RestForOne`]) for controlling how a
//! supervisor reacts when a child actor fails.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::actor::ActorError;
use crate::message::Message;
use crate::node::{ActorId, NodeId};

/// Notification delivered when a watched actor terminates.
///
/// Actors that call `watch()` must implement `Handler<ChildTerminated>`
/// to receive these notifications.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChildTerminated {
    /// The ID of the actor that terminated.
    pub child_id: ActorId,
    /// The name the actor was spawned with.
    pub child_name: String,
    /// `None` for graceful shutdown, `Some(reason)` for failure.
    pub reason: Option<String>,
}

impl Message for ChildTerminated {
    type Reply = ();
}

// ---------------------------------------------------------------------------
// Supervision strategy types
// ---------------------------------------------------------------------------

/// Action a supervisor should take when a child fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionAction {
    /// Restart the failed child (and possibly siblings, depending on strategy).
    Restart,
    /// Stop the failed child permanently.
    Stop,
    /// Escalate the failure to this supervisor's parent.
    Escalate,
}

/// Determines how a supervisor reacts to child failures.
pub trait SupervisionStrategy: Send + Sync + 'static {
    /// Called when a child actor fails. Returns the action the supervisor
    /// should take.
    fn on_child_failed(
        &self,
        child_id: &ActorId,
        child_name: &str,
        error: &ActorError,
    ) -> SupervisionAction;
}

// ---------------------------------------------------------------------------
// Restart-rate tracking (shared by all built-in strategies)
// ---------------------------------------------------------------------------

struct RestartTracker {
    max_restarts: u32,
    within: Duration,
    timestamps: Mutex<HashMap<ActorId, Vec<Instant>>>,
}

impl RestartTracker {
    fn new(max_restarts: u32, within: Duration) -> Self {
        Self {
            max_restarts,
            within,
            timestamps: Mutex::new(HashMap::new()),
        }
    }

    /// Record a restart attempt for a specific child. Returns `true` if
    /// allowed, `false` if max rate exceeded. Used by OneForOne.
    fn record(&self, child_id: &ActorId) -> bool {
        let now = Instant::now();
        let mut map = self.timestamps.lock().unwrap();
        let entries = map.entry(child_id.clone()).or_default();

        // Evict timestamps outside the window
        entries.retain(|t| now.duration_since(*t) <= self.within);

        if entries.len() as u32 >= self.max_restarts {
            false
        } else {
            entries.push(now);
            true
        }
    }

    /// Record a restart attempt against a global group counter. Returns
    /// `true` if allowed, `false` if max rate exceeded.
    /// Used by AllForOne and RestForOne where the restart budget applies
    /// to the entire supervision group, not individual children.
    fn record_global(&self) -> bool {
        let sentinel = ActorId {
            node: NodeId("__supervision_group__".into()),
            local: 0,
        };
        self.record(&sentinel)
    }
}

// ---------------------------------------------------------------------------
// OneForOne — restart only the failed child
// ---------------------------------------------------------------------------

/// Restart only the failed child actor. This is the default strategy.
///
/// If `max_restarts` is exceeded within `within`, returns [`SupervisionAction::Stop`].
pub struct OneForOne {
    tracker: RestartTracker,
}

impl OneForOne {
    /// Create a new OneForOne strategy with the given restart limits.
    pub fn new(max_restarts: u32, within: Duration) -> Self {
        Self {
            tracker: RestartTracker::new(max_restarts, within),
        }
    }
}

impl SupervisionStrategy for OneForOne {
    fn on_child_failed(
        &self,
        child_id: &ActorId,
        _child_name: &str,
        _error: &ActorError,
    ) -> SupervisionAction {
        if self.tracker.record(child_id) {
            SupervisionAction::Restart
        } else {
            SupervisionAction::Stop
        }
    }
}

// ---------------------------------------------------------------------------
// AllForOne — restart all children when one fails
// ---------------------------------------------------------------------------

/// Restart **all** children when any one fails.
///
/// If `max_restarts` is exceeded within `within`, returns [`SupervisionAction::Stop`].
pub struct AllForOne {
    tracker: RestartTracker,
}

impl AllForOne {
    /// Create a new AllForOne strategy with the given restart limits.
    pub fn new(max_restarts: u32, within: Duration) -> Self {
        Self {
            tracker: RestartTracker::new(max_restarts, within),
        }
    }
}

impl SupervisionStrategy for AllForOne {
    fn on_child_failed(
        &self,
        _child_id: &ActorId,
        _child_name: &str,
        _error: &ActorError,
    ) -> SupervisionAction {
        if self.tracker.record_global() {
            SupervisionAction::Restart
        } else {
            SupervisionAction::Stop
        }
    }
}

// ---------------------------------------------------------------------------
// RestForOne — restart the failed child + all children started after it
// ---------------------------------------------------------------------------

/// Restart the failed child **and** all children that were started after it
/// (based on insertion order).
///
/// Call [`RestForOne::register_child`] in spawn order to track ordering.
/// If `max_restarts` is exceeded within `within`, returns [`SupervisionAction::Stop`].
pub struct RestForOne {
    tracker: RestartTracker,
    children: Mutex<Vec<ActorId>>,
}

impl RestForOne {
    /// Create a new RestForOne strategy with the given restart limits.
    pub fn new(max_restarts: u32, within: Duration) -> Self {
        Self {
            tracker: RestartTracker::new(max_restarts, within),
            children: Mutex::new(Vec::new()),
        }
    }

    /// Register a child in spawn order. Must be called for each child actor
    /// when it is spawned so the strategy knows the ordering.
    pub fn register_child(&self, child_id: ActorId) {
        self.children.lock().unwrap().push(child_id);
    }

    /// Returns the IDs of all children that should be restarted when
    /// `failed_id` fails: the failed child itself plus every child that was
    /// registered after it.
    pub fn children_to_restart(&self, failed_id: &ActorId) -> Vec<ActorId> {
        let children = self.children.lock().unwrap();
        if let Some(pos) = children.iter().position(|id| id == failed_id) {
            children[pos..].to_vec()
        } else {
            vec![failed_id.clone()]
        }
    }
}

impl SupervisionStrategy for RestForOne {
    fn on_child_failed(
        &self,
        _child_id: &ActorId,
        _child_name: &str,
        _error: &ActorError,
    ) -> SupervisionAction {
        if self.tracker.record_global() {
            SupervisionAction::Restart
        } else {
            SupervisionAction::Stop
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ActorError;
    use crate::node::{ActorId, NodeId};

    fn test_id(local: u64) -> ActorId {
        ActorId {
            node: NodeId("test".into()),
            local,
        }
    }

    fn test_error() -> ActorError {
        ActorError::internal("test failure")
    }

    // -- OneForOne ----------------------------------------------------------

    #[test]
    fn one_for_one_returns_restart() {
        let strategy = OneForOne::new(5, Duration::from_secs(60));
        let id = test_id(1);
        let action = strategy.on_child_failed(&id, "child-1", &test_error());
        assert_eq!(action, SupervisionAction::Restart);
    }

    #[test]
    fn one_for_one_max_exceeded_returns_stop() {
        let strategy = OneForOne::new(3, Duration::from_secs(60));
        let id = test_id(1);
        for _ in 0..3 {
            assert_eq!(
                strategy.on_child_failed(&id, "child-1", &test_error()),
                SupervisionAction::Restart,
            );
        }
        // 4th restart exceeds the limit
        assert_eq!(
            strategy.on_child_failed(&id, "child-1", &test_error()),
            SupervisionAction::Stop,
        );
    }

    #[test]
    fn one_for_one_tracks_per_child() {
        let strategy = OneForOne::new(2, Duration::from_secs(60));
        let id1 = test_id(1);
        let id2 = test_id(2);

        assert_eq!(strategy.on_child_failed(&id1, "a", &test_error()), SupervisionAction::Restart);
        assert_eq!(strategy.on_child_failed(&id1, "a", &test_error()), SupervisionAction::Restart);
        // id1 has exhausted its quota
        assert_eq!(strategy.on_child_failed(&id1, "a", &test_error()), SupervisionAction::Stop);
        // id2 is independent — still has quota
        assert_eq!(strategy.on_child_failed(&id2, "b", &test_error()), SupervisionAction::Restart);
    }

    // -- AllForOne ----------------------------------------------------------

    #[test]
    fn all_for_one_returns_restart() {
        let strategy = AllForOne::new(5, Duration::from_secs(60));
        let id = test_id(1);
        assert_eq!(
            strategy.on_child_failed(&id, "child-1", &test_error()),
            SupervisionAction::Restart,
        );
    }

    #[test]
    fn all_for_one_max_exceeded_returns_stop() {
        let strategy = AllForOne::new(2, Duration::from_secs(60));
        let id = test_id(1);
        for _ in 0..2 {
            assert_eq!(
                strategy.on_child_failed(&id, "child-1", &test_error()),
                SupervisionAction::Restart,
            );
        }
        assert_eq!(
            strategy.on_child_failed(&id, "child-1", &test_error()),
            SupervisionAction::Stop,
        );
    }

    // -- RestForOne ---------------------------------------------------------

    #[test]
    fn rest_for_one_returns_restart() {
        let strategy = RestForOne::new(5, Duration::from_secs(60));
        let id = test_id(1);
        strategy.register_child(id.clone());
        assert_eq!(
            strategy.on_child_failed(&id, "child-1", &test_error()),
            SupervisionAction::Restart,
        );
    }

    #[test]
    fn rest_for_one_children_to_restart() {
        let strategy = RestForOne::new(5, Duration::from_secs(60));
        let id1 = test_id(1);
        let id2 = test_id(2);
        let id3 = test_id(3);
        strategy.register_child(id1.clone());
        strategy.register_child(id2.clone());
        strategy.register_child(id3.clone());

        // Failing child 2 should restart child 2 and child 3
        let to_restart = strategy.children_to_restart(&id2);
        assert_eq!(to_restart, vec![id2.clone(), id3.clone()]);

        // Failing child 1 should restart all
        let to_restart = strategy.children_to_restart(&id1);
        assert_eq!(to_restart, vec![id1.clone(), id2.clone(), id3.clone()]);

        // Failing child 3 should restart only child 3
        let to_restart = strategy.children_to_restart(&id3);
        assert_eq!(to_restart, vec![id3.clone()]);
    }

    #[test]
    fn rest_for_one_max_exceeded_returns_stop() {
        let strategy = RestForOne::new(2, Duration::from_secs(60));
        let id = test_id(1);
        strategy.register_child(id.clone());
        for _ in 0..2 {
            assert_eq!(
                strategy.on_child_failed(&id, "child-1", &test_error()),
                SupervisionAction::Restart,
            );
        }
        assert_eq!(
            strategy.on_child_failed(&id, "child-1", &test_error()),
            SupervisionAction::Stop,
        );
    }
}
