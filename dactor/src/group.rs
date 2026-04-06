//! Named processing groups for actor pub/sub.
//!
//! A [`ProcessingGroup`] is a named, typed collection of actor references
//! that enables group-based messaging patterns (pub/sub, work distribution).
//!
//! # Usage
//!
//! ```ignore
//! let mut workers = ProcessingGroup::new("workers");
//! workers.join(actor_ref_1);
//! workers.join(actor_ref_2);
//!
//! // Broadcast to all members via BroadcastRef
//! let broadcast = workers.to_broadcast();
//! let result = broadcast.tell(MyMessage);
//!
//! // List members
//! assert_eq!(workers.len(), 2);
//! ```
//!
//! # Relation to `BroadcastRef`
//!
//! `ProcessingGroup` manages **membership** (join/leave/list).
//! [`BroadcastRef`](crate::broadcast::BroadcastRef) manages **messaging**
//! (tell/ask to all members).  They compose:
//! `group.to_broadcast()` returns a `BroadcastRef` snapshot.
//!
//! # Thread Safety
//!
//! `ProcessingGroup` is an owned, non-shared struct.  For concurrent access
//! from multiple tasks, wrap in `Arc<tokio::sync::RwLock<ProcessingGroup<…>>>`.

use std::collections::HashMap;
use std::marker::PhantomData;

use crate::actor::{Actor, ActorRef};
use crate::broadcast::BroadcastRef;
use crate::node::ActorId;

/// A named, typed group of actor references.
///
/// Members are keyed by [`ActorId`] for O(1) join/leave/lookup.
/// Iteration order is **not guaranteed** — if deterministic ordering
/// matters, sort the results or use [`BroadcastRef`] directly.
///
/// Duplicate joins replace the existing member and return the old
/// reference (see [`join`](Self::join)).
pub struct ProcessingGroup<A: Actor, R: ActorRef<A>> {
    name: String,
    members: HashMap<ActorId, R>,
    _phantom: PhantomData<fn() -> A>,
}

impl<A: Actor, R: ActorRef<A>> ProcessingGroup<A, R> {
    /// Create a new empty processing group with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            members: HashMap::new(),
            _phantom: PhantomData,
        }
    }

    /// The group's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of members in the group.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Returns `true` if the group has no members.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Add an actor to the group.
    ///
    /// If an actor with the same [`ActorId`] is already a member, the
    /// reference is updated (replaced) and the old reference is returned.
    pub fn join(&mut self, actor_ref: R) -> Option<R> {
        let id = actor_ref.id();
        self.members.insert(id, actor_ref)
    }

    /// Remove an actor from the group by its [`ActorId`].
    ///
    /// Returns `Some(actor_ref)` if the actor was found, `None` otherwise.
    /// Removing an actor does **not** stop it.
    pub fn leave(&mut self, actor_id: &ActorId) -> Option<R> {
        self.members.remove(actor_id)
    }

    /// Returns `true` if the group contains the given [`ActorId`].
    pub fn contains(&self, actor_id: &ActorId) -> bool {
        self.members.contains_key(actor_id)
    }

    /// Returns a reference to the actor ref for the given [`ActorId`], if present.
    pub fn get(&self, actor_id: &ActorId) -> Option<&R> {
        self.members.get(actor_id)
    }

    /// Returns an iterator over all member actor references.
    pub fn members(&self) -> impl Iterator<Item = &R> {
        self.members.values()
    }

    /// Returns a vector of all member [`ActorId`]s.
    pub fn member_ids(&self) -> Vec<ActorId> {
        self.members.keys().cloned().collect()
    }

    /// Create a [`BroadcastRef`] snapshot of the current group members.
    ///
    /// Clones all member refs. Later membership changes are **not**
    /// reflected in the returned `BroadcastRef`.
    pub fn to_broadcast(&self) -> BroadcastRef<A, R> {
        BroadcastRef::new(self.members.values().cloned().collect())
    }

    /// Remove all members from the group.
    pub fn clear(&mut self) {
        self.members.clear();
    }

    /// Remove all actors that are no longer alive.
    ///
    /// **Note:** This is a best-effort snapshot — an actor may stop
    /// immediately after being checked.  Callers should still handle
    /// `ActorSendError` when messaging group members.
    ///
    /// Removing an actor from the group does **not** stop it.
    ///
    /// Returns the number of members removed.
    pub fn prune_dead(&mut self) -> usize {
        let before = self.members.len();
        self.members.retain(|_, r| r.is_alive());
        before - self.members.len()
    }
}

impl<A: Actor, R: ActorRef<A>> Clone for ProcessingGroup<A, R> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            members: self.members.clone(),
            _phantom: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use crate::actor::{ActorContext, Handler};
    use crate::message::Message;
    use crate::test_support::test_runtime::TestRuntime;

    // -- Test actor ---

    struct Worker {
        received: Arc<Mutex<Vec<ActorId>>>,
    }

    impl Actor for Worker {
        type Args = Arc<Mutex<Vec<ActorId>>>;
        type Deps = ();
        fn create(args: Self::Args, _deps: ()) -> Self {
            Worker { received: args }
        }
    }

    #[derive(Clone)]
    struct Ping;
    impl Message for Ping {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<Ping> for Worker {
        async fn handle(&mut self, _msg: Ping, ctx: &mut ActorContext) {
            self.received.lock().await.push(ctx.actor_id.clone());
        }
    }

    #[derive(Clone)]
    struct GetId;
    impl Message for GetId {
        type Reply = String;
    }

    #[async_trait]
    impl Handler<GetId> for Worker {
        async fn handle(&mut self, _msg: GetId, ctx: &mut ActorContext) -> String {
            format!("{}", ctx.actor_id.local)
        }
    }

    // -- Tests ---

    #[tokio::test]
    async fn test_group_join_and_leave() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("workers");

        let a = rt.spawn::<Worker>("w-0", received.clone()).await.unwrap();
        let b = rt.spawn::<Worker>("w-1", received.clone()).await.unwrap();
        let a_id = a.id();
        let b_id = b.id();

        assert!(group.is_empty());

        group.join(a);
        group.join(b);
        assert_eq!(group.len(), 2);
        assert!(group.contains(&a_id));
        assert!(group.contains(&b_id));

        group.leave(&a_id);
        assert_eq!(group.len(), 1);
        assert!(!group.contains(&a_id));
        assert!(group.contains(&b_id));
    }

    #[tokio::test]
    async fn test_group_duplicate_join_replaces() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("workers");

        let a = rt.spawn::<Worker>("w-0", received.clone()).await.unwrap();
        let a_clone = a.clone();

        assert!(group.join(a).is_none()); // first join
        assert!(group.join(a_clone).is_some()); // duplicate — returns old
        assert_eq!(group.len(), 1); // still 1 member
    }

    #[tokio::test]
    async fn test_group_to_broadcast() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("workers");

        for i in 0..3 {
            let w = rt
                .spawn::<Worker>(&format!("w-{i}"), received.clone())
                .await
                .unwrap();
            group.join(w);
        }

        let broadcast = group.to_broadcast();
        let result = broadcast.tell(Ping);
        assert_eq!(result.succeeded(), 3);

        tokio::time::sleep(Duration::from_millis(100)).await;
        let ids = received.lock().await;
        assert_eq!(ids.len(), 3);
    }

    #[tokio::test]
    async fn test_group_prune_dead() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("workers");

        let a = rt.spawn::<Worker>("w-0", received.clone()).await.unwrap();
        let b = rt.spawn::<Worker>("w-1", received.clone()).await.unwrap();

        group.join(a.clone());
        group.join(b);

        assert_eq!(group.len(), 2);

        // Stop one actor
        a.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let pruned = group.prune_dead();
        assert_eq!(pruned, 1);
        assert_eq!(group.len(), 1);
    }

    #[tokio::test]
    async fn test_group_broadcast_ask() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("workers");

        for i in 0..3 {
            let w = rt
                .spawn::<Worker>(&format!("w-{i}"), received.clone())
                .await
                .unwrap();
            group.join(w);
        }

        let broadcast = group.to_broadcast();
        let receipts = broadcast.ask(GetId, Duration::from_secs(1)).await;
        assert_eq!(receipts.len(), 3);

        let mut replies: Vec<String> = receipts
            .into_iter()
            .filter_map(|r| match r {
                crate::broadcast::BroadcastReceipt::Ok { reply, .. } => Some(reply),
                _ => None,
            })
            .collect();
        replies.sort();
        assert_eq!(replies.len(), 3);
    }

    #[tokio::test]
    async fn test_group_name_and_member_ids() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("my-group");

        let a = rt.spawn::<Worker>("w-0", received.clone()).await.unwrap();
        let b = rt.spawn::<Worker>("w-1", received.clone()).await.unwrap();
        let a_id = a.id();
        let b_id = b.id();

        group.join(a);
        group.join(b);

        assert_eq!(group.name(), "my-group");

        let ids = group.member_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a_id));
        assert!(ids.contains(&b_id));
    }

    #[tokio::test]
    async fn test_group_empty_broadcast() {
        let group: ProcessingGroup<
            Worker,
            crate::test_support::test_runtime::TestActorRef<Worker>,
        > = ProcessingGroup::new("empty");

        let broadcast = group.to_broadcast();
        let result = broadcast.tell(Ping);
        assert_eq!(result.succeeded(), 0);
        assert!(result.outcomes.is_empty());
    }

    #[tokio::test]
    async fn test_group_leave_nonexistent() {
        let mut group: ProcessingGroup<
            Worker,
            crate::test_support::test_runtime::TestActorRef<Worker>,
        > = ProcessingGroup::new("g");

        let fake_id = crate::node::ActorId {
            node: crate::node::NodeId("none".into()),
            local: 999,
        };
        assert!(group.leave(&fake_id).is_none());
    }

    #[tokio::test]
    async fn test_group_get() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("g");

        let a = rt.spawn::<Worker>("w-0", received.clone()).await.unwrap();
        let a_id = a.id();
        group.join(a);

        assert!(group.get(&a_id).is_some());

        let fake_id = crate::node::ActorId {
            node: crate::node::NodeId("none".into()),
            local: 999,
        };
        assert!(group.get(&fake_id).is_none());
    }

    #[tokio::test]
    async fn test_group_clear() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("g");

        for i in 0..3 {
            let w = rt
                .spawn::<Worker>(&format!("w-{i}"), received.clone())
                .await
                .unwrap();
            group.join(w);
        }
        assert_eq!(group.len(), 3);

        group.clear();
        assert!(group.is_empty());
        assert_eq!(group.len(), 0);
    }

    #[tokio::test]
    async fn test_group_snapshot_independence() {
        let rt = TestRuntime::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut group = ProcessingGroup::new("g");

        let a = rt.spawn::<Worker>("w-0", received.clone()).await.unwrap();
        group.join(a);

        // Snapshot with 1 member
        let snap1 = group.to_broadcast();
        assert_eq!(snap1.len(), 1);

        // Add another member — snapshot should be unaffected
        let b = rt.spawn::<Worker>("w-1", received.clone()).await.unwrap();
        group.join(b);
        let snap2 = group.to_broadcast();

        assert_eq!(snap1.len(), 1, "old snapshot should still have 1 member");
        assert_eq!(snap2.len(), 2, "new snapshot should have 2 members");
    }
}
