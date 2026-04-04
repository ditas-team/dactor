//! Tests for SA10: system actor wiring in the coerce adapter.
//!
//! Validates that `CoerceRuntime` correctly integrates SpawnManager,
//! WatchManager, CancelManager, and NodeDirectory for remote operations.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::{CancelResponse, SpawnRequest};
use dactor_coerce::CoerceRuntime;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// SpawnManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa10_runtime_has_spawn_manager() {
    let runtime = CoerceRuntime::new();
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
}

#[test]
fn sa10_register_factory_and_create() {
    let mut runtime = CoerceRuntime::new();
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
            assert_eq!(actor_id.node, NodeId("coerce-node".into()));
            let value = actor.downcast::<i32>().expect("should be i32");
            assert_eq!(*value, 42);
        }
        Err(resp) => panic!("spawn should succeed, got: {resp:?}"),
    }
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 1);
}

#[test]
fn sa10_spawn_unknown_type_fails() {
    let mut runtime = CoerceRuntime::new();
    let request = SpawnRequest {
        type_name: "nonexistent::Actor".into(),
        args_bytes: vec![],
        name: "ghost".into(),
        request_id: "req-2".into(),
    };

    let result = runtime.handle_spawn_request(&request);
    match result {
        Err(resp) => {
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
fn sa10_with_node_id() {
    let mut runtime = CoerceRuntime::with_node_id(NodeId("node-42".into()));
    runtime.register_factory("test::Actor", |_| Ok(Box::new(())));

    let request = SpawnRequest {
        type_name: "test::Actor".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "a".into(),
        request_id: "req-3".into(),
    };

    let (actor_id, _) = runtime.handle_spawn_request(&request).unwrap();
    assert_eq!(actor_id.node, NodeId("node-42".into()));
}

#[test]
fn sa10_multiple_spawns_get_unique_ids() {
    let mut runtime = CoerceRuntime::new();
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
fn sa10_malformed_bytes_returns_error() {
    let mut runtime = CoerceRuntime::new();
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

    assert!(runtime.handle_spawn_request(&request).is_err());
}

// ---------------------------------------------------------------------------
// WatchManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa10_watch_manager() {
    let runtime = CoerceRuntime::new();
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

#[test]
fn sa10_remote_watch_and_notify() {
    let mut runtime = CoerceRuntime::new();
    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 10 };

    runtime.remote_watch(target.clone(), watcher.clone());
    assert_eq!(runtime.watch_manager().watched_count(), 1);

    let notifications = runtime.notify_terminated(&target);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].terminated, target);
    assert_eq!(notifications[0].watcher, watcher);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

#[test]
fn sa10_remote_unwatch() {
    let mut runtime = CoerceRuntime::new();
    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 10 };

    runtime.remote_watch(target.clone(), watcher.clone());
    runtime.remote_unwatch(&target, &watcher);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.notify_terminated(&target).len(), 0);
}

#[test]
fn sa10_unwatch_nonexistent_is_noop() {
    let mut runtime = CoerceRuntime::new();
    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 2 };
    runtime.remote_unwatch(&target, &watcher);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
}

// ---------------------------------------------------------------------------
// CancelManager wiring
// ---------------------------------------------------------------------------

#[test]
fn sa10_cancel_manager() {
    let runtime = CoerceRuntime::new();
    assert_eq!(runtime.cancel_manager().active_count(), 0);
}

#[test]
fn sa10_register_and_cancel() {
    let mut runtime = CoerceRuntime::new();
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
fn sa10_cancel_unknown_request() {
    let mut runtime = CoerceRuntime::new();
    assert!(matches!(runtime.cancel_request("nope"), CancelResponse::NotFound { .. }));
}

#[test]
fn sa10_double_cancel_returns_not_found() {
    let mut runtime = CoerceRuntime::new();
    runtime.register_cancel("req-1".into(), CancellationToken::new());

    assert!(matches!(runtime.cancel_request("req-1"), CancelResponse::Acknowledged));
    assert!(matches!(runtime.cancel_request("req-1"), CancelResponse::NotFound { .. }));
}

#[test]
fn sa10_complete_request_cleans_up_token() {
    let mut runtime = CoerceRuntime::new();
    runtime.register_cancel("req-cleanup".into(), CancellationToken::new());
    assert_eq!(runtime.cancel_manager().active_count(), 1);

    runtime.complete_request("req-cleanup");
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert!(matches!(runtime.cancel_request("req-cleanup"), CancelResponse::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// NodeDirectory wiring
// ---------------------------------------------------------------------------

#[test]
fn sa10_node_directory() {
    let runtime = CoerceRuntime::new();
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

#[test]
fn sa10_connect_and_disconnect_peer() {
    let mut runtime = CoerceRuntime::new();
    let peer = NodeId("peer-1".into());

    runtime.connect_peer(peer.clone(), Some("10.0.0.1:4697".into()));
    assert!(runtime.is_peer_connected(&peer));
    assert_eq!(runtime.node_directory().get_peer(&peer).unwrap().address.as_deref(), Some("10.0.0.1:4697"));

    runtime.disconnect_peer(&peer);
    assert!(!runtime.is_peer_connected(&peer));
    assert_eq!(runtime.node_directory().peer_count(), 1);
}

#[test]
fn sa10_reconnect_preserves_address() {
    let mut runtime = CoerceRuntime::new();
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
fn sa10_reconnect_updates_address() {
    let mut runtime = CoerceRuntime::new();
    let peer = NodeId("peer-1".into());

    runtime.connect_peer(peer.clone(), Some("10.0.0.1:4697".into()));
    runtime.disconnect_peer(&peer);
    runtime.connect_peer(peer.clone(), Some("10.0.0.2:4697".into()));

    assert_eq!(
        runtime.node_directory().get_peer(&peer).unwrap().address.as_deref(),
        Some("10.0.0.2:4697")
    );
}

#[test]
fn sa10_node_id_accessor() {
    let runtime = CoerceRuntime::new();
    assert_eq!(runtime.node_id(), &NodeId("coerce-node".into()));

    let custom = CoerceRuntime::with_node_id(NodeId("custom-42".into()));
    assert_eq!(custom.node_id(), &NodeId("custom-42".into()));
}

// ---------------------------------------------------------------------------
// Cross-system-actor integration
// ---------------------------------------------------------------------------

#[test]
fn sa10_all_system_actors_initialized() {
    let runtime = CoerceRuntime::new();
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

#[test]
fn sa10_with_node_id_initializes_all() {
    let runtime = CoerceRuntime::with_node_id(NodeId("test-node".into()));
    assert_eq!(runtime.node_id(), &NodeId("test-node".into()));
    assert_eq!(runtime.spawn_manager().spawned_actors().len(), 0);
    assert_eq!(runtime.watch_manager().watched_count(), 0);
    assert_eq!(runtime.cancel_manager().active_count(), 0);
    assert_eq!(runtime.node_directory().peer_count(), 0);
}

// ---------------------------------------------------------------------------
// Node ID consistency between local and remote spawn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sa10_local_spawn_uses_correct_node_id() {
    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::message::Message;

    struct DummyActor;
    impl Actor for DummyActor {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self { DummyActor }
    }

    struct Ping;
    impl Message for Ping { type Reply = (); }

    #[async_trait::async_trait]
    impl Handler<Ping> for DummyActor {
        async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {}
    }

    let runtime = CoerceRuntime::new();
    let actor_ref = runtime.spawn::<DummyActor>("dummy", ());

    // Local spawn should use the same node ID as runtime.node_id()
    assert_eq!(actor_ref.id().node, *runtime.node_id());
}

#[tokio::test]
async fn sa10_local_and_remote_spawn_ids_dont_collide() {
    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::message::Message;

    struct DummyActor;
    impl Actor for DummyActor {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self { DummyActor }
    }

    struct Ping;
    impl Message for Ping { type Reply = (); }

    #[async_trait::async_trait]
    impl Handler<Ping> for DummyActor {
        async fn handle(&mut self, _msg: Ping, _ctx: &mut ActorContext) {}
    }

    let mut runtime = CoerceRuntime::new();
    runtime.register_factory("test::Actor", |_| Ok(Box::new(())));

    // Local spawn
    let local_ref = runtime.spawn::<DummyActor>("local", ());
    let local_id = local_ref.id();

    // Remote spawn
    let request = SpawnRequest {
        type_name: "test::Actor".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "remote".into(),
        request_id: "req-1".into(),
    };
    let (remote_id, _) = runtime.handle_spawn_request(&request).unwrap();

    // Both should use the same node ID
    assert_eq!(local_id.node, *runtime.node_id());
    assert_eq!(remote_id.node, *runtime.node_id());

    // IDs should not collide
    assert_ne!(local_id.local, remote_id.local,
        "local spawn ({}) and remote spawn ({}) should have different local IDs",
        local_id.local, remote_id.local);
}
