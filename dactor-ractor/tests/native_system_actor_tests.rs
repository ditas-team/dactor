//! Tests for NA1-NA4: native ractor system actors.
//!
//! Validates that SpawnManagerActor, WatchManagerActor, CancelManagerActor,
//! and NodeDirectoryActor work as real ractor actors with mailbox-based
//! message delivery.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::{CancelResponse, SpawnRequest, SpawnResponse};
use dactor::type_registry::TypeRegistry;
use dactor_ractor::system_actors::*;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// NA1: SpawnManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na1_spawn_manager_actor_handles_request() {
    let mut registry = TypeRegistry::new();
    registry.register_factory("test::Counter", |bytes: &[u8]| {
        let val: i32 = serde_json::from_slice(bytes)
            .map_err(|e| dactor::remote::SerializationError::new(e.to_string()))?;
        Ok(Box::new(val))
    });

    let node_id = NodeId("na-node".into());
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, SpawnManagerActor, (node_id.clone(), registry, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1))))
            .await
            .expect("spawn failed");

    let request = SpawnRequest {
        type_name: "test::Counter".into(),
        args_bytes: serde_json::to_vec(&42i32).unwrap(),
        name: "counter-1".into(),
        request_id: "req-1".into(),
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(SpawnManagerMsg::HandleRequest {
            request,
            reply: tx,
        })
        .expect("send failed");

    let result = rx.await.expect("reply dropped");
    let (actor_id, actor) = result.expect("spawn should succeed");
    assert_eq!(actor_id.node, node_id);
    let val = actor.downcast::<i32>().expect("should be i32");
    assert_eq!(*val, 42);

    // Verify spawned actors list
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(SpawnManagerMsg::GetSpawnedActors { reply: tx })
        .expect("send failed");
    let spawned = rx.await.expect("reply dropped");
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0], actor_id);

    actor_ref.stop(None);
}

#[tokio::test]
async fn na1_spawn_manager_actor_unknown_type() {
    let registry = TypeRegistry::new();
    let node_id = NodeId("na-node".into());
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, SpawnManagerActor, (node_id, registry, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1))))
            .await
            .expect("spawn failed");

    let request = SpawnRequest {
        type_name: "nonexistent::Actor".into(),
        args_bytes: vec![],
        name: "ghost".into(),
        request_id: "req-2".into(),
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(SpawnManagerMsg::HandleRequest {
            request,
            reply: tx,
        })
        .expect("send failed");

    let result = rx.await.expect("reply dropped");
    match result {
        Err(SpawnResponse::Failure { request_id, .. }) => {
            assert_eq!(request_id, "req-2");
        }
        _ => panic!("expected failure"),
    }

    actor_ref.stop(None);
}

#[tokio::test]
async fn na1_spawn_manager_actor_register_factory() {
    let registry = TypeRegistry::new();
    let node_id = NodeId("na-node".into());
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, SpawnManagerActor, (node_id, registry, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1))))
            .await
            .expect("spawn failed");

    // Register factory via message — await reply for synchronization
    let (reg_tx, reg_rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(SpawnManagerMsg::RegisterFactory {
            type_name: "test::Unit".into(),
            factory: Box::new(|_| Ok(Box::new(()))),
            reply: reg_tx,
        })
        .expect("send failed");

    reg_rx.await.expect("registration should complete");

    // Now spawn should succeed
    let request = SpawnRequest {
        type_name: "test::Unit".into(),
        args_bytes: serde_json::to_vec(&()).unwrap(),
        name: "unit".into(),
        request_id: "req-3".into(),
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(SpawnManagerMsg::HandleRequest {
            request,
            reply: tx,
        })
        .expect("send failed");

    let result = rx.await.expect("reply dropped");
    assert!(result.is_ok());

    actor_ref.stop(None);
}

// ---------------------------------------------------------------------------
// NA2: WatchManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na2_watch_manager_actor_watch_and_notify() {
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, WatchManagerActor, ())
            .await
            .expect("spawn failed");

    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 10 };

    // Watch
    actor_ref
        .cast(WatchManagerMsg::Watch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    // Check count
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(WatchManagerMsg::GetWatchedCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 1);

    // Terminate
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(WatchManagerMsg::OnTerminated {
            terminated: target.clone(),
            reply: tx,
        })
        .expect("send failed");

    let notifications = rx.await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].terminated, target);
    assert_eq!(notifications[0].watcher, watcher);

    // Count should be 0 after termination
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(WatchManagerMsg::GetWatchedCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 0);

    actor_ref.stop(None);
}

#[tokio::test]
async fn na2_watch_manager_actor_unwatch() {
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, WatchManagerActor, ())
            .await
            .expect("spawn failed");

    let target = ActorId { node: NodeId("n1".into()), local: 1 };
    let watcher = ActorId { node: NodeId("n2".into()), local: 10 };

    actor_ref
        .cast(WatchManagerMsg::Watch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    actor_ref
        .cast(WatchManagerMsg::Unwatch {
            target: target.clone(),
            watcher: watcher.clone(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(WatchManagerMsg::GetWatchedCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 0);

    actor_ref.stop(None);
}

// ---------------------------------------------------------------------------
// NA3: CancelManagerActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na3_cancel_manager_actor_register_and_cancel() {
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, CancelManagerActor, ())
            .await
            .expect("spawn failed");

    let token = CancellationToken::new();
    let token_clone = token.clone();

    actor_ref
        .cast(CancelManagerMsg::Register {
            request_id: "req-1".into(),
            token,
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    // Verify active count
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(CancelManagerMsg::GetActiveCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 1);

    // Cancel
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(CancelManagerMsg::Cancel {
            request_id: "req-1".into(),
            reply: tx,
        })
        .expect("send failed");

    assert!(matches!(rx.await.unwrap(), CancelResponse::Acknowledged));
    assert!(token_clone.is_cancelled());

    // Active count should be 0
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(CancelManagerMsg::GetActiveCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 0);

    actor_ref.stop(None);
}

#[tokio::test]
async fn na3_cancel_manager_actor_complete_cleanup() {
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, CancelManagerActor, ())
            .await
            .expect("spawn failed");

    actor_ref
        .cast(CancelManagerMsg::Register {
            request_id: "req-cleanup".into(),
            token: CancellationToken::new(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    actor_ref
        .cast(CancelManagerMsg::Complete {
            request_id: "req-cleanup".into(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(CancelManagerMsg::GetActiveCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 0);

    actor_ref.stop(None);
}

// ---------------------------------------------------------------------------
// NA4: NodeDirectoryActor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na4_node_directory_actor_connect_disconnect() {
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, NodeDirectoryActor, ())
            .await
            .expect("spawn failed");

    let peer = NodeId("peer-1".into());

    actor_ref
        .cast(NodeDirectoryMsg::ConnectPeer {
            peer_id: peer.clone(),
            address: Some("10.0.0.1:4697".into()),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    // Check connected
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(NodeDirectoryMsg::IsConnected {
            peer_id: peer.clone(),
            reply: tx,
        })
        .expect("send failed");
    assert!(rx.await.unwrap());

    // Check counts
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(NodeDirectoryMsg::GetPeerCount { reply: tx })
        .expect("send failed");
    assert_eq!(rx.await.unwrap(), 1);

    // Disconnect
    actor_ref
        .cast(NodeDirectoryMsg::DisconnectPeer {
            peer_id: peer.clone(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(NodeDirectoryMsg::IsConnected {
            peer_id: peer,
            reply: tx,
        })
        .expect("send failed");
    assert!(!rx.await.unwrap());

    actor_ref.stop(None);
}

#[tokio::test]
async fn na4_node_directory_actor_reconnect_preserves_address() {
    let (actor_ref, _handle) =
        ractor::Actor::spawn(None, NodeDirectoryActor, ())
            .await
            .expect("spawn failed");

    let peer = NodeId("peer-1".into());

    // Connect with address
    actor_ref
        .cast(NodeDirectoryMsg::ConnectPeer {
            peer_id: peer.clone(),
            address: Some("10.0.0.1:4697".into()),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    // Disconnect
    actor_ref
        .cast(NodeDirectoryMsg::DisconnectPeer {
            peer_id: peer.clone(),
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    // Reconnect without address
    actor_ref
        .cast(NodeDirectoryMsg::ConnectPeer {
            peer_id: peer.clone(),
            address: None,
        })
        .expect("send failed");

    tokio::task::yield_now().await;

    // Should still be connected with preserved address
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(NodeDirectoryMsg::IsConnected {
            peer_id: peer.clone(),
            reply: tx,
        })
        .expect("send failed");
    assert!(rx.await.unwrap());

    // Verify address was preserved
    let (tx, rx) = tokio::sync::oneshot::channel();
    actor_ref
        .cast(NodeDirectoryMsg::GetPeerInfo {
            peer_id: peer,
            reply: tx,
        })
        .expect("send failed");
    let info = rx.await.unwrap().expect("peer should exist");
    assert_eq!(info.address.as_deref(), Some("10.0.0.1:4697"),
        "address should be preserved on reconnect without explicit address");

    actor_ref.stop(None);
}
