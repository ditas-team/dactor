//! E2E integration tests for the dactor-kameo adapter.
//!
//! These tests exercise the full stack:  test process → gRPC → test-node-kameo
//! binary → KameoRuntime → CounterActor → reply back through gRPC.
//!
//! **T4** — 2-node spawn + tell/ask cross-check
//! **T5** — Watch notification on actor stop

use dactor_test_harness::TestCluster;
use std::time::Duration;

/// Locate the `test-node-kameo` binary built by cargo.
fn test_node_binary() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps
    path.push("test-node-kameo");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path.to_string_lossy().to_string()
}

/// Skip gracefully when the binary hasn't been built.
fn require_binary() -> String {
    let binary = test_node_binary();
    if !std::path::Path::new(&binary).exists() {
        eprintln!(
            "Skipping E2E test: test-node-kameo binary not found at {}.\n\
             Build it with: cargo build -p dactor-kameo --features test-harness --bin test-node-kameo",
            binary
        );
    }
    binary
}

// =========================================================================
// T4 — 2-node spawn + tell/ask cross-check
// =========================================================================

#[tokio::test]
async fn t4_kameo_two_node_spawn_tell_ask() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    // Launch two independent kameo test nodes
    let mut cluster = TestCluster::builder()
        .node("node-1", &binary, &[], 50081)
        .node("node-2", &binary, &[], 50082)
        .build()
        .await;

    // Verify both nodes are alive with kameo adapter
    let info1 = cluster.get_node_info("node-1").await.unwrap();
    assert_eq!(info1.adapter, "kameo");
    let info2 = cluster.get_node_info("node-2").await.unwrap();
    assert_eq!(info2.adapter, "kameo");

    // Spawn a counter actor on each node
    let resp1 = cluster
        .spawn_actor("node-1", "counter", "counter-a", b"0")
        .await
        .unwrap();
    assert!(resp1.success, "spawn on node-1 failed: {}", resp1.error);

    let resp2 = cluster
        .spawn_actor("node-2", "counter", "counter-b", b"100")
        .await
        .unwrap();
    assert!(resp2.success, "spawn on node-2 failed: {}", resp2.error);

    // Tell node-1's counter to increment 3 times
    for _ in 0..3 {
        let tell_resp = cluster
            .tell_actor("node-1", "counter-a", "increment", b"1")
            .await
            .unwrap();
        assert!(tell_resp.success, "tell failed: {}", tell_resp.error);
    }

    // Ask node-1's counter for its count — expect 3
    let ask_resp = cluster
        .ask_actor("node-1", "counter-a", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success, "ask failed: {}", ask_resp.error);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 3, "node-1 counter should be 3");

    // Tell node-2's counter to increment by 5
    let tell_resp = cluster
        .tell_actor("node-2", "counter-b", "increment", b"5")
        .await
        .unwrap();
    assert!(tell_resp.success);

    // Ask node-2's counter — expect 105 (started at 100 + 5)
    let ask_resp = cluster
        .ask_actor("node-2", "counter-b", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 105, "node-2 counter should be 105");

    // Cross-check: node-1's counter is independent from node-2's
    let ask_resp = cluster
        .ask_actor("node-1", "counter-a", "get_count", b"")
        .await
        .unwrap();
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 3, "node-1 counter unchanged");

    // Verify actor counts via node info
    let info1 = cluster.get_node_info("node-1").await.unwrap();
    assert_eq!(info1.actor_count, 1);
    let info2 = cluster.get_node_info("node-2").await.unwrap();
    assert_eq!(info2.actor_count, 1);

    cluster.shutdown().await;
}

// =========================================================================
// T5 — Watch notification on actor stop
// =========================================================================

#[tokio::test]
async fn t5_kameo_stop_notification() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("watch-node", &binary, &[], 50083)
        .build()
        .await;

    // Subscribe to events so we can observe actor_stopped
    let mut events = cluster
        .subscribe_events("watch-node", &["actor_stopped"])
        .await
        .unwrap();

    // Spawn the target actor
    let resp = cluster
        .spawn_actor("watch-node", "counter", "target", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    // Verify it's alive
    let ask_resp = cluster
        .ask_actor("watch-node", "target", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 0);

    // Stop the target actor
    let stop_resp = cluster
        .stop_actor("watch-node", "target")
        .await
        .unwrap();
    assert!(stop_resp.success, "stop failed: {}", stop_resp.error);

    // Verify we get the actor_stopped event
    let event = events.next_event(Duration::from_secs(5)).await;
    assert!(event.is_some(), "expected actor_stopped event");
    let event = event.unwrap();
    assert_eq!(event.event_type, "actor_stopped");
    assert!(
        event.detail.contains("target"),
        "event detail should mention the actor name"
    );

    // Verify the actor is gone — ask should fail
    let ask_resp = cluster
        .ask_actor("watch-node", "target", "get_count", b"")
        .await
        .unwrap();
    assert!(
        !ask_resp.success,
        "actor should be gone after stop"
    );

    // Verify actor count dropped
    let info = cluster.get_node_info("watch-node").await.unwrap();
    assert_eq!(info.actor_count, 0, "no actors should remain");

    cluster.shutdown().await;
}
