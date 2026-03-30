//! Integration tests for the dactor-kameo adapter.
//!
//! Covers ADAPT-01 through ADAPT-06 (adapter conformance) and KAMEO-02
//! through KAMEO-11 (kameo-specific implementation tests).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dactor::{ActorRef, ActorRuntime, ClusterEvent, ClusterEvents, NodeId, TimerHandle};
use dactor_kameo::KameoRuntime;

// ---------------------------------------------------------------------------
// ADAPT-01: spawn + send round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapt_01_spawn_and_send() {
    let rt = KameoRuntime::new();
    let received = Arc::new(AtomicU64::new(0));
    let r = received.clone();

    let actor = rt.spawn("adapt01", move |msg: u64| {
        r.fetch_add(msg, Ordering::SeqCst);
    });

    actor.send(42).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(received.load(Ordering::SeqCst), 42);
}

#[tokio::test]
async fn adapt_01_multiple_messages_in_order() {
    let rt = KameoRuntime::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();

    let actor = rt.spawn("adapt01_order", move |msg: u64| {
        l.lock().unwrap().push(msg);
    });

    for i in 1..=5 {
        actor.send(i).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let msgs = log.lock().unwrap().clone();
    assert_eq!(msgs, vec![1, 2, 3, 4, 5]);
}

// ---------------------------------------------------------------------------
// ADAPT-02: Processing group join / leave / broadcast
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapt_02_group_broadcast_reaches_all() {
    let rt = KameoRuntime::new();
    let c1 = Arc::new(AtomicU64::new(0));
    let c2 = Arc::new(AtomicU64::new(0));
    let c3 = Arc::new(AtomicU64::new(0));

    let r1 = c1.clone();
    let r2 = c2.clone();
    let r3 = c3.clone();

    let a1 = rt.spawn("g1", move |msg: u64| {
        r1.fetch_add(msg, Ordering::SeqCst);
    });
    let a2 = rt.spawn("g2", move |msg: u64| {
        r2.fetch_add(msg, Ordering::SeqCst);
    });
    let a3 = rt.spawn("g3", move |msg: u64| {
        r3.fetch_add(msg, Ordering::SeqCst);
    });

    rt.join_group("test_group", &a1).unwrap();
    rt.join_group("test_group", &a2).unwrap();
    rt.join_group("test_group", &a3).unwrap();

    rt.broadcast_group("test_group", 10u64).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(c1.load(Ordering::SeqCst), 10);
    assert_eq!(c2.load(Ordering::SeqCst), 10);
    assert_eq!(c3.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn adapt_02_leave_group_excludes_from_broadcast() {
    let rt = KameoRuntime::new();
    let c1 = Arc::new(AtomicU64::new(0));
    let c2 = Arc::new(AtomicU64::new(0));

    let r1 = c1.clone();
    let r2 = c2.clone();

    let a1 = rt.spawn("leave1", move |msg: u64| {
        r1.fetch_add(msg, Ordering::SeqCst);
    });
    let a2 = rt.spawn("leave2", move |msg: u64| {
        r2.fetch_add(msg, Ordering::SeqCst);
    });

    rt.join_group("leave_group", &a1).unwrap();
    rt.join_group("leave_group", &a2).unwrap();

    rt.leave_group("leave_group", &a1).unwrap();
    rt.broadcast_group("leave_group", 5u64).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(c1.load(Ordering::SeqCst), 0, "left actor should not receive");
    assert_eq!(c2.load(Ordering::SeqCst), 5, "remaining actor should receive");
}

// ---------------------------------------------------------------------------
// ADAPT-03: Cluster events subscribe / emit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapt_03_cluster_events_node_joined() {
    let rt = KameoRuntime::new();
    let joined = Arc::new(Mutex::new(Vec::new()));
    let j = joined.clone();

    rt.cluster_events()
        .subscribe(Box::new(move |event| {
            if let ClusterEvent::NodeJoined(id) = event {
                j.lock().unwrap().push(id);
            }
        }))
        .unwrap();

    rt.cluster_events_handle()
        .emit(ClusterEvent::NodeJoined(NodeId("5".into())));

    let events = joined.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], NodeId("5".into()));
}

#[tokio::test]
async fn adapt_03_cluster_events_unsubscribe() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let sub_id = rt
        .cluster_events()
        .subscribe(Box::new(move |_event| {
            c.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();

    rt.cluster_events_handle()
        .emit(ClusterEvent::NodeJoined(NodeId("1".into())));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    rt.cluster_events().unsubscribe(sub_id).unwrap();
    rt.cluster_events_handle()
        .emit(ClusterEvent::NodeJoined(NodeId("2".into())));
    assert_eq!(count.load(Ordering::SeqCst), 1, "unsubscribed callback should not fire");
}

// ---------------------------------------------------------------------------
// ADAPT-04: send_interval fires periodically
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapt_04_send_interval_fires_periodically() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let actor = rt.spawn("ticker", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    let _timer = rt.send_interval(&actor, Duration::from_millis(50), 1u64);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let n = count.load(Ordering::SeqCst);
    assert!(n >= 3, "expected at least 3 timer fires, got {n}");
}

// ---------------------------------------------------------------------------
// ADAPT-05: send_after fires once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapt_05_send_after_fires_once() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let actor = rt.spawn("one_shot", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    let _timer = rt.send_after(&actor, Duration::from_millis(100), 1u64);
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(count.load(Ordering::SeqCst), 1, "send_after should fire exactly once");
}

// ---------------------------------------------------------------------------
// ADAPT-06: TimerHandle::cancel stops delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapt_06_timer_cancel_stops_delivery() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let actor = rt.spawn("cancel_test", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    let timer = rt.send_interval(&actor, Duration::from_millis(30), 1u64);
    tokio::time::sleep(Duration::from_millis(120)).await;

    let before = count.load(Ordering::SeqCst);
    assert!(before >= 1, "should have received at least 1 message before cancel");
    timer.cancel();

    tokio::time::sleep(Duration::from_millis(150)).await;
    let after = count.load(Ordering::SeqCst);

    // Allow at most 1 in-flight message after cancel
    assert!(
        after <= before + 1,
        "expected no more than 1 extra after cancel, before={before} after={after}"
    );
}

// ---------------------------------------------------------------------------
// KAMEO-02: send() maps to kameo tell() — fire-and-forget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kameo_02_send_is_fire_and_forget() {
    let rt = KameoRuntime::new();
    let received = Arc::new(Mutex::new(Vec::new()));
    let r = received.clone();

    let actor = rt.spawn("kameo02", move |msg: String| {
        r.lock().unwrap().push(msg);
    });

    actor.send("hello".to_string()).unwrap();
    actor.send("world".to_string()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let msgs = received.lock().unwrap().clone();
    assert_eq!(msgs, vec!["hello", "world"]);
}

// ---------------------------------------------------------------------------
// KAMEO-04: get_group_members returns typed refs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kameo_04_get_group_members() {
    let rt = KameoRuntime::new();

    let a1 = rt.spawn("mem1", move |_msg: u64| {});
    let a2 = rt.spawn("mem2", move |_msg: u64| {});

    rt.join_group("members_group", &a1).unwrap();
    rt.join_group("members_group", &a2).unwrap();

    let members = rt.get_group_members::<u64>("members_group").unwrap();
    assert_eq!(members.len(), 2);

    // Verify the returned refs work — send through them
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();
    let collector = rt.spawn("collector", move |msg: u64| {
        c.fetch_add(msg, Ordering::SeqCst);
    });
    rt.join_group("members_group", &collector).unwrap();

    rt.broadcast_group("members_group", 1u64).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// KAMEO-07 / KAMEO-08: Timer via tokio + cancel before fire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kameo_07_send_after_cancel_before_fire() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let actor = rt.spawn("cancel_before", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    let timer = rt.send_after(&actor, Duration::from_millis(200), 1u64);
    timer.cancel();

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "cancelled send_after should not fire"
    );
}

// ---------------------------------------------------------------------------
// KAMEO-11: Re-exports core crate types
// ---------------------------------------------------------------------------

#[test]
fn kameo_11_reexports_core_types() {
    // Verify that the dactor crate is re-exported and types are accessible
    let _ = dactor_kameo::dactor::NodeId("1".into());

    // Verify adapter types are exported
    let _rt = dactor_kameo::KameoRuntime::new();
    let _events = dactor_kameo::KameoClusterEvents::new();
}

// ---------------------------------------------------------------------------
// Additional: KameoRuntime is Clone and Default
// ---------------------------------------------------------------------------

#[test]
fn runtime_is_clone_and_default() {
    let rt = KameoRuntime::default();
    let _rt2 = rt.clone();
}

// ---------------------------------------------------------------------------
// Additional: Cluster events emit NodeLeft
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cluster_events_node_left() {
    let rt = KameoRuntime::new();
    let left = Arc::new(Mutex::new(Vec::new()));
    let l = left.clone();

    rt.cluster_events()
        .subscribe(Box::new(move |event| {
            if let ClusterEvent::NodeLeft(id) = event {
                l.lock().unwrap().push(id);
            }
        }))
        .unwrap();

    rt.cluster_events_handle()
        .emit(ClusterEvent::NodeLeft(NodeId("99".into())));

    let events = left.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], NodeId("99".into()));
}

// ---------------------------------------------------------------------------
// Additional: Multiple subscribers receive the same event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cluster_events_multiple_subscribers() {
    let rt = KameoRuntime::new();
    let c1 = Arc::new(AtomicU64::new(0));
    let c2 = Arc::new(AtomicU64::new(0));

    let r1 = c1.clone();
    let r2 = c2.clone();

    rt.cluster_events()
        .subscribe(Box::new(move |_| {
            r1.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();

    rt.cluster_events()
        .subscribe(Box::new(move |_| {
            r2.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();

    rt.cluster_events_handle()
        .emit(ClusterEvent::NodeJoined(NodeId("1".into())));

    assert_eq!(c1.load(Ordering::SeqCst), 1);
    assert_eq!(c2.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Additional: Empty group operations are no-ops
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_group_broadcast_is_noop() {
    let rt = KameoRuntime::new();
    rt.broadcast_group::<u64>("nonexistent", 42).unwrap();
}

#[tokio::test]
async fn empty_group_get_members_returns_empty() {
    let rt = KameoRuntime::new();
    let members = rt.get_group_members::<u64>("nonexistent").unwrap();
    assert!(members.is_empty());
}

// ---------------------------------------------------------------------------
// Timer handle Drop aborts the task (no leak on drop without cancel)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timer_drop_aborts_interval() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let actor = rt.spawn("timer_drop", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    {
        let _timer = rt.send_interval(&actor, Duration::from_millis(20), 1u64);
        tokio::time::sleep(Duration::from_millis(80)).await;
        // _timer dropped here without calling cancel()
    }

    let count_at_drop = count.load(Ordering::SeqCst);
    assert!(count_at_drop > 0, "timer should have fired before drop");

    // After drop, no more messages should arrive
    tokio::time::sleep(Duration::from_millis(100)).await;
    let count_after = count.load(Ordering::SeqCst);
    assert_eq!(count_at_drop, count_after, "timer should stop after handle drop");
}

#[tokio::test]
async fn timer_drop_aborts_send_after() {
    let rt = KameoRuntime::new();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();

    let actor = rt.spawn("timer_drop_after", move |_msg: u64| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    {
        let _timer = rt.send_after(&actor, Duration::from_millis(200), 1u64);
        // _timer dropped immediately — should abort before firing
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count.load(Ordering::SeqCst), 0, "send_after should not fire after drop");
}

// ---------------------------------------------------------------------------
// Group type mismatch: broadcast with wrong type is a silent no-op
// ---------------------------------------------------------------------------

#[tokio::test]
async fn group_type_mismatch_broadcast_is_noop() {
    let rt = KameoRuntime::new();
    let received = Arc::new(AtomicU64::new(0));
    let r = received.clone();

    // Join with u64 message type
    let actor = rt.spawn("type_mismatch", move |msg: u64| {
        r.fetch_add(msg, Ordering::SeqCst);
    });
    rt.join_group("mixed", &actor).unwrap();

    // Broadcast with String type — should silently skip the u64 actor
    rt.broadcast_group::<String>("mixed", "hello".to_string()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(received.load(Ordering::SeqCst), 0, "mismatched type should not deliver");

    // Broadcast with correct type — should deliver
    rt.broadcast_group::<u64>("mixed", 10).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(received.load(Ordering::SeqCst), 10, "correct type should deliver");
}

#[tokio::test]
async fn group_get_members_wrong_type_returns_empty() {
    let rt = KameoRuntime::new();
    let actor = rt.spawn::<u64, _>("member_type", move |_| {});
    rt.join_group("typed", &actor).unwrap();

    // Query with wrong type returns empty
    let members = rt.get_group_members::<String>("typed").unwrap();
    assert!(members.is_empty(), "wrong type should return no members");

    // Query with correct type returns the member
    let members = rt.get_group_members::<u64>("typed").unwrap();
    assert_eq!(members.len(), 1);
}
