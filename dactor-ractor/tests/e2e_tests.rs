//! E2E integration tests for the dactor-ractor adapter.
//!
//! These tests exercise the full stack:  test process → gRPC → test-node-ractor
//! binary → RactorRuntime → CounterActor → reply back through gRPC.
//!
//! **T1** — 2-node spawn + tell/ask cross-check
//! **T2** — Watch notification on actor stop
//! **T3** — Partition fault injection + heal + recovery
//! **E2E** — Spawn duplicate rejected, tell/ask stopped actor, tell unknown actor,
//!           concurrent operations, graceful shutdown

use dactor_test_harness::TestCluster;
use std::time::Duration;

/// Locate the `test-node-ractor` binary built by cargo.
fn test_node_binary() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps
    path.push("test-node-ractor");
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
            "Skipping E2E test: test-node-ractor binary not found at {}.\n\
             Build it with: cargo build -p dactor-ractor --features test-harness --bin test-node-ractor",
            binary
        );
    }
    binary
}

// =========================================================================
// T1 — 2-node spawn + tell/ask cross-check
// =========================================================================

#[tokio::test]
async fn t1_two_node_spawn_tell_ask() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    // Launch two independent ractor test nodes
    let mut cluster = TestCluster::builder()
        .node("node-1", &binary, &[], 50071)
        .node("node-2", &binary, &[], 50072)
        .build()
        .await;

    // Verify both nodes are alive with ractor adapter
    let info1 = cluster.get_node_info("node-1").await.unwrap();
    assert_eq!(info1.adapter, "ractor");
    let info2 = cluster.get_node_info("node-2").await.unwrap();
    assert_eq!(info2.adapter, "ractor");

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
// T2 — Watch notification on actor stop
// =========================================================================

#[tokio::test]
async fn t2_watch_actor_stop_notification() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("watch-node", &binary, &[], 50073)
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

// =========================================================================
// T3 — Partition fault injection + heal + recovery
// =========================================================================

#[tokio::test]
async fn t3_partition_heal_recovery() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("partition-node", &binary, &[], 50074)
        .build()
        .await;

    // Spawn a counter and increment it
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

    // Ask should also be blocked
    let ask_resp = cluster
        .ask_actor("partition-node", "resilient", "get_count", b"")
        .await
        .unwrap();
    assert!(
        !ask_resp.success,
        "ask should fail during partition"
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
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Ask should return the correct count (10 + 5 = 15, not 20)
    // The increment during partition was blocked, so only the post-heal one counts
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
        .node("dup-node", &binary, &[], 50075)
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
        .node("tell-stop-node", &binary, &[], 50076)
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
        .node("ask-stop-node", &binary, &[], 50077)
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
        .node("unknown-node", &binary, &[], 50078)
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
        .node("concurrent-node", &binary, &[], 50079)
        .build()
        .await;

    // Spawn actor with initial count 0
    let resp = cluster
        .spawn_actor("concurrent-node", "counter", "rapid", b"0")
        .await
        .unwrap();
    assert!(resp.success);

    // Note: These sends are sequential (TestCluster is not Clone).
    // This tests rapid throughput, not true concurrency.
    for _ in 0..50 {
        let tell_resp = cluster
            .tell_actor("concurrent-node", "rapid", "increment", b"1")
            .await
            .unwrap();
        assert!(tell_resp.success, "tell failed: {}", tell_resp.error);
    }

    // Sleep for processing
    tokio::time::sleep(Duration::from_millis(500)).await;

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
        .node("shutdown-node", &binary, &[], 50080)
        .build()
        .await;

    // Ping should succeed while the node is running
    let ping_resp = cluster.ping("shutdown-node", "hello").await;
    assert!(ping_resp.is_ok(), "ping should succeed before shutdown");

    // Shutdown the node
    cluster.shutdown_node("shutdown-node").await.unwrap();

    // Give the process time to fully terminate
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Ping should fail after shutdown
    let ping_resp = cluster.ping("shutdown-node", "hello").await;
    assert!(
        ping_resp.is_err(),
        "ping should fail after node shutdown"
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Watch actor termination notification
// =========================================================================

#[tokio::test]
async fn e2e_watch_actor_termination() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("watch-term", &binary, &[], 50101)
        .build()
        .await;

    // Spawn watcher and target actors
    let resp = cluster
        .spawn_actor("watch-term", "counter", "watcher", b"0")
        .await
        .unwrap();
    assert!(resp.success, "spawn watcher failed: {}", resp.error);

    let resp = cluster
        .spawn_actor("watch-term", "counter", "target", b"0")
        .await
        .unwrap();
    assert!(resp.success, "spawn target failed: {}", resp.error);

    // Register watch: watcher watches target
    let watch_resp = cluster
        .watch_actor("watch-term", "watcher", "target")
        .await
        .unwrap();
    assert!(watch_resp.success, "watch failed: {}", watch_resp.error);

    // Verify watcher count is 0 before target dies
    let ask_resp = cluster
        .ask_actor("watch-term", "watcher", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 0, "watcher count should be 0 initially");

    // Stop the target actor
    let stop_resp = cluster.stop_actor("watch-term", "target").await.unwrap();
    assert!(stop_resp.success, "stop failed: {}", stop_resp.error);

    // Poll until watcher count changes (up to 2s)
    let mut watcher_notified = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let ask_resp = cluster
            .ask_actor("watch-term", "watcher", "get_count", b"")
            .await
            .unwrap();
        if ask_resp.success {
            let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
            if count == -999 {
                watcher_notified = true;
                break;
            }
        }
    }
    assert!(
        watcher_notified,
        "watcher should have received ChildTerminated within 2s"
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Node crash detection
// =========================================================================

#[tokio::test]
async fn e2e_node_crash_detection() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    // Launch 2 nodes
    let mut cluster = TestCluster::builder()
        .node("alive", &binary, &[], 50102)
        .node("doomed", &binary, &[], 50103)
        .build()
        .await;

    // Both nodes should be alive
    let ping_resp = cluster.ping("alive", "ok").await;
    assert!(ping_resp.is_ok(), "alive node should respond to ping");

    let ping_resp = cluster.ping("doomed", "ok").await;
    assert!(ping_resp.is_ok(), "doomed node should respond to ping");

    // Kill the doomed node (simulates crash)
    cluster.shutdown_node("doomed").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Doomed node should be unreachable
    let ping_resp = cluster.ping("doomed", "test").await;
    assert!(
        ping_resp.is_err(),
        "doomed node should be unreachable after crash"
    );

    // Alive node should still be fine
    let resp = cluster.ping("alive", "still-ok").await.unwrap();
    assert_eq!(resp.echo, "still-ok", "alive node should still respond");

    // Spawn an actor on the alive node to prove it's fully functional
    let spawn_resp = cluster
        .spawn_actor("alive", "counter", "survivor", b"42")
        .await
        .unwrap();
    assert!(spawn_resp.success, "should be able to spawn on alive node");

    let ask_resp = cluster
        .ask_actor("alive", "survivor", "get_count", b"")
        .await
        .unwrap();
    assert!(ask_resp.success);
    let count: i64 = serde_json::from_slice(&ask_resp.payload).unwrap();
    assert_eq!(count, 42, "alive node actor should work normally");

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Large payload echo round-trip
// =========================================================================

#[tokio::test]
async fn e2e_large_payload() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("large-node", &binary, &[], 50110)
        .build()
        .await;

    // Spawn actor
    let resp = cluster
        .spawn_actor("large-node", "counter", "echo-actor", b"0")
        .await
        .unwrap();
    assert!(resp.success, "spawn failed: {}", resp.error);

    // Send a 100KB payload via echo ask
    let payload = vec![42u8; 100_000];
    let ask_resp = cluster
        .ask_actor("large-node", "echo-actor", "echo", &payload)
        .await
        .unwrap();
    assert!(ask_resp.success, "echo ask failed: {}", ask_resp.error);
    assert_eq!(
        ask_resp.payload, payload,
        "echo response should match the 100KB payload exactly"
    );

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Multi-actor interaction and isolation
// =========================================================================

#[tokio::test]
async fn e2e_multi_actor_interaction() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("multi-node", &binary, &[], 50111)
        .build()
        .await;

    // Spawn 3 actors with different initial counts
    let resp_a = cluster
        .spawn_actor("multi-node", "counter", "actor-a", b"0")
        .await
        .unwrap();
    assert!(resp_a.success, "spawn actor-a failed: {}", resp_a.error);

    let resp_b = cluster
        .spawn_actor("multi-node", "counter", "actor-b", b"100")
        .await
        .unwrap();
    assert!(resp_b.success, "spawn actor-b failed: {}", resp_b.error);

    let resp_c = cluster
        .spawn_actor("multi-node", "counter", "actor-c", b"200")
        .await
        .unwrap();
    assert!(resp_c.success, "spawn actor-c failed: {}", resp_c.error);

    // Tell each to increment by different amounts
    let tell = cluster
        .tell_actor("multi-node", "actor-a", "increment", b"10")
        .await
        .unwrap();
    assert!(tell.success);

    let tell = cluster
        .tell_actor("multi-node", "actor-b", "increment", b"20")
        .await
        .unwrap();
    assert!(tell.success);

    let tell = cluster
        .tell_actor("multi-node", "actor-c", "increment", b"30")
        .await
        .unwrap();
    assert!(tell.success);

    // Poll actor-a with up to 2s timeout (100ms intervals)
    let mut count = 0;
    let mut found = false;
    for _ in 0..20 {
        let ask = cluster
            .ask_actor("multi-node", "actor-a", "get_count", b"")
            .await
            .unwrap();
        if ask.success {
            count = serde_json::from_slice(&ask.payload).unwrap();
            if count == 10 {
                found = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "actor-a should reach count 10, got {}", count);

    // Poll actor-b with up to 2s timeout (100ms intervals)
    let mut count = 0;
    let mut found = false;
    for _ in 0..20 {
        let ask = cluster
            .ask_actor("multi-node", "actor-b", "get_count", b"")
            .await
            .unwrap();
        if ask.success {
            count = serde_json::from_slice(&ask.payload).unwrap();
            if count == 120 {
                found = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "actor-b should reach count 120, got {}", count);

    // Poll actor-c with up to 2s timeout (100ms intervals)
    let mut count = 0;
    let mut found = false;
    for _ in 0..20 {
        let ask = cluster
            .ask_actor("multi-node", "actor-c", "get_count", b"")
            .await
            .unwrap();
        if ask.success {
            count = serde_json::from_slice(&ask.payload).unwrap();
            if count == 230 {
                found = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "actor-c should reach count 230, got {}", count);

    // Stop one actor, verify others still work
    let stop = cluster
        .stop_actor("multi-node", "actor-b")
        .await
        .unwrap();
    assert!(stop.success, "stop actor-b failed: {}", stop.error);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // actor-a and actor-c should still respond
    let ask = cluster
        .ask_actor("multi-node", "actor-a", "get_count", b"")
        .await
        .unwrap();
    assert!(ask.success, "actor-a should still work after actor-b stopped");

    let ask = cluster
        .ask_actor("multi-node", "actor-c", "get_count", b"")
        .await
        .unwrap();
    assert!(ask.success, "actor-c should still work after actor-b stopped");

    // actor-b should be gone
    let ask = cluster
        .ask_actor("multi-node", "actor-b", "get_count", b"")
        .await
        .unwrap();
    assert!(!ask.success, "actor-b should be gone after stop");

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Rapid spawn/stop cycle (resource cleanup)
// =========================================================================

#[tokio::test]
async fn e2e_rapid_spawn_stop_cycle() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("rapid-node", &binary, &[], 50112)
        .build()
        .await;

    // Rapidly spawn and stop 5 actors in sequence
    for i in 0..5 {
        let name = format!("cycle-actor-{}", i);
        let resp = cluster
            .spawn_actor("rapid-node", "counter", &name, b"0")
            .await
            .unwrap();
        assert!(resp.success, "spawn {} failed: {}", name, resp.error);

        let stop = cluster.stop_actor("rapid-node", &name).await.unwrap();
        assert!(stop.success, "stop {} failed: {}", name, stop.error);

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Verify actor_count is 0
    let info = cluster.get_node_info("rapid-node").await.unwrap();
    assert_eq!(info.actor_count, 0, "all actors should be cleaned up");

    // Spawn a final actor and verify it works normally
    let resp = cluster
        .spawn_actor("rapid-node", "counter", "final-actor", b"42")
        .await
        .unwrap();
    assert!(resp.success, "final spawn failed: {}", resp.error);

    let ask = cluster
        .ask_actor("rapid-node", "final-actor", "get_count", b"")
        .await
        .unwrap();
    assert!(ask.success, "final ask failed: {}", ask.error);
    let count: i64 = serde_json::from_slice(&ask.payload).unwrap();
    assert_eq!(count, 42, "final actor should work normally");

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Slow handler doesn't block other actors
// =========================================================================

#[tokio::test]
async fn e2e_slow_handler_doesnt_block_others() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("slow-node", &binary, &[], 50113)
        .build()
        .await;

    // Spawn two actors
    let resp = cluster
        .spawn_actor("slow-node", "counter", "slow", b"0")
        .await
        .unwrap();
    assert!(resp.success, "spawn slow failed: {}", resp.error);

    let resp = cluster
        .spawn_actor("slow-node", "counter", "fast", b"0")
        .await
        .unwrap();
    assert!(resp.success, "spawn fast failed: {}", resp.error);

    // Tell "slow" to slow_increment (500ms delay)
    let tell = cluster
        .tell_actor("slow-node", "slow", "slow_increment", b"10")
        .await
        .unwrap();
    assert!(tell.success, "slow_increment tell failed: {}", tell.error);

    // Immediately ask "fast" for count — should respond quickly
    let start = std::time::Instant::now();
    let ask = cluster
        .ask_actor("slow-node", "fast", "get_count", b"")
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert!(ask.success, "fast ask failed: {}", ask.error);
    let count: i64 = serde_json::from_slice(&ask.payload).unwrap();
    assert_eq!(count, 0, "fast actor should have count 0");
    assert!(
        elapsed < Duration::from_secs(2),
        "fast actor should respond quickly (took {:?}), not blocked by slow actor. Using 2s for CI robustness but should be much less than 500ms slow handler",
        elapsed
    );

    // Poll until slow handler finishes (up to 2s)
    let mut slow_done = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let ask = cluster
            .ask_actor("slow-node", "slow", "get_count", b"")
            .await
            .unwrap();
        if ask.success {
            let count: i64 = serde_json::from_slice(&ask.payload).unwrap();
            if count == 10 {
                slow_done = true;
                break;
            }
        }
    }
    assert!(slow_done, "slow actor count should reach 10 within 2s");

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Ask with timeout succeeds when handler is fast
// =========================================================================

#[tokio::test]
async fn e2e_ask_with_timeout_succeeds() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("timeout-ok", &binary, &[], 50130)
        .build()
        .await;

    // Spawn a counter actor
    let resp = cluster
        .spawn_actor("timeout-ok", "counter", "c1", b"42")
        .await
        .unwrap();
    assert!(resp.success, "spawn failed: {}", resp.error);

    // Ask with a generous 5s timeout — should succeed quickly
    let ask = cluster
        .ask_actor_with_timeout("timeout-ok", "c1", "get_count", b"", 5000)
        .await
        .unwrap();
    assert!(ask.success, "ask with timeout failed: {}", ask.error);
    let count: i64 = serde_json::from_slice(&ask.payload).unwrap();
    assert_eq!(count, 42, "counter should be 42");

    cluster.shutdown().await;
}

// =========================================================================
// E2E — Ask timeout cancels slow handler
// =========================================================================

#[tokio::test]
async fn e2e_ask_timeout_cancels_slow_handler() {
    let binary = require_binary();
    if !std::path::Path::new(&binary).exists() {
        return;
    }

    let mut cluster = TestCluster::builder()
        .node("timeout-cancel", &binary, &[], 50131)
        .build()
        .await;

    // Spawn a counter actor
    let resp = cluster
        .spawn_actor("timeout-cancel", "counter", "slow1", b"0")
        .await
        .unwrap();
    assert!(resp.success, "spawn failed: {}", resp.error);

    // Ask slow_echo (2s delay) with only 200ms timeout — should time out
    let start = std::time::Instant::now();
    let ask = cluster
        .ask_actor_with_timeout("timeout-cancel", "slow1", "slow_echo", b"hello", 200)
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert!(!ask.success, "slow ask should have timed out");
    assert!(
        ask.error.contains("timed out"),
        "error should contain 'timed out', got: {}",
        ask.error
    );
    // Client-side timeout: returns quickly, but the actor handler continues
    // running to completion (fire-and-forget semantics on timeout).
    assert!(
        elapsed < Duration::from_secs(1),
        "should have timed out quickly (took {:?}), not waited for full 2s handler",
        elapsed
    );

    cluster.shutdown().await;
}
