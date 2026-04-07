//! E2E integration tests for the dactor-coerce adapter.
//!
//! These tests exercise the full stack:  test process → gRPC → test-node-coerce
//! binary → CoerceRuntime → CounterActor → reply back through gRPC.
//!
//! **T6** — 2-node spawn + tell/ask cross-check
//! **T6b** — Stop actor notification
//! **E2E** — Partition + heal + recovery, spawn duplicate rejected, tell/ask stopped actor,
//!           tell unknown actor, concurrent operations, graceful shutdown

use dactor_test_harness::TestCluster;
use std::time::Duration;

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

// =========================================================================
// E2E — Partition fault injection + heal + recovery
// =========================================================================

#[tokio::test]
async fn e2e_partition_heal_recovery() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("partition-node", &binary, &[], 50094)
        .build()
        .await;

    // Spawn a counter and increment it to 10
    let resp = cluster
        .spawn_actor("partition-node", "counter", "resilient", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    let tell_resp = cluster
        .tell_actor("partition-node", "resilient", "increment", b"10")
        .await
        .unwrap();
    assert!(tell_resp.success);

    // Verify count is 10
    let ask_resp = cluster
        .ask_actor("partition-node", "resilient", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 10);

    // Inject a partition fault targeting the "resilient" actor
    cluster
        .inject_fault("partition-node", "partition", "resilient", 0, 0)
        .await
        .unwrap();

    // Tell should be blocked by the partition
    let tell_resp = cluster
        .tell_actor("partition-node", "resilient", "increment", b"5")
        .await
        .unwrap();
    assert!(
        !tell_resp.success,
        "tell should fail during partition"
    );
    assert!(
        tell_resp.error.contains("partition"),
        "error should mention partition"
    );

    // Heal: clear all faults
    cluster.clear_faults("partition-node").await.unwrap();

    // After healing, tell should work again
    let tell_resp = cluster
        .tell_actor("partition-node", "resilient", "increment", b"5")
        .await
        .unwrap();
    assert!(
        tell_resp.success,
        "tell should succeed after healing: {}",
        tell_resp.error
    );

    // Give the actor a moment to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Ask should return the correct count (10 + 5 = 15, not 20)
    let ask_resp = cluster
        .ask_actor("partition-node", "resilient", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success, "ask should succeed: {}", ask_resp.error);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(
        count, 15,
        "count should be 15 (10 pre-partition + 5 post-heal)"
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Spawn duplicate rejected
// =========================================================================

#[tokio::test]
async fn e2e_spawn_duplicate_rejected() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("dup-node", &binary, &[], 50095)
        .build()
        .await;

    // First spawn should succeed
    let resp = cluster
        .spawn_actor("dup-node", "counter", "dup-actor", b"0")
        .await
        .unwrap();
    assert!(resp.success, "first spawn should succeed");

    // Second spawn with same name should fail
    let resp2 = cluster
        .spawn_actor("dup-node", "counter", "dup-actor", b"0")
        .await
        .unwrap();
    assert!(!resp2.success, "duplicate spawn should fail");
    assert!(
        resp2.error.to_lowercase().contains("already"),
        "error should mention 'already', got: {}",
        resp2.error
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Tell to stopped actor
// =========================================================================

#[tokio::test]
async fn e2e_tell_stopped_actor() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("tell-stop-node", &binary, &[], 50096)
        .build()
        .await;

    // Subscribe to events so we can observe actor_stopped
    let mut events = cluster
        .subscribe_events("tell-stop-node", &["actor_stopped"])
        .await
        .unwrap();

    // Spawn and then stop actor
    let resp = cluster
        .spawn_actor("tell-stop-node", "counter", "stopped-tell", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    let stop_resp = cluster
        .stop_actor("tell-stop-node", "stopped-tell")
        .await
        .unwrap();
    assert!(stop_resp.success);

    // Wait for stop event
    let event = events.next_event(Duration::from_secs(5)).await;
    assert!(event.is_some(), "expected actor_stopped event");

    // Tell to stopped actor should fail
    let tell_resp = cluster
        .tell_actor("tell-stop-node", "stopped-tell", "increment", b"1")
        .await
        .unwrap();
    assert!(
        !tell_resp.success,
        "tell to stopped actor should fail"
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Ask stopped actor
// =========================================================================

#[tokio::test]
async fn e2e_ask_stopped_actor() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("ask-stop-node", &binary, &[], 50097)
        .build()
        .await;

    // Subscribe to events so we can observe actor_stopped
    let mut events = cluster
        .subscribe_events("ask-stop-node", &["actor_stopped"])
        .await
        .unwrap();

    // Spawn and then stop actor
    let resp = cluster
        .spawn_actor("ask-stop-node", "counter", "stopped-ask", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    let stop_resp = cluster
        .stop_actor("ask-stop-node", "stopped-ask")
        .await
        .unwrap();
    assert!(stop_resp.success);

    // Wait for stop event
    let event = events.next_event(Duration::from_secs(5)).await;
    assert!(event.is_some(), "expected actor_stopped event");

    // Ask stopped actor should fail
    let ask_resp = cluster
        .ask_actor("ask-stop-node", "stopped-ask", "get_count", b"")
        .await
        .unwrap();
    assert!(
        !ask_resp.success,
        "ask to stopped actor should fail"
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Tell unknown actor
// =========================================================================

#[tokio::test]
async fn e2e_tell_unknown_actor() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("unknown-node", &binary, &[], 50098)
        .build()
        .await;

    // Tell to a nonexistent actor — should fail
    let tell_resp = cluster
        .tell_actor("unknown-node", "nonexistent-actor", "increment", b"1")
        .await
        .unwrap();
    assert!(
        !tell_resp.success,
        "tell to unknown actor should fail"
    );
    assert!(
        tell_resp.error.to_lowercase().contains("not found"),
        "error should mention 'not found', got: {}",
        tell_resp.error
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Concurrent tell/ask operations
// =========================================================================

#[tokio::test]
async fn e2e_concurrent_operations() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("concurrent-node", &binary, &[], 50099)
        .build()
        .await;

    // Spawn actor with initial count 0
    let resp = cluster
        .spawn_actor("concurrent-node", "counter", "rapid", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    // Send 50 tell increments of 1 rapidly
    for _ in 0..50 {
        let tell_resp = cluster
            .tell_actor("concurrent-node", "rapid", "increment", b"1")
            .await
            .unwrap();
        assert!(tell_resp.success, "tell failed: {}", tell_resp.error);
    }

    // Sleep for processing
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Ask count — should be 50
    let ask_resp = cluster
        .ask_actor("concurrent-node", "rapid", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success, "ask failed: {}", ask_resp.error);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 50, "count should be 50 after 50 increments");

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Graceful node shutdown
// =========================================================================

#[tokio::test]
async fn e2e_graceful_shutdown() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("shutdown-node", &binary, &[], 50100)
        .build()
        .await;

    // Ping should succeed while the node is running
    let ping_resp = cluster.ping("shutdown-node", "hello").await;
    assert!(ping_resp.is_ok(), "ping should succeed before shutdown");

    // Shutdown the node
    cluster.shutdown_node("shutdown-node").await.unwrap();

    // Give the process time to fully terminate
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Ping should fail after shutdown
    let ping_resp = cluster.ping("shutdown-node", "hello").await;
    assert!(
        ping_resp.is_err(),
        "ping should fail after node shutdown"
    );

    cluster.shutdown().await;
}
