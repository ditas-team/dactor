//! E2E integration tests for the dactor-coerce adapter.
//!
//! These tests exercise the full stack:  test process → gRPC → test-node-coerce
//! binary → CoerceRuntime → CounterActor → reply back through gRPC.
//!
//! **T6** — 2-node spawn + tell/ask cross-check

use dactor_test_harness::TestCluster;

/// Locate the `test-node-coerce` binary built by cargo.
fn test_node_binary() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps
    path.push("test-node-coerce");
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
            "Skipping E2E test: test-node-coerce binary not found at {}.\n\
             Build it with: cargo build -p dactor-coerce --features test-harness --bin test-node-coerce",
            binary
        );
    }
    binary
}

// =========================================================================
// T6 — 2-node spawn + tell/ask cross-check
// =========================================================================

#[tokio::test]
async fn t6_coerce_two_node_spawn_tell_ask() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    // Launch two independent coerce test nodes
    let mut cluster = TestCluster::builder()
        .node("node-1", &binary, &[], 50091)
        .node("node-2", &binary, &[], 50092)
        .build()
        .await;

    // Verify both nodes are alive with coerce adapter
    let info1 = cluster.get_node_info("node-1").await.unwrap();
    assert_eq!(info1.adapter, "coerce");
    let info2 = cluster.get_node_info("node-2").await.unwrap();
    assert_eq!(info2.adapter, "coerce");

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
// T6b — stop actor notification (coerce)
// =========================================================================

#[tokio::test]
async fn t6b_coerce_stop_notification() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("stop-node", &binary, &[], 50093)
        .build()
        .await;

    // Subscribe to events before spawning
    let mut events = cluster
        .subscribe_events("stop-node", &["actor_stopped"])
        .await
        .unwrap();

    // Spawn a counter actor
    let resp = cluster
        .spawn_actor("stop-node", "counter", "doomed", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    // Verify actor is alive
    let info = cluster.get_node_info("stop-node").await.unwrap();
    assert_eq!(info.actor_count, 1);

    // Stop the actor
    let stop_resp = cluster.stop_actor("stop-node", "doomed").await.unwrap();
    assert!(stop_resp.success, "stop failed: {}", stop_resp.error);

    // Verify actor_stopped event
    let event = events
        .next_event(std::time::Duration::from_secs(5))
        .await;
    assert!(event.is_some(), "expected actor_stopped event");
    assert_eq!(event.unwrap().event_type, "actor_stopped");

    // Verify actor count dropped
    let info = cluster.get_node_info("stop-node").await.unwrap();
    assert_eq!(info.actor_count, 0);

    cluster.shutdown().await;
}
