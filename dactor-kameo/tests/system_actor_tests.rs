//! Tests for SA5-SA8: system actor wiring in the kameo adapter.
//!
//! Validates that `KameoRuntime` correctly integrates SpawnManager,
//! WatchManager, CancelManager, and NodeDirectory for remote operations.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::{CancelResponse, PeerStatus, SpawnRequest};
use dactor_kameo::KameoRuntime;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// SA5: SpawnManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa5_runtime_has_spawn_manager() {
    let runtime = KameoRuntime::new();
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
}

#[test]
fn sa5_register_factory_and_create() {
    let mut runtime = KameoRuntime::new();
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

    let result = runtime.handle_spawn_request(&request);
    match result {
        Ok((actor_id, actor)) => {
            assert_eq!(actor_id.node, NodeId("kameo-node".into()));
            let value = actor.downcast::<i32>().expect("should be i32");
            assert_eq!(*value, 42);
        }
        Err(resp) => {
            panic!("spawn should succeed, got: {resp:?}");
        }
    }
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 1);
}

#[test]
fn sa5_spawn_unknown_type_fails() {
    let mut runtime = KameoRuntime::new();
    let request = SpawnRequest {
        type_name: "nonexistent::Actor".into(),
        args_bytes: vec![],
        name: "ghost".into(),
        request_id: "req-2".into(),
    };

    let result = runtime.handle_spawn_request(&request);
    match result {
        Err(resp) => {
            // Verify request_id is preserved for error correlation
            if let dactor::system_actors::SpawnResponse::Failure { request_id, .. } = resp {
                assert_eq!(request_id, "req-2");
            } else {
                panic!("expected SpawnResponse::Failure");
            }
        }
        Ok(_) => panic!("spawn of unknown type should fail"),
    }
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
}

#[test]
fn sa5_with_node_id() {
    let mut runtime = KameoRuntime::with_node_id(NodeId("node-42".into()));
    runtime.register_factory("test::Actor", |_| Ok(Box::new(())));

    let request = SpawnRequest {
        type_name: "test::Actor".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "a".into(),
        request_id: "req-3".into(),
    };

    let result = runtime.handle_spawn_request(&request);
    match result {
        Ok((actor_id, _actor)) => {
            assert_eq!(actor_id.node, NodeId("node-42".into()));
        }
        Err(resp) => {
            panic!("spawn should succeed, got: {resp:?}");
        }
    }
}

#[test]
fn sa5_multiple_spawns_get_unique_ids() {
    let mut runtime = KameoRuntime::new();
    runtime.register_factory("test::Actor", |_| Ok(Box::new(())));

    let mut ids = Vec::new();
    for i in 0..5 {
        let request = SpawnRequest {
            type_name: "test::Actor".into(),
            args_bytes: serde_json::to_vec(&()).unwrap(),
            name: format!("actor-{i}"),
            request_id: format!("req-{i}"),
        };
        if let Ok((actor_id, _)) = runtime.handle_spawn_request(&request) {
            ids.push(actor_id);
        }
    }
    assert_eq!(ids.len(), 5);
    let unique: std::collections::HashSet<_> = ids.iter().map(|id| id.local).collect();
    assert_eq!(unique.len(), 5);
}

#[test]
fn sa5_spawn_returns_actor_for_caller_to_use() {
    let mut runtime = KameoRuntime::new();
    runtime.register_factory("test::Pair", |bytes: &[u8]| {
        let pair: (String, u32) = serde_json::from_slice(bytes)
            .map_err(|e| dactor::remote::SerializationError::new(e.to_string()))?;
        Ok(Box::new(pair))
    });

    let request = SpawnRequest {
        type_name: "test::Pair".into(),
        args_bytes: serde_json::to_vec(&("hello".to_string(), 99u32)).unwrap(),
        name: "pair-actor".into(),
        request_id: "req-typed".into(),
    };

    let (actor_id, actor) = runtime.handle_spawn_request(&request).unwrap();
    let pair = actor.downcast::<(String, u32)>().expect("should be (String, u32)");
    assert_eq!(*pair, ("hello".to_string(), 99));
    assert_eq!(actor_id.node, *runtime.node_id());
}

#[test]
fn sa5_malformed_bytes_returns_error() {
    let mut runtime = KameoRuntime::new();
    runtime.register_factory("test::Counter", |bytes: &[u8]| {
        let _: i32 = serde_json::from_slice(bytes)
            .map_err(|e| dactor::remote::SerializationError::new(e.to_string()))?;
        Ok(Box::new(()))
    });

    let request = SpawnRequest {
        type_name: "test::Counter".into(),
        args_bytes: b"not valid json".to_vec(),
        name: "bad-actor".into(),
        request_id: "req-bad".into(),
    };

    let result = runtime.handle_spawn_request(&request);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// SA6: WatchManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa6_runtime_has_watch_manager() {
    let runtime = KameoRuntime::new();
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

#[test]
fn sa6_remote_watch_and_notify() {
    let mut runtime = KameoRuntime::new();
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
fn sa6_remote_unwatch() {
    let mut runtime = KameoRuntime::new();
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

    let notifications = runtime.notify_terminated(&target);
    assert_eq!(notifications.len(), 0);
}

#[test]
fn sa6_multiple_watchers() {
    let mut runtime = KameoRuntime::new();
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

#[test]
fn sa6_unwatch_nonexistent_is_noop() {
    let mut runtime = KameoRuntime::new();
    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 2 };

    runtime.remote_unwatch(&target, &watcher);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

// ---------------------------------------------------------------------------
// SA7: CancelManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa7_runtime_has_cancel_manager() {
    let runtime = KameoRuntime::new();
    assert_eq!(runtime.cancel_manager().active_count(), 0);
}

#[test]
fn sa7_register_and_cancel() {
    let mut runtime = KameoRuntime::new();
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
fn sa7_cancel_unknown_request() {
    let mut runtime = KameoRuntime::new();
    let response = runtime.cancel_request("nonexistent");
    assert!(matches!(response, CancelResponse::NotFound { .. }));
}

#[test]
fn sa7_multiple_tokens() {
    let mut runtime = KameoRuntime::new();
    let tokens: Vec<_> = (0..3)
        .map(|i| {
            let token = CancellationToken::new();
            let clone = token.clone();
            runtime.register_cancel(format!("req-{i}"), token);
            clone
        })
        .collect();

    assert_eq!(runtime.cancel_manager().active_count(), 3);

    let response = runtime.cancel_request("req-1");
    assert!(matches!(response, CancelResponse::Acknowledged));
    assert!(!tokens[0].is_cancelled());
    assert!(tokens[1].is_cancelled());
    assert!(!tokens[2].is_cancelled());
    assert_eq!(runtime.cancel_manager().active_count(), 2);
}

#[test]
fn sa7_double_cancel_returns_not_found() {
    let mut runtime = KameoRuntime::new();
    let token = CancellationToken::new();
    runtime.register_cancel("req-1".into(), token);

    let first = runtime.cancel_request("req-1");
    assert!(matches!(first, CancelResponse::Acknowledged));

    let second = runtime.cancel_request("req-1");
    assert!(matches!(second, CancelResponse::NotFound { .. }));
}

#[test]
fn sa7_complete_request_cleans_up_token() {
    let mut runtime = KameoRuntime::new();
    let token = CancellationToken::new();
    runtime.register_cancel("req-cleanup".into(), token);
    assert_eq!(runtime.cancel_manager().active_count(), 1);

    // Simulate normal completion — token should be removed
    runtime.complete_request("req-cleanup");
    assert_eq!(runtime.cancel_manager().active_count(), 0);

    // Cancel after completion should return NotFound
    let response = runtime.cancel_request("req-cleanup");
    assert!(matches!(response, CancelResponse::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// SA8: NodeDirectory wiring
// ---------------------------------------------------------------------------

#[test]
fn sa8_runtime_has_node_directory() {
    let runtime = KameoRuntime::new();
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

#[test]
fn sa8_connect_and_disconnect_peer() {
    let mut runtime = KameoRuntime::new();
    let peer = NodeId("peer-1".into());

    runtime.connect_peer(peer.clone(), Some("10.0.0.1:4697".into()));
    assert!(runtime.is_peer_connected(&peer));
    assert_eq!(runtime.node_directory().peer_count(), 1);
    assert_eq!(runtime.node_directory().connected_count(), 1);

    let info = runtime.node_directory().get_peer(&peer).unwrap();
    assert_eq!(info.address.as_deref(), Some("10.0.0.1:4697"));

    runtime.disconnect_peer(&peer);
    assert!(!runtime.is_peer_connected(&peer));
    assert_eq!(runtime.node_directory().peer_count(), 1);
    assert_eq!(runtime.node_directory().connected_count(), 0);
}

#[test]
fn sa8_multiple_peers() {
    let mut runtime = KameoRuntime::new();
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
fn sa8_node_id_accessor() {
    let runtime = KameoRuntime::new();
    assert_eq!(runtime.node_id(), &NodeId("kameo-node".into()));

    let custom = KameoRuntime::with_node_id(NodeId("custom-42".into()));
    assert_eq!(custom.node_id(), &NodeId("custom-42".into()));
}

#[test]
fn sa8_connect_without_address() {
    let mut runtime = KameoRuntime::new();
    let peer = NodeId("peer-no-addr".into());
    runtime.connect_peer(peer.clone(), None);

    let info = runtime.node_directory().get_peer(&peer).unwrap();
    assert!(info.address.is_none());
    assert_eq!(info.status, PeerStatus::Connected);
}

#[test]
fn sa8_reconnect_preserves_address() {
    let mut runtime = KameoRuntime::new();
    let peer = NodeId("peer-1".into());

    runtime.connect_peer(peer.clone(), Some("10.0.0.1:4697".into()));
    runtime.disconnect_peer(&peer);
    runtime.connect_peer(peer.clone(), None);

    assert!(runtime.is_peer_connected(&peer));
    assert_eq!(
        runtime.node_directory().get_peer(&peer).unwrap().address.as_deref(),
        Some("10.0.0.1:4697"),
        "address should be preserved on reconnect without explicit address"
    );
}

#[test]
fn sa8_reconnect_updates_address() {
    let mut runtime = KameoRuntime::new();
    let peer = NodeId("peer-1".into());

    runtime.connect_peer(peer.clone(), Some("10.0.0.1:4697".into()));
    runtime.disconnect_peer(&peer);
    runtime.connect_peer(peer.clone(), Some("10.0.0.2:4697".into()));

    assert_eq!(
        runtime.node_directory().get_peer(&peer).unwrap().address.as_deref(),
        Some("10.0.0.2:4697"),
        "address should be updated when explicitly provided"
    );
}

// ---------------------------------------------------------------------------
// Cross-system-actor integration
// ---------------------------------------------------------------------------

#[test]
fn all_system_actors_initialized_on_new() {
    let runtime = KameoRuntime::new();
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

#[test]
fn with_node_id_initializes_all_system_actors() {
    let runtime = KameoRuntime::with_node_id(NodeId("test-node".into()));
    assert_eq!(runtime.node_id(), &NodeId("test-node".into()));
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert_eq!(runtime.node_directory().peer_count(), 0);
}


