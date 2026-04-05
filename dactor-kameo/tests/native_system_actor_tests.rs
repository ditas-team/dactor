//! Tests for NA5-NA8: native kameo system actors.
//!
//! Validates that SpawnManagerActor, WatchManagerActor, CancelManagerActor,
//! and NodeDirectoryActor work as real kameo actors with mailbox-based
//! message delivery using kameo's native tell/ask semantics.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::{CancelResponse, SpawnRequest};
use dactor::type_registry::TypeRegistry;
use dactor_kameo::system_actors::*;
use kameo::actor::Spawn;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// NA5: SpawnManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na5_spawn_manager_handles_request() {
    let mut registry = TypeRegistry::new();
    registry.register_factory("test::Counter", |bytes: &[u8]| {
        let val: i32 = serde_json::from_slice(bytes)
            .map_err(|e| dactor::remote::SerializationError::new(e.to_string()))?;
        Ok(Box::new(val))
    });

    let node_id = NodeId("na-kameo".into());
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1));
    let actor_ref = SpawnManagerActor::spawn_with_mailbox(
        (node_id.clone(), registry, counter),
        kameo::mailbox::unbounded(),
    );

    let request = SpawnRequest {
        type_name: "test::Counter".into(),
        args_bytes: serde_json::to_vec(&42i32).unwrap(),
        name: "counter-1".into(),
        request_id: "req-1".into(),
    };

    let result = actor_ref
        .ask(HandleSpawnRequest(request))
        .await
        .expect("ask failed");

    let (actor_id, actor) = result;
    assert_eq!(actor_id.node, node_id);
    let val = actor.downcast::<i32>().expect("should be i32");
    assert_eq!(*val, 42);

    // Verify spawned actors list
    let spawned = actor_ref
        .ask(GetSpawnedActors)
        .await
        .expect("ask failed");
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0], actor_id);
}

#[tokio::test]
async fn na5_spawn_manager_unknown_type() {
    let registry = TypeRegistry::new();
    let node_id = NodeId("na-kameo".into());
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1));
    let actor_ref = SpawnManagerActor::spawn_with_mailbox(
        (node_id, registry, counter),
        kameo::mailbox::unbounded(),
    );

    let request = SpawnRequest {
        type_name: "nonexistent::Actor".into(),
        args_bytes: vec![],
        name: "ghost".into(),
        request_id: "req-2".into(),
    };

    let result = actor_ref
        .ask(HandleSpawnRequest(request))
        .await;

    // Kameo unwraps Result replies — SpawnResponse::Failure becomes the ask error
    assert!(result.is_err(), "expected spawn failure for unknown type");
}

#[tokio::test]
async fn na5_spawn_manager_register_factory() {
    let registry = TypeRegistry::new();
    let node_id = NodeId("na-kameo".into());
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1));
    let actor_ref = SpawnManagerActor::spawn_with_mailbox(
        (node_id, registry, counter),
        kameo::mailbox::unbounded(),
    );

    // Register factory via ask (synchronous — reply confirms registration)
    actor_ref
        .ask(RegisterFactory {
            type_name: "test::Unit".into(),
            factory: Box::new(|_| Ok(Box::new(()))),
        })
        .await
        .expect("registration failed");

    // Now spawn should succeed
    let request = SpawnRequest {
        type_name: "test::Unit".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "unit".into(),
        request_id: "req-3".into(),
    };

    actor_ref
        .ask(HandleSpawnRequest(request))
        .await
        .expect("ask failed");
    // If we got here, the spawn succeeded (kameo unwraps Result::Ok)
}

// ---------------------------------------------------------------------------
// NA6: WatchManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na6_watch_manager_watch_and_notify() {
    let actor_ref = WatchManagerActor::spawn_with_mailbox((), kameo::mailbox::unbounded());

    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 10 };

    actor_ref
        .ask(RemoteWatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .await
        .expect("watch failed");

    let count = actor_ref.ask(GetWatchedCount).await.expect("ask failed");
    assert_eq!(count, 1);

    let notifications = actor_ref
        .ask(OnTerminated(target.clone()))
        .await
        .expect("ask failed");
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].terminated, target);
    assert_eq!(notifications[0].watcher, watcher);

    let count = actor_ref.ask(GetWatchedCount).await.expect("ask failed");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn na6_watch_manager_unwatch() {
    let actor_ref = WatchManagerActor::spawn_with_mailbox((), kameo::mailbox::unbounded());

    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 10 };

    actor_ref
        .ask(RemoteWatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .await
        .expect("watch failed");

    actor_ref
        .ask(RemoteUnwatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .await
        .expect("unwatch failed");

    let count = actor_ref.ask(GetWatchedCount).await.expect("ask failed");
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// NA7: CancelManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na7_cancel_manager_register_and_cancel() {
    let actor_ref = CancelManagerActor::spawn_with_mailbox((), kameo::mailbox::unbounded());

    let token = CancellationToken::new();
    let token_clone = token.clone();

    actor_ref
        .ask(RegisterCancel {
            request_id: "req-1".into(),
            token,
        })
        .await
        .expect("register failed");

    let count = actor_ref.ask(GetActiveCount).await.expect("ask failed");
    assert_eq!(count, 1);

    let response = actor_ref
        .ask(CancelById("req-1".into()))
        .await
        .expect("ask failed");
    assert!(matches!(response, CancelResponse::Acknowledged));
    assert!(token_clone.is_cancelled());

    let count = actor_ref.ask(GetActiveCount).await.expect("ask failed");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn na7_cancel_manager_complete_cleanup() {
    let actor_ref = CancelManagerActor::spawn_with_mailbox((), kameo::mailbox::unbounded());

    actor_ref
        .ask(RegisterCancel {
            request_id: "req-cleanup".into(),
            token: CancellationToken::new(),
        })
        .await
        .expect("register failed");

    actor_ref
        .ask(CompleteRequest("req-cleanup".into()))
        .await
        .expect("complete failed");

    let count = actor_ref.ask(GetActiveCount).await.expect("ask failed");
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// NA8: NodeDirectoryActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na8_node_directory_connect_disconnect() {
    let actor_ref = NodeDirectoryActor::spawn_with_mailbox((), kameo::mailbox::unbounded());

    let peer = NodeId("peer-1".into());

    actor_ref
        .ask(ConnectPeer {
            peer_id: peer.clone(),
            address: Some("10.0.0.1:4697".into()),
        })
        .await
        .expect("connect failed");

    let connected = actor_ref
        .ask(IsConnected(peer.clone()))
        .await
        .expect("ask failed");
    assert!(connected);

    let count = actor_ref.ask(GetPeerCount).await.expect("ask failed");
    assert_eq!(count, 1);

    actor_ref
        .ask(DisconnectPeer(peer.clone()))
        .await
        .expect("disconnect failed");

    let connected = actor_ref
        .ask(IsConnected(peer))
        .await
        .expect("ask failed");
    assert!(!connected);
}

#[tokio::test]
async fn na8_node_directory_reconnect_preserves_address() {
    let actor_ref = NodeDirectoryActor::spawn_with_mailbox((), kameo::mailbox::unbounded());

    let peer = NodeId("peer-1".into());

    actor_ref
        .ask(ConnectPeer {
            peer_id: peer.clone(),
            address: Some("10.0.0.1:4697".into()),
        })
        .await
        .expect("connect failed");

    actor_ref
        .ask(DisconnectPeer(peer.clone()))
        .await
        .expect("disconnect failed");

    // Reconnect without address
    actor_ref
        .ask(ConnectPeer {
            peer_id: peer.clone(),
            address: None,
        })
        .await
        .expect("reconnect failed");

    let connected = actor_ref
        .ask(IsConnected(peer.clone()))
        .await
        .expect("ask failed");
    assert!(connected);

    // Verify address preserved
    let info = actor_ref
        .ask(GetPeerInfo(peer))
        .await
        .expect("ask failed")
        .expect("peer should exist");
    assert_eq!(
        info.address.as_deref(),
        Some("10.0.0.1:4697"),
        "address should be preserved on reconnect without explicit address"
    );
}
