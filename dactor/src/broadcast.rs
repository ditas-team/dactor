//! Broadcast messaging for actor groups.
//!
//! A [`BroadcastRef`] holds an **owned, non-shared** list of actor references
//! of the same type and fans out `tell` (fire-and-forget) and `ask`
//! (request-reply) messages to every member concurrently.
//!
//! # Comparison with `PoolRef`
//!
//! - **`PoolRef`**: Routes messages to *one* selected member (round-robin, random, keyed).
//! - **`BroadcastRef`**: Fans out messages to *all* members.
//!
//! # Membership Management
//!
//! Membership is managed through `&mut self` methods ([`BroadcastRef::add`] /
//! [`BroadcastRef::remove`]), meaning the caller must own or exclusively borrow
//! the group.  This is intentional — `BroadcastRef` is a simple owned group,
//! not a thread-safe shared registry.  If you need concurrent membership
//! changes from multiple tasks, wrap the group in an
//! `Arc<tokio::sync::RwLock<BroadcastRef<…>>>` or build a dedicated membership
//! actor.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorRef, Handler};
use crate::dead_letter::{DeadLetterEvent, DeadLetterHandler, DeadLetterReason};
use crate::errors::{ActorSendError, RuntimeError};
use crate::interceptor::SendMode;
use crate::message::Message;
use crate::node::ActorId;

// ---------------------------------------------------------------------------
// BroadcastReceipt
// ---------------------------------------------------------------------------

/// Outcome of an `ask` to a single member in a broadcast group.
#[derive(Debug)]
pub enum BroadcastReceipt<R> {
    /// The actor replied successfully.
    Ok {
        /// Identity of the actor that replied.
        actor_id: ActorId,
        /// The reply value.
        reply: R,
    },
    /// The actor did not reply within the timeout.
    Timeout {
        /// Identity of the actor that timed out.
        actor_id: ActorId,
    },
    /// A send or transport-level failure prevented delivery to the actor.
    SendError {
        /// Identity of the actor that failed.
        actor_id: ActorId,
        /// The send error.
        error: ActorSendError,
    },
    /// The actor processed the message but the reply resolved to an error.
    ReplyError {
        /// Identity of the actor that failed.
        actor_id: ActorId,
        /// The runtime error from the reply channel.
        error: RuntimeError,
    },
}

// ---------------------------------------------------------------------------
// TellOutcome / BroadcastTellResult
// ---------------------------------------------------------------------------

/// Outcome of a `tell` to a single member in a broadcast group.
#[derive(Debug)]
pub struct BroadcastTellOutcome {
    /// Identity of the target actor.
    pub actor_id: ActorId,
    /// `Ok(())` if the message was enqueued, `Err` otherwise.
    pub result: Result<(), ActorSendError>,
}

/// Aggregated result of a broadcast `tell`.
#[must_use = "inspect outcomes to detect failed deliveries"]
#[derive(Debug)]
pub struct BroadcastTellResult {
    /// Per-actor outcomes, in the same order as the group members.
    pub outcomes: Vec<BroadcastTellOutcome>,
}

impl BroadcastTellResult {
    /// Number of actors that accepted the message.
    pub fn succeeded(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_ok()).count()
    }

    /// Number of actors that rejected the message.
    pub fn failed(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_err()).count()
    }
}

// ---------------------------------------------------------------------------
// BroadcastRef
// ---------------------------------------------------------------------------

/// An **owned, non-shared** reference to a group of actors, enabling
/// broadcast messaging.
///
/// Membership is mutated via `&mut self`, so the caller must own or
/// exclusively borrow the group.  For concurrent access, wrap in
/// `Arc<tokio::sync::RwLock<…>>`.
///
/// # Interceptors (BC7)
///
/// Outbound interceptors are **not** run at the broadcast level.  Each
/// member's [`ActorRef::tell`] / [`ActorRef::ask`] invocation runs through
/// whatever interceptor pipeline the underlying adapter has configured,
/// so interceptors execute **once per target actor**, not once for the
/// whole broadcast.
///
/// # Dead Letters (BC8)
///
/// Attach a [`DeadLetterHandler`] via [`with_dead_letter_handler`](Self::with_dead_letter_handler)
/// to be notified when a member cannot accept a message (actor stopped,
/// mailbox full, etc.).  Dead letters are emitted only for **send-level
/// failures**, not for timeouts or handler errors:
///
/// - [`BroadcastReceipt::SendError`] → dead letter emitted
/// - [`BroadcastReceipt::Timeout`] → **no** dead letter (use the receipt)
/// - [`BroadcastReceipt::ReplyError`] → **no** dead letter (message was delivered)
///
/// Without a handler, failures are still recorded in
/// [`BroadcastTellResult`] / [`BroadcastReceipt`] but not routed elsewhere.
///
/// Generic over actor type `A` and a single concrete [`ActorRef`]
/// implementation `R`. The constraint mirrors [`PoolRef`](crate::pool::PoolRef).
pub struct BroadcastRef<A: Actor, R: ActorRef<A>> {
    refs: Vec<R>,
    dead_letter_handler: Option<Arc<dyn DeadLetterHandler>>,
    _phantom: PhantomData<fn() -> A>,
}

impl<A: Actor, R: ActorRef<A>> Clone for BroadcastRef<A, R> {
    fn clone(&self) -> Self {
        Self {
            refs: self.refs.clone(),
            dead_letter_handler: self.dead_letter_handler.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<A: Actor, R: ActorRef<A>> Default for BroadcastRef<A, R> {
    fn default() -> Self {
        Self::new(vec![])
    }
}

impl<A: Actor, R: ActorRef<A>> BroadcastRef<A, R> {
    /// Create a broadcast group from a vector of actor references.
    ///
    /// An empty vector is allowed — `tell` / `ask` will simply return
    /// empty results.
    pub fn new(refs: Vec<R>) -> Self {
        Self {
            refs,
            dead_letter_handler: None,
            _phantom: PhantomData,
        }
    }

    /// Attach a [`DeadLetterHandler`] that is notified whenever a `tell` or
    /// `ask` cannot deliver to a member.
    pub fn with_dead_letter_handler(mut self, handler: Arc<dyn DeadLetterHandler>) -> Self {
        self.dead_letter_handler = Some(handler);
        self
    }

    /// Number of members in the group.
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    /// Returns `true` if the group has no members.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    /// Add an actor to the group.
    ///
    /// Duplicate [`ActorId`]s are not checked — adding the same actor twice
    /// causes it to receive messages twice per broadcast.  Call
    /// [`contains`](Self::contains) first if deduplication is needed.
    pub fn add(&mut self, actor_ref: R) {
        self.refs.push(actor_ref);
    }

    /// Returns `true` if the group contains a member with the given [`ActorId`].
    pub fn contains(&self, actor_id: &ActorId) -> bool {
        self.refs.iter().any(|r| r.id() == *actor_id)
    }

    /// Remove an actor from the group by its [`ActorId`].
    ///
    /// Uses `swap_remove` for O(1) removal, which **does not preserve**
    /// membership order.  If the group contains duplicate entries for the
    /// same [`ActorId`], only the first match is removed.  Returns
    /// `Some(actor_ref)` if an actor was found and removed, or `None` if
    /// no member matched.
    pub fn remove(&mut self, actor_id: &ActorId) -> Option<R> {
        let pos = self.refs.iter().position(|r| r.id() == *actor_id)?;
        Some(self.refs.swap_remove(pos))
    }

    /// Fire-and-forget: deliver a cloned message to every member.
    ///
    /// If a [`DeadLetterHandler`] is attached, failed sends are routed to
    /// it in addition to being recorded in the returned
    /// [`BroadcastTellResult`].
    pub fn tell<M>(&self, msg: M) -> BroadcastTellResult
    where
        A: Handler<M>,
        M: Message<Reply = ()> + Clone,
    {
        let mut outcomes = Vec::with_capacity(self.refs.len());
        for actor_ref in &self.refs {
            let result = actor_ref.tell(msg.clone());
            if let Err(ref err) = result {
                self.route_dead_letter::<M>(
                    &actor_ref.id(),
                    &actor_ref.name(),
                    err,
                    SendMode::Tell,
                );
            }
            outcomes.push(BroadcastTellOutcome {
                actor_id: actor_ref.id(),
                result,
            });
        }
        BroadcastTellResult { outcomes }
    }

    /// Request-reply: send a cloned message to every member and collect
    /// replies concurrently with a per-actor timeout.
    ///
    /// **Timeout semantics:** Each actor gets an independent timeout.  If the
    /// timeout fires before the actor replies, [`BroadcastReceipt::Timeout`]
    /// is returned and a [`CancellationToken`] is triggered.  Cancellation is
    /// **cooperative** — handlers that check [`ActorContext`](crate::actor::ActorContext)
    /// cancellation will exit early, but handlers that ignore it may continue
    /// running.
    ///
    /// If a [`DeadLetterHandler`] is attached, send-level failures are
    /// routed to it.
    pub async fn ask<M>(&self, msg: M, timeout: Duration) -> Vec<BroadcastReceipt<M::Reply>>
    where
        A: Handler<M> + 'static,
        M: Message + Clone,
    {
        let dl = self.dead_letter_handler.clone();
        let futures: Vec<_> = self
            .refs
            .iter()
            .map(|actor_ref| {
                let id = actor_ref.id();
                let name = actor_ref.name();
                let token = CancellationToken::new();
                let reply_future = actor_ref.ask(msg.clone(), Some(token.clone()));
                let dl = dl.clone();
                async move {
                    match reply_future {
                        Ok(ask_reply) => {
                            match tokio::time::timeout(timeout, ask_reply).await {
                                Ok(Ok(reply)) => BroadcastReceipt::Ok {
                                    actor_id: id,
                                    reply,
                                },
                                Ok(Err(RuntimeError::Send(error))) => {
                                    if let Some(ref handler) = dl {
                                        emit_dead_letter::<M>(
                                            handler, &id, &name, &error, SendMode::Ask,
                                        );
                                    }
                                    BroadcastReceipt::SendError {
                                        actor_id: id,
                                        error,
                                    }
                                }
                                // Handler-level errors are not dead letters —
                                // the message was delivered but processing failed.
                                Ok(Err(e)) => BroadcastReceipt::ReplyError {
                                    actor_id: id,
                                    error: e,
                                },
                                Err(_) => {
                                    token.cancel();
                                    BroadcastReceipt::Timeout { actor_id: id }
                                }
                            }
                        }
                        Err(e) => {
                            if let Some(ref handler) = dl {
                                emit_dead_letter::<M>(
                                    handler, &id, &name, &e, SendMode::Ask,
                                );
                            }
                            BroadcastReceipt::SendError {
                                actor_id: id,
                                error: e,
                            }
                        }
                    }
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    /// Route a failed send to the dead letter handler, if configured.
    fn route_dead_letter<M: Message>(
        &self,
        target_id: &ActorId,
        target_name: &str,
        error: &ActorSendError,
        send_mode: SendMode,
    ) {
        if let Some(ref handler) = self.dead_letter_handler {
            emit_dead_letter::<M>(handler, target_id, target_name, error, send_mode);
        }
    }
}

// ---------------------------------------------------------------------------
// Dead letter helper
// ---------------------------------------------------------------------------

fn emit_dead_letter<M: Message>(
    handler: &Arc<dyn DeadLetterHandler>,
    target_id: &ActorId,
    target_name: &str,
    error: &ActorSendError,
    send_mode: SendMode,
) {
    let reason = if error.0.contains("stopped") || error.0.contains("closed") {
        DeadLetterReason::ActorStopped
    } else if error.0.contains("not found") {
        DeadLetterReason::ActorNotFound
    } else if error.0.contains("full") {
        DeadLetterReason::MailboxFull
    } else {
        // Unrecognized error text — default to ActorStopped as the most
        // common cause. The original error string is preserved in the
        // BroadcastTellOutcome / BroadcastReceipt for precise diagnosis.
        DeadLetterReason::ActorStopped
    };
    let event = DeadLetterEvent {
        target_id: target_id.clone(),
        target_name: Some(target_name.to_owned()),
        message_type: std::any::type_name::<M>(),
        send_mode,
        reason,
        message: None,
    };
    // Guard against panicking custom handlers — don't let a bad handler
    // abort the entire broadcast.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.on_dead_letter(event);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use crate::actor::ActorContext;
    use crate::test_support::test_runtime::TestRuntime;

    // -- Shared test actor --------------------------------------------------

    #[derive(Clone)]
    struct Ping;
    impl Message for Ping {
        type Reply = ();
    }

    #[derive(Clone)]
    struct GetValue;
    impl Message for GetValue {
        type Reply = u64;
    }

    #[derive(Clone)]
    struct SlowMessage {
        delay_ms: u64,
    }
    impl Message for SlowMessage {
        type Reply = u64;
    }

    struct Accumulator {
        value: u64,
        received: Arc<Mutex<Vec<ActorId>>>,
    }

    impl Actor for Accumulator {
        type Args = (u64, Arc<Mutex<Vec<ActorId>>>);
        type Deps = ();
        fn create(args: Self::Args, _deps: ()) -> Self {
            Accumulator {
                value: args.0,
                received: args.1,
            }
        }
    }

    #[async_trait]
    impl Handler<Ping> for Accumulator {
        async fn handle(&mut self, _msg: Ping, ctx: &mut ActorContext) {
            self.received.lock().await.push(ctx.actor_id.clone());
        }
    }

    #[async_trait]
    impl Handler<GetValue> for Accumulator {
        async fn handle(&mut self, _msg: GetValue, _ctx: &mut ActorContext) -> u64 {
            self.value
        }
    }

    #[async_trait]
    impl Handler<SlowMessage> for Accumulator {
        async fn handle(&mut self, msg: SlowMessage, _ctx: &mut ActorContext) -> u64 {
            tokio::time::sleep(Duration::from_millis(msg.delay_ms)).await;
            self.value
        }
    }

    // -- Helpers ------------------------------------------------------------

    async fn spawn_group(
        rt: &TestRuntime,
        count: usize,
        received: Arc<Mutex<Vec<ActorId>>>,
    ) -> BroadcastRef<Accumulator, crate::test_support::test_runtime::TestActorRef<Accumulator>>
    {
        let mut refs = Vec::new();
        for i in 0..count {
            let r = rt
                .spawn::<Accumulator>(&format!("acc-{i}"), (i as u64, received.clone()))
                .await
                .unwrap();
            refs.push(r);
        }
        BroadcastRef::new(refs)
    }

    // -- Tests --------------------------------------------------------------

    #[tokio::test]
    async fn test_broadcast_tell_to_multiple() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 3, received.clone()).await;

        let result = group.tell(Ping);
        assert_eq!(result.succeeded(), 3);
        assert_eq!(result.failed(), 0);

        tokio::time::sleep(Duration::from_millis(100)).await;

        let ids = received.lock().await;
        assert_eq!(ids.len(), 3, "all 3 actors should have received Ping");
    }

    #[tokio::test]
    async fn test_broadcast_ask_all_succeed() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 3, received).await;

        let receipts = group.ask(GetValue, Duration::from_secs(1)).await;
        assert_eq!(receipts.len(), 3);

        let mut values: Vec<u64> = receipts
            .into_iter()
            .map(|r| match r {
                BroadcastReceipt::Ok { reply, .. } => reply,
                other => panic!("expected Ok, got {:?}", other),
            })
            .collect();
        values.sort();
        assert_eq!(values, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn test_broadcast_ask_with_timeout() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();

        // Spawn two actors: one fast, one slow
        let fast_ref = rt
            .spawn::<Accumulator>("fast", (42, received.clone()))
            .await
            .unwrap();
        let slow_ref = rt
            .spawn::<Accumulator>("slow", (99, received.clone()))
            .await
            .unwrap();

        let group = BroadcastRef::new(vec![fast_ref, slow_ref]);

        // All actors receive a 500ms-delayed message with a 50ms timeout
        let receipts = group
            .ask(SlowMessage { delay_ms: 500 }, Duration::from_millis(50))
            .await;

        assert_eq!(receipts.len(), 2);
        let timeouts = receipts
            .iter()
            .filter(|r| matches!(r, BroadcastReceipt::Timeout { .. }))
            .count();
        assert_eq!(timeouts, 2, "both should timeout");
    }

    #[tokio::test]
    async fn test_broadcast_ask_partial_failure() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 3, received).await;

        // Stop actor 1
        group.refs[1].stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let receipts = group.ask(GetValue, Duration::from_secs(1)).await;
        assert_eq!(receipts.len(), 3);

        let ok_count = receipts
            .iter()
            .filter(|r| matches!(r, BroadcastReceipt::Ok { .. }))
            .count();
        let err_count = receipts
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    BroadcastReceipt::SendError { .. } | BroadcastReceipt::ReplyError { .. }
                )
            })
            .count();

        assert_eq!(ok_count, 2);
        assert_eq!(err_count, 1);
    }

    #[tokio::test]
    async fn test_broadcast_empty_group() {
        let group: BroadcastRef<
            Accumulator,
            crate::test_support::test_runtime::TestActorRef<Accumulator>,
        > = BroadcastRef::new(vec![]);

        assert!(group.is_empty());
        assert_eq!(group.len(), 0);

        let tell_result = group.tell(Ping);
        assert_eq!(tell_result.succeeded(), 0);
        assert_eq!(tell_result.failed(), 0);
        assert!(tell_result.outcomes.is_empty());

        let ask_result = group.ask(GetValue, Duration::from_secs(1)).await;
        assert!(ask_result.is_empty());
    }

    #[tokio::test]
    async fn test_broadcast_add_remove() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let mut group = spawn_group(&rt, 2, received.clone()).await;

        assert_eq!(group.len(), 2);

        // Add a third actor
        let r = rt
            .spawn::<Accumulator>("acc-extra", (10, received.clone()))
            .await
            .unwrap();
        let extra_id = r.id();
        group.add(r);
        assert_eq!(group.len(), 3);

        // Remove it by id
        assert!(group.remove(&extra_id).is_some());
        assert_eq!(group.len(), 2);

        // Removing a non-existent id returns None
        let fake_id = ActorId {
            node: crate::node::NodeId("no-node".into()),
            local: 999,
        };
        assert!(group.remove(&fake_id).is_none());
        assert_eq!(group.len(), 2);
    }

    #[tokio::test]
    async fn test_broadcast_tell_result_counts() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 3, received).await;

        // Stop one actor
        group.refs[0].stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = group.tell(Ping);
        assert_eq!(result.succeeded(), 2);
        assert_eq!(result.failed(), 1);
        assert_eq!(result.outcomes.len(), 3);
    }

    /// Message whose handler sleeps for `actor.value` milliseconds.
    /// This lets us give different actors different delays via their state.
    #[derive(Clone)]
    struct SleepByValue;
    impl Message for SleepByValue {
        type Reply = u64;
    }

    #[async_trait]
    impl Handler<SleepByValue> for Accumulator {
        async fn handle(&mut self, _msg: SleepByValue, _ctx: &mut ActorContext) -> u64 {
            tokio::time::sleep(Duration::from_millis(self.value)).await;
            self.value
        }
    }

    #[tokio::test]
    async fn test_broadcast_ask_mixed_fast_slow() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();

        // value = delay in ms: fast replies in 10ms, slow replies in 500ms
        let fast_ref = rt
            .spawn::<Accumulator>("fast", (10, received.clone()))
            .await
            .unwrap();
        let slow_ref = rt
            .spawn::<Accumulator>("slow", (500, received.clone()))
            .await
            .unwrap();

        let fast_id = fast_ref.id();
        let slow_id = slow_ref.id();
        let group = BroadcastRef::new(vec![fast_ref, slow_ref]);

        // 100ms timeout: fast actor should succeed, slow actor should time out
        let receipts = group
            .ask(SleepByValue, Duration::from_millis(100))
            .await;

        assert_eq!(receipts.len(), 2);

        let mut ok_ids = Vec::new();
        let mut timeout_ids = Vec::new();
        for r in &receipts {
            match r {
                BroadcastReceipt::Ok { actor_id, reply, .. } => {
                    assert_eq!(*reply, 10);
                    ok_ids.push(actor_id.clone());
                }
                BroadcastReceipt::Timeout { actor_id } => {
                    timeout_ids.push(actor_id.clone());
                }
                other => panic!("unexpected receipt: {:?}", other),
            }
        }
        assert_eq!(ok_ids.len(), 1, "fast actor should succeed");
        assert_eq!(timeout_ids.len(), 1, "slow actor should timeout");
        assert_eq!(ok_ids[0], fast_id);
        assert_eq!(timeout_ids[0], slow_id);
    }

    #[tokio::test]
    async fn test_broadcast_default_is_empty() {
        let group: BroadcastRef<
            Accumulator,
            crate::test_support::test_runtime::TestActorRef<Accumulator>,
        > = BroadcastRef::default();

        assert!(group.is_empty());
        assert_eq!(group.len(), 0);

        let tell_result = group.tell(Ping);
        assert_eq!(tell_result.succeeded(), 0);

        let ask_result = group.ask(GetValue, Duration::from_secs(1)).await;
        assert!(ask_result.is_empty());
    }

    #[tokio::test]
    async fn test_broadcast_contains() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let mut group = spawn_group(&rt, 2, received.clone()).await;

        let id0 = group.refs[0].id();
        let id1 = group.refs[1].id();
        let fake_id = ActorId {
            node: crate::node::NodeId("no-node".into()),
            local: 999,
        };

        assert!(group.contains(&id0));
        assert!(group.contains(&id1));
        assert!(!group.contains(&fake_id));

        // After removal, contains returns false
        group.remove(&id0);
        assert!(!group.contains(&id0));
        assert!(group.contains(&id1));
    }

    #[tokio::test]
    async fn test_broadcast_duplicate_members() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let mut group = spawn_group(&rt, 1, received.clone()).await;

        // Add the same actor again (duplicate)
        let dup = group.refs[0].clone();
        group.add(dup);
        assert_eq!(group.len(), 2);

        let result = group.tell(Ping);
        assert_eq!(result.succeeded(), 2);

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Same actor received the message twice
        let ids = received.lock().await;
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1]);
    }

    #[tokio::test]
    async fn test_broadcast_ask_send_error_classification() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 2, received).await;

        // Stop one actor so its ask yields a send-level error
        group.refs[0].stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let receipts = group.ask(GetValue, Duration::from_secs(1)).await;
        assert_eq!(receipts.len(), 2);

        let mut has_ok = false;
        let mut has_send_err = false;
        for r in &receipts {
            match r {
                BroadcastReceipt::Ok { .. } => has_ok = true,
                BroadcastReceipt::SendError { .. } => has_send_err = true,
                BroadcastReceipt::ReplyError { .. } => {
                    // Also acceptable — some adapters surface send errors
                    // through the reply channel as RuntimeError::Send
                    has_send_err = true;
                }
                BroadcastReceipt::Timeout { .. } => {
                    panic!("unexpected timeout for a stopped actor")
                }
            }
        }
        assert!(has_ok, "live actor should reply");
        assert!(has_send_err, "stopped actor should yield SendError or ReplyError");
    }

    // -- BC6: PoolRef::to_broadcast() ---------------------------------------

    #[tokio::test]
    async fn test_pool_to_broadcast() {
        use crate::pool::{PoolRef, PoolRouting};

        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();

        let mut workers = Vec::new();
        for i in 0..3 {
            let r = rt
                .spawn::<Accumulator>(&format!("w-{i}"), (i as u64, received.clone()))
                .await
                .unwrap();
            workers.push(r);
        }

        let pool = PoolRef::new(workers, PoolRouting::RoundRobin);
        let group = pool.to_broadcast();

        assert_eq!(group.len(), 3);

        let result = group.tell(Ping);
        assert_eq!(result.succeeded(), 3);

        tokio::time::sleep(Duration::from_millis(100)).await;
        let ids = received.lock().await;
        assert_eq!(ids.len(), 3);
    }

    // -- BC8: Dead letter routing -------------------------------------------

    #[tokio::test]
    async fn test_broadcast_tell_dead_letter_routing() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 3, received).await;

        let collector = Arc::new(crate::dead_letter::CollectingDeadLetterHandler::new());
        let group = group.with_dead_letter_handler(collector.clone() as Arc<dyn DeadLetterHandler>);

        // Stop one actor
        group.refs[1].stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = group.tell(Ping);
        assert_eq!(result.succeeded(), 2);
        assert_eq!(result.failed(), 1);

        // Dead letter handler should have been notified
        let events = collector.events();
        assert_eq!(events.len(), 1, "exactly one dead letter expected");
        assert_eq!(events[0].target_id, group.refs[1].id());
        assert!(
            matches!(events[0].send_mode, SendMode::Tell),
            "dead letter from tell should have SendMode::Tell"
        );
        assert!(
            matches!(events[0].reason, crate::dead_letter::DeadLetterReason::ActorStopped),
            "stopped actor should produce ActorStopped reason"
        );
        assert!(
            events[0].target_name.is_some(),
            "target_name should be populated"
        );
    }

    #[tokio::test]
    async fn test_broadcast_ask_dead_letter_routing() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = TestRuntime::new();
        let group = spawn_group(&rt, 2, received).await;

        let collector = Arc::new(crate::dead_letter::CollectingDeadLetterHandler::new());
        let group = group.with_dead_letter_handler(collector.clone() as Arc<dyn DeadLetterHandler>);

        // Stop one actor
        group.refs[0].stop();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let receipts = group.ask(GetValue, Duration::from_secs(1)).await;
        assert_eq!(receipts.len(), 2);

        // Dead letter handler should be notified for the stopped actor
        let events = collector.events();
        assert!(
            !events.is_empty(),
            "dead letter handler should be notified for stopped actor"
        );
        assert!(
            matches!(events[0].send_mode, SendMode::Ask),
            "dead letter from ask should have SendMode::Ask"
        );
    }
}
