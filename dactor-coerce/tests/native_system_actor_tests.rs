//! Tests for CP5/CP6: native coerce system actors.
//!
//! Validates that the native coerce system actors (SpawnManagerActor,
//! WatchManagerActor, CancelManagerActor, NodeDirectoryActor) work correctly
//! when spawned via the CoerceRuntime.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::{CancelResponse, SpawnRequest};
use dactor_coerce::system_actors::*;
use dactor_coerce::CoerceRuntime;

// ---------------------------------------------------------------------------
// CP5: SpawnManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cp5_spawn_manager_handle_spawn_request() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().expect("system actors started");

    // Register a factory via the native actor
    let factory: FactoryFn = Box::new(|bytes: &[u8]| {
        let value: i32 = serde_json::from_slice(bytes)
            .map_err(|e| dactor::remote::SerializationError::new(e.to_string()))?;
        Ok(Box::new(value))
    });
    refs.spawn_manager
        .send(RegisterFactory {
            type_name: "test::Counter".into(),
            factory,
        })
        .await
        .expect("register factory");

    let request = SpawnRequest {
        type_name: "test::Counter".into(),
        args_bytes: serde_json::to_vec(&42i32).unwrap(),
        name: "counter-1".into(),
        request_id: "req-1".into(),
    };
    let outcome = refs
        .spawn_manager
        .send(HandleSpawnRequest(request))
        .await
        .expect("send spawn request");

    match outcome {
        SpawnOutcome::Success { ref actor_id, .. } => {
            assert_eq!(actor_id.node, *runtime.node_id());
            let actor = outcome.take_actor().expect("actor should be present");
            let value = actor.downcast::<i32>().expect("should be i32");
            assert_eq!(*value, 42);
        }
        SpawnOutcome::Failure(resp) => panic!("spawn should succeed, got: {resp:?}"),
    }

    let spawned = refs
        .spawn_manager
        .send(GetSpawnedActors)
        .await
        .expect("get spawned actors");
    assert_eq!(spawned.len(), 1);
}

#[tokio::test]
async fn cp5_spawn_manager_unknown_type_fails() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let request = SpawnRequest {
        type_name: "nonexistent::Actor".into(),
        args_bytes: vec![],
        name: "ghost".into(),
        request_id: "req-2".into(),
    };
    let outcome = refs
        .spawn_manager
        .send(HandleSpawnRequest(request))
        .await
        .expect("send spawn request");

    match outcome {
        SpawnOutcome::Failure(resp) => {
            if let dactor::system_actors::SpawnResponse::Failure { request_id, .. } = resp {
                assert_eq!(request_id, "req-2");
            } else {
                panic!("expected SpawnResponse::Failure");
            }
        }
        SpawnOutcome::Success { .. } => panic!("spawn of unknown type should fail"),
    }
}

#[tokio::test]
async fn cp5_spawn_manager_multiple_unique_ids() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let factory: FactoryFn = Box::new(|_| Ok(Box::new(())));
    refs.spawn_manager
        .send(RegisterFactory {
            type_name: "test::Actor".into(),
            factory,
        })
        .await
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..5 {
        let request = SpawnRequest {
            type_name: "test::Actor".into(),
            args_bytes: serde_json::to_vec(&()).unwrap(),
            name: format!("actor-{i}"),
            request_id: format!("req-{i}"),
        };
        let outcome = refs
            .spawn_manager
            .send(HandleSpawnRequest(request))
            .await
            .unwrap();
        if let SpawnOutcome::Success { actor_id, .. } = outcome {
            ids.push(actor_id);
        }
    }
    assert_eq!(ids.len(), 5);
    let unique: std::collections::HashSet<_> = ids.iter().map(|id| id.local).collect();
    assert_eq!(unique.len(), 5);
}

// ---------------------------------------------------------------------------
// CP5: WatchManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cp5_watch_manager_watch_and_notify() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let target = ActorId {
        node: NodeId("n1".into()),
        local: 1,
    };
    let watcher = ActorId {
        node: NodeId("n2".into()),
        local: 10,
    };

    refs.watch_manager
        .send(RemoteWatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .await
        .unwrap();

    let count = refs.watch_manager.send(GetWatchedCount).await.unwrap();
    assert_eq!(count, 1);

    let notifications = refs
        .watch_manager
        .send(OnTerminated(target.clone()))
        .await
        .unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].terminated, target);
    assert_eq!(notifications[0].watcher, watcher);

    let count = refs.watch_manager.send(GetWatchedCount).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn cp5_watch_manager_unwatch() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let target = ActorId {
        node: NodeId("n1".into()),
        local: 1,
    };
    let watcher = ActorId {
        node: NodeId("n2".into()),
        local: 10,
    };

    refs.watch_manager
        .send(RemoteWatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .await
        .unwrap();

    refs.watch_manager
        .send(RemoteUnwatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .await
        .unwrap();

    let count = refs.watch_manager.send(GetWatchedCount).await.unwrap();
    assert_eq!(count, 0);

    let notifications = refs
        .watch_manager
        .send(OnTerminated(target))
        .await
        .unwrap();
    assert_eq!(notifications.len(), 0);
}

// ---------------------------------------------------------------------------
// CP5: CancelManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cp5_cancel_manager_register_and_cancel() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let token = tokio_util::sync::CancellationToken::new();
    let token_clone = token.clone();

    refs.cancel_manager
        .send(RegisterCancel {
            request_id: "req-1".into(),
            token,
        })
        .await
        .unwrap();

    let count = refs.cancel_manager.send(GetActiveCount).await.unwrap();
    assert_eq!(count, 1);

    let outcome = refs
        .cancel_manager
        .send(CancelById("req-1".into()))
        .await
        .unwrap();
    assert!(matches!(outcome.0, CancelResponse::Acknowledged));
    assert!(token_clone.is_cancelled());

    let count = refs.cancel_manager.send(GetActiveCount).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn cp5_cancel_manager_cancel_unknown() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let outcome = refs
        .cancel_manager
        .send(CancelById("nope".into()))
        .await
        .unwrap();
    assert!(matches!(outcome.0, CancelResponse::NotFound { .. }));
}

#[tokio::test]
async fn cp5_cancel_manager_complete_request() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    refs.cancel_manager
        .send(RegisterCancel {
            request_id: "req-cleanup".into(),
            token: tokio_util::sync::CancellationToken::new(),
        })
        .await
        .unwrap();

    let count = refs.cancel_manager.send(GetActiveCount).await.unwrap();
    assert_eq!(count, 1);

    refs.cancel_manager
        .send(CompleteRequest("req-cleanup".into()))
        .await
        .unwrap();

    let count = refs.cancel_manager.send(GetActiveCount).await.unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// CP5: NodeDirectoryActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cp5_node_directory_connect_and_query() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let peer = NodeId("peer-1".into());

    refs.node_directory
        .send(ConnectPeer {
            peer_id: peer.clone(),
            address: Some("10.0.0.1:4697".into()),
        })
        .await
        .unwrap();

    let connected = refs
        .node_directory
        .send(IsConnected(peer.clone()))
        .await
        .unwrap();
    assert!(connected);

    let count = refs.node_directory.send(GetPeerCount).await.unwrap();
    assert_eq!(count, 1);

    let connected_count = refs.node_directory.send(GetConnectedCount).await.unwrap();
    assert_eq!(connected_count, 1);

    let info = refs
        .node_directory
        .send(GetPeerInfo(peer.clone()))
        .await
        .unwrap();
    assert!(info.is_some());
    assert_eq!(info.unwrap().address.as_deref(), Some("10.0.0.1:4697"));
}

#[tokio::test]
async fn cp5_node_directory_disconnect() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let peer = NodeId("peer-1".into());

    refs.node_directory
        .send(ConnectPeer {
            peer_id: peer.clone(),
            address: None,
        })
        .await
        .unwrap();

    refs.node_directory
        .send(DisconnectPeer(peer.clone()))
        .await
        .unwrap();

    let connected = refs
        .node_directory
        .send(IsConnected(peer.clone()))
        .await
        .unwrap();
    assert!(!connected);

    let count = refs.node_directory.send(GetPeerCount).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn cp5_node_directory_reconnect_preserves_address() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    let refs = runtime.system_actor_refs().unwrap();

    let peer = NodeId("peer-1".into());

    refs.node_directory
        .send(ConnectPeer {
            peer_id: peer.clone(),
            address: Some("10.0.0.1:4697".into()),
        })
        .await
        .unwrap();

    refs.node_directory
        .send(DisconnectPeer(peer.clone()))
        .await
        .unwrap();

    // Reconnect without address
    refs.node_directory
        .send(ConnectPeer {
            peer_id: peer.clone(),
            address: None,
        })
        .await
        .unwrap();

    let info = refs
        .node_directory
        .send(GetPeerInfo(peer.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        info.address.as_deref(),
        Some("10.0.0.1:4697"),
        "address should be preserved on reconnect without explicit address"
    );
}

// ---------------------------------------------------------------------------
// CP6: Runtime auto-start integration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cp6_system_actors_none_before_start() {
    let runtime = CoerceRuntime::new();
    assert!(runtime.system_actor_refs().is_none());
}

#[tokio::test]
async fn cp6_system_actors_available_after_start() {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();
    assert!(runtime.system_actor_refs().is_some());
}

#[tokio::test]
async fn cp6_system_actors_with_custom_node_id() {
    let mut runtime = CoerceRuntime::with_node_id(NodeId("custom-42".into()));
    runtime.start_system_actors();

    let refs = runtime.system_actor_refs().unwrap();

    // The spawn manager should use the custom node ID
    let factory: FactoryFn = Box::new(|_| Ok(Box::new(())));
    refs.spawn_manager
        .send(RegisterFactory {
            type_name: "test::Actor".into(),
            factory,
        })
        .await
        .unwrap();

    let request = SpawnRequest {
        type_name: "test::Actor".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "a".into(),
        request_id: "req-1".into(),
    };
    let outcome = refs
        .spawn_manager
        .send(HandleSpawnRequest(request))
        .await
        .unwrap();

    if let SpawnOutcome::Success { actor_id, .. } = outcome {
        assert_eq!(actor_id.node, NodeId("custom-42".into()));
    } else {
        panic!("expected success");
    }
}
