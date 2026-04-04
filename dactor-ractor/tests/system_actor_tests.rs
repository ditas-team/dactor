//! Tests for SA1-SA4: system actor wiring in the ractor adapter.
//!
//! Validates that `RactorRuntime` correctly integrates SpawnManager,
//! WatchManager, CancelManager, and NodeDirectory for remote operations.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::{
    CancelResponse, PeerStatus, SpawnRequest, SpawnResponse,
};
use dactor_ractor::RactorRuntime;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// SA1: SpawnManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa1_runtime_has_spawn_manager() {
    let runtime = RactorRuntime::new();
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
}

#[test]
fn sa1_register_factory_and_create() {
    let mut runtime = RactorRuntime::new();
    runtime.register_factory("test::Counter", |bytes: &[u8]| {
        let value: i32 = serde_json::from_slice(bytes)
            .map_err(|e| dactor::remote::SerializationError::new(e.to_string()))?;
        Ok(Box::new(value))
    });

    let request = SpawnRequest {
        type_name: "test::Counter".into(),
        args_bytes: serde_json::to_vec(&42i32).unwrap(),
        name: "counter-1".into(),
        request_id: "req-1".into(),
    };

    let response = runtime.handle_spawn_request(&request);
    match response {
        SpawnResponse::Success { request_id, actor_id } => {
            assert_eq!(request_id, "req-1");
            assert_eq!(actor_id.node, NodeId("ractor-node".into()));
        }
        SpawnResponse::Failure { error, .. } => {
            panic!("spawn should succeed, got: {error}");
        }
    }
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 1);
}

#[test]
fn sa1_spawn_unknown_type_fails() {
    let mut runtime = RactorRuntime::new();
    let request = SpawnRequest {
        type_name: "nonexistent::Actor".into(),
        args_bytes: vec![],
        name: "ghost".into(),
        request_id: "req-2".into(),
    };

    let response = runtime.handle_spawn_request(&request);
    assert!(matches!(response, SpawnResponse::Failure { .. }));
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
}

#[test]
fn sa1_with_node_id() {
    let mut runtime = RactorRuntime::with_node_id(NodeId("node-42".into()));
    runtime.register_factory("test::Actor", |_| Ok(Box::new(())));

    let request = SpawnRequest {
        type_name: "test::Actor".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "a".into(),
        request_id: "req-3".into(),
    };

    let response = runtime.handle_spawn_request(&request);
    match response {
        SpawnResponse::Success { actor_id, .. } => {
            assert_eq!(actor_id.node, NodeId("node-42".into()));
        }
        SpawnResponse::Failure { error, .. } => {
            panic!("spawn should succeed, got: {error}");
        }
    }
}

#[test]
fn sa1_multiple_spawns_get_unique_ids() {
    let mut runtime = RactorRuntime::new();
    runtime.register_factory("test::Actor", |_| Ok(Box::new(())));

    let mut ids = Vec::new();
    for i in 0..5 {
        let request = SpawnRequest {
            type_name: "test::Actor".into(),
            args_bytes: serde_json::to_vec(&()).unwrap(),
            name: format!("actor-{i}"),
            request_id: format!("req-{i}"),
        };
        if let SpawnResponse::Success { actor_id, .. } = runtime.handle_spawn_request(&request) {
            ids.push(actor_id);
        }
    }
    assert_eq!(ids.len(), 5);
    // All IDs should be unique
    let unique: std::collections::HashSet<_> = ids.iter().map(|id| id.local).collect();
    assert_eq!(unique.len(), 5);
}

// ---------------------------------------------------------------------------
// SA2: WatchManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa2_runtime_has_watch_manager() {
    let runtime = RactorRuntime::new();
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

#[test]
fn sa2_remote_watch_and_notify() {
    let mut runtime = RactorRuntime::new();
    let target = ActorId {
        node: NodeId("node-1".into()),
        local: 1,
    };
    let watcher = ActorId {
        node: NodeId("node-2".into()),
        local: 10,
    };

    runtime.remote_watch(target.clone(), watcher.clone());
    assert_eq!(runtime.watch_manager().watched_count(), 1);
    assert_eq!(runtime.watch_manager().watchers_of(&target).len(), 1);

    let notifications = runtime.notify_terminated(&target);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].terminated, target);
    assert_eq!(notifications[0].watcher, watcher);

    // After notification, watch entry is cleaned up
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

#[test]
fn sa2_remote_unwatch() {
    let mut runtime = RactorRuntime::new();
    let target = ActorId {
        node: NodeId("node-1".into()),
        local: 1,
    };
    let watcher = ActorId {
        node: NodeId("node-2".into()),
        local: 10,
    };

    runtime.remote_watch(target.clone(), watcher.clone());
    assert_eq!(runtime.watch_manager().watched_count(), 1);

    runtime.remote_unwatch(&target, &watcher);
    assert_eq!(runtime.watch_manager().watched_count(), 0);

    // Notification after unwatch should produce no results
    let notifications = runtime.notify_terminated(&target);
    assert_eq!(notifications.len(), 0);
}

#[test]
fn sa2_multiple_watchers() {
    let mut runtime = RactorRuntime::new();
    let target = ActorId {
        node: NodeId("node-1".into()),
        local: 1,
    };

    for i in 0..3 {
        let watcher = ActorId {
            node: NodeId(format!("node-{}", i + 10)),
            local: i + 100,
        };
        runtime.remote_watch(target.clone(), watcher);
    }

    assert_eq!(runtime.watch_manager().watchers_of(&target).len(), 3);

    let notifications = runtime.notify_terminated(&target);
    assert_eq!(notifications.len(), 3);
}

// ---------------------------------------------------------------------------
// SA3: CancelManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa3_runtime_has_cancel_manager() {
    let runtime = RactorRuntime::new();
    assert_eq!(runtime.cancel_manager().active_count(), 0);
}

#[test]
fn sa3_register_and_cancel() {
    let mut runtime = RactorRuntime::new();
    let token = CancellationToken::new();
    let token_clone = token.clone();

    runtime.register_cancel("req-1".into(), token);
    assert_eq!(runtime.cancel_manager().active_count(), 1);

    let response = runtime.cancel_request("req-1");
    assert!(matches!(response, CancelResponse::Acknowledged));
    assert!(token_clone.is_cancelled());
    assert_eq!(runtime.cancel_manager().active_count(), 0);
}

#[test]
fn sa3_cancel_unknown_request() {
    let mut runtime = RactorRuntime::new();
    let response = runtime.cancel_request("nonexistent");
    assert!(matches!(response, CancelResponse::NotFound { .. }));
}

#[test]
fn sa3_multiple_tokens() {
    let mut runtime = RactorRuntime::new();
    let tokens: Vec<_> = (0..3)
        .map(|i| {
            let token = CancellationToken::new();
            let clone = token.clone();
            runtime.register_cancel(format!("req-{i}"), token);
            clone
        })
        .collect();

    assert_eq!(runtime.cancel_manager().active_count(), 3);

    // Cancel the middle one
    let response = runtime.cancel_request("req-1");
    assert!(matches!(response, CancelResponse::Acknowledged));
    assert!(!tokens[0].is_cancelled());
    assert!(tokens[1].is_cancelled());
    assert!(!tokens[2].is_cancelled());
    assert_eq!(runtime.cancel_manager().active_count(), 2);
}

// ---------------------------------------------------------------------------
// SA4: NodeDirectory wiring
// ---------------------------------------------------------------------------

#[test]
fn sa4_runtime_has_node_directory() {
    let runtime = RactorRuntime::new();
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

#[test]
fn sa4_connect_and_disconnect_peer() {
    let mut runtime = RactorRuntime::new();
    let peer = NodeId("peer-1".into());

    runtime.connect_peer(peer.clone(), Some("10.0.0.1:4697".into()));
    assert!(runtime.is_peer_connected(&peer));
    assert_eq!(runtime.node_directory().peer_count(), 1);
    assert_eq!(runtime.node_directory().connected_count(), 1);

    let info = runtime.node_directory().get_peer(&peer).unwrap();
    assert_eq!(info.address.as_deref(), Some("10.0.0.1:4697"));

    runtime.disconnect_peer(&peer);
    assert!(!runtime.is_peer_connected(&peer));
    assert_eq!(runtime.node_directory().peer_count(), 1); // still known, just disconnected
    assert_eq!(runtime.node_directory().connected_count(), 0);
}

#[test]
fn sa4_multiple_peers() {
    let mut runtime = RactorRuntime::new();
    for i in 0..5 {
        runtime.connect_peer(NodeId(format!("peer-{i}")), None);
    }

    assert_eq!(runtime.node_directory().peer_count(), 5);
    assert_eq!(runtime.node_directory().connected_count(), 5);

    runtime.disconnect_peer(&NodeId("peer-2".into()));
    assert_eq!(runtime.node_directory().connected_count(), 4);

    let disconnected = runtime
        .node_directory()
        .peers_with_status(PeerStatus::Disconnected);
    assert_eq!(disconnected.len(), 1);
    assert_eq!(disconnected[0].node_id, NodeId("peer-2".into()));
}

#[test]
fn sa4_node_id_accessor() {
    let runtime = RactorRuntime::new();
    assert_eq!(runtime.node_id(), &NodeId("ractor-node".into()));

    let custom = RactorRuntime::with_node_id(NodeId("custom-42".into()));
    assert_eq!(custom.node_id(), &NodeId("custom-42".into()));
}

#[test]
fn sa4_connect_without_address() {
    let mut runtime = RactorRuntime::new();
    let peer = NodeId("peer-no-addr".into());
    runtime.connect_peer(peer.clone(), None);

    let info = runtime.node_directory().get_peer(&peer).unwrap();
    assert!(info.address.is_none());
    assert_eq!(info.status, PeerStatus::Connected);
}

// ---------------------------------------------------------------------------
// Cross-system-actor integration
// ---------------------------------------------------------------------------

#[test]
fn all_system_actors_initialized_on_new() {
    let runtime = RactorRuntime::new();
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

#[test]
fn with_node_id_initializes_all_system_actors() {
    let runtime = RactorRuntime::with_node_id(NodeId("test-node".into()));
    assert_eq!(runtime.node_id(), &NodeId("test-node".into()));
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert_eq!(runtime.node_directory().peer_count(), 0);
}
