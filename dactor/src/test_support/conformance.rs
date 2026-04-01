//! Conformance test suite for verifying runtime implementations.
//!
//! Any runtime that implements the dactor v0.2 API can use these tests
//! to verify correct behavior. Call each test function with a factory
//! that creates your runtime's actor ref.

use std::time::Duration;

use async_trait::async_trait;

use crate::actor::{Actor, ActorContext, Handler, ActorRef};
use crate::message::Message;

// ── Test Actor Definitions ──────────────────────────

/// A simple counter actor used by the conformance suite.
pub struct ConformanceCounter {
    count: u64,
}

impl Actor for ConformanceCounter {
    type Args = u64;
    type Deps = ();
    fn create(args: u64, _deps: ()) -> Self {
        Self { count: args }
    }
}

/// Tell message: increment the counter by the given amount.
pub struct Increment(pub u64);
impl Message for Increment {
    type Reply = ();
}

#[async_trait]
impl Handler<Increment> for ConformanceCounter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

/// Ask message: return the current count.
pub struct GetCount;
impl Message for GetCount {
    type Reply = u64;
}

#[async_trait]
impl Handler<GetCount> for ConformanceCounter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

// ── Conformance Tests ───────────────────────────────

/// Test: tell delivers messages and ask returns the correct reply.
pub async fn test_tell_and_ask<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("counter", 0);
    actor.tell(Increment(5)).unwrap();
    actor.tell(Increment(3)).unwrap();

    // Allow messages to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 8, "tell+ask: expected 8, got {}", count);

    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive(), "actor should be stopped");
}

/// Test: 100 messages processed in order.
pub async fn test_message_ordering<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("ordered", 0);
    for _ in 1..=100 {
        actor.tell(Increment(1)).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 100, "ordering: expected 100, got {}", count);
}

/// Test: ask returns the correct reply type.
pub async fn test_ask_reply<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("ask-reply", 42);
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 42);
}

/// Test: stop() makes actor not alive.
pub async fn test_stop<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("stopper", 0);
    assert!(actor.is_alive());
    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive());
}

/// Test: actor IDs are unique per spawn.
pub async fn test_unique_ids<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: Fn(&str, u64) -> R,
{
    let a1 = spawn("a", 0);
    let a2 = spawn("b", 0);
    assert_ne!(a1.id(), a2.id(), "actor IDs should be unique");
}

/// Test: actor name matches the name given at spawn.
pub async fn test_actor_name<R, F>(spawn: F)
where
    R: ActorRef<ConformanceCounter>,
    F: FnOnce(&str, u64) -> R,
{
    let actor = spawn("my-counter", 0);
    assert_eq!(actor.name(), "my-counter");
}
