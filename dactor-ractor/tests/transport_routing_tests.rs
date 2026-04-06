//! Tests for NA10: transport routing in the ractor adapter.
//!
//! Validates that RactorRuntime's SystemMessageRouter implementation correctly
//! deserializes WireEnvelopes and routes them to native system actor mailboxes.

use dactor::interceptor::SendMode;
use dactor::node::{ActorId, NodeId};
use dactor::remote::{SerializationError, WireEnvelope, WireHeaders};
use dactor::system_actors::*;
use dactor::system_router::{RoutingOutcome, SystemMessageRouter};

use dactor_ractor::RactorRuntime;

/// Build a WireEnvelope with the given message_type and body.
fn make_envelope(message_type: &str, body: Vec<u8>) -> WireEnvelope {
    WireEnvelope {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 0,
        },
        target_name: "system".into(),
        message_type: message_type.into(),
        send_mode: SendMode::Tell,
        headers: WireHeaders::new(),
        body,
        request_id: None,
        version: None,
    }
}

/// Create a RactorRuntime with system actors started and a factory registered.
async fn setup_runtime() -> RactorRuntime {
    let mut runtime = RactorRuntime::with_node_id(NodeId("test-node".into()));
    runtime.start_system_actors().await;

    // Register a factory via the native actor
    let refs = runtime.system_actor_refs().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    refs.spawn_manager
        .cast(dactor_ractor::system_actors::SpawnManagerMsg::RegisterFactory {
            type_name: "test::Widget".into(),
            factory: Box::new(|bytes: &[u8]| {
                let val: i32 = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(e.to_string()))?;
                Ok(Box::new(val))
            }),
            reply: tx,
        })
        .expect("register factory");
    rx.await.expect("factory registered");

    runtime
}

// ---------------------------------------------------------------------------
// SpawnRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_route_spawn_request_success() {
    let runtime = setup_runtime().await;

    let request = SpawnRequest {
        type_name: "test::Widget".into(),
        args_bytes: serde_json::to_vec(&99i32).unwrap(),
        name: "widget-1".into(),
        request_id: "req-1".into(),
    };
    let body = dactor::proto::encode_spawn_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_SPAWN, body);

    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed");

    match outcome {
        RoutingOutcome::SpawnCompleted { request_id, actor_id } => {
            assert_eq!(request_id, "req-1");
            assert_eq!(actor_id.node, NodeId("test-node".into()));
        }
        other => panic!("expected SpawnCompleted, got {other:?}"),
    }
}

#[tokio::test]
async fn na10_route_spawn_request_unknown_type() {
    let runtime = setup_runtime().await;

    let request = SpawnRequest {
        type_name: "unknown::Type".into(),
        args_bytes: vec![],
        name: "fail".into(),
        request_id: "req-fail".into(),
    };
    let body = dactor::proto::encode_spawn_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_SPAWN, body);

    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed (spawn failure is an outcome, not error)");

    assert!(matches!(outcome, RoutingOutcome::SpawnFailed { .. }));
}

// ---------------------------------------------------------------------------
// WatchRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_route_watch_request() {
    let runtime = setup_runtime().await;

    let request = WatchRequest {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        watcher: ActorId {
            node: NodeId("remote-node".into()),
            local: 10,
        },
    };
    let body = dactor::proto::encode_watch_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_WATCH, body);

    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed");

    assert!(matches!(outcome, RoutingOutcome::Acknowledged));

    // Verify the watch was registered by querying the watch manager
    let refs = runtime.system_actor_refs().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    refs.watch_manager
        .cast(dactor_ractor::system_actors::WatchManagerMsg::GetWatchedCount { reply: tx })
        .unwrap();
    let count = rx.await.unwrap();
    assert_eq!(count, 1);
}

// ---------------------------------------------------------------------------
// UnwatchRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_route_unwatch_request() {
    let runtime = setup_runtime().await;

    let target = ActorId {
        node: NodeId("test-node".into()),
        local: 1,
    };
    let watcher = ActorId {
        node: NodeId("remote-node".into()),
        local: 10,
    };

    // First watch, then unwatch
    let watch_body = dactor::proto::encode_watch_request(&WatchRequest {
        target: target.clone(),
        watcher: watcher.clone(),
    });
    runtime
        .route_system_envelope(
            make_envelope(SYSTEM_MSG_TYPE_WATCH, watch_body),
        )
        .await
        .unwrap();

    let unwatch_body = dactor::proto::encode_unwatch_request(&UnwatchRequest {
        target: target.clone(),
        watcher: watcher.clone(),
    });
    let outcome = runtime
        .route_system_envelope(
            make_envelope(SYSTEM_MSG_TYPE_UNWATCH, unwatch_body),
        )
        .await
        .unwrap();

    assert!(matches!(outcome, RoutingOutcome::Acknowledged));

    // Verify the watch was removed
    let refs = runtime.system_actor_refs().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    refs.watch_manager
        .cast(dactor_ractor::system_actors::WatchManagerMsg::GetWatchedCount { reply: tx })
        .unwrap();
    let count = rx.await.unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// CancelRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_route_cancel_not_found() {
    let runtime = setup_runtime().await;

    let request = CancelRequest {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        request_id: Some("nonexistent".into()),
    };
    let body = dactor::proto::encode_cancel_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CANCEL, body);

    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed");

    assert!(matches!(outcome, RoutingOutcome::CancelNotFound { .. }));
}

#[tokio::test]
async fn na10_route_cancel_acknowledged() {
    let runtime = setup_runtime().await;

    // Register a cancellation token first
    let token = tokio_util::sync::CancellationToken::new();
    let token_check = token.clone();
    let refs = runtime.system_actor_refs().unwrap();
    refs.cancel_manager
        .cast(dactor_ractor::system_actors::CancelManagerMsg::Register {
            request_id: "req-42".into(),
            token,
        })
        .unwrap();

    // Confirm registration via request-response (no sleep needed)
    let (tx, rx) = tokio::sync::oneshot::channel();
    refs.cancel_manager
        .cast(dactor_ractor::system_actors::CancelManagerMsg::GetActiveCount { reply: tx })
        .unwrap();
    assert_eq!(rx.await.unwrap(), 1);

    let request = CancelRequest {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        request_id: Some("req-42".into()),
    };
    let body = dactor::proto::encode_cancel_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CANCEL, body);

    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed");

    assert!(matches!(outcome, RoutingOutcome::CancelAcknowledged));
    assert!(token_check.is_cancelled());
}

// ---------------------------------------------------------------------------
// ConnectPeer / DisconnectPeer routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_route_connect_disconnect_peer() {
    let runtime = setup_runtime().await;

    // Connect peer
    let body = dactor::proto::encode_connect_peer(&NodeId("peer-node-1".into()), Some("10.0.0.1:4697"));
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CONNECT_PEER, body);

    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed");
    assert!(matches!(outcome, RoutingOutcome::Acknowledged));

    // Verify peer was registered (no sleep needed — cast ordering is FIFO)
    let refs = runtime.system_actor_refs().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    refs.node_directory
        .cast(dactor_ractor::system_actors::NodeDirectoryMsg::IsConnected {
            peer_id: NodeId("peer-node-1".into()),
            reply: tx,
        })
        .unwrap();
    let connected = rx.await.unwrap();
    assert!(connected);

    // Disconnect peer
    let body = dactor::proto::encode_disconnect_peer(&NodeId("peer-node-1".into()));
    let envelope = make_envelope(SYSTEM_MSG_TYPE_DISCONNECT_PEER, body);
    let outcome = runtime
        .route_system_envelope(envelope)
        .await
        .expect("routing should succeed");
    assert!(matches!(outcome, RoutingOutcome::Acknowledged));

    // Verify peer was disconnected
    let (tx, rx) = tokio::sync::oneshot::channel();
    refs.node_directory
        .cast(dactor_ractor::system_actors::NodeDirectoryMsg::IsConnected {
            peer_id: NodeId("peer-node-1".into()),
            reply: tx,
        })
        .unwrap();
    let connected = rx.await.unwrap();
    assert!(!connected);
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_route_unknown_message_type_rejected() {
    let runtime = setup_runtime().await;

    let envelope = make_envelope("myapp::FooMessage", b"hello".to_vec());

    let result = runtime.route_system_envelope(envelope).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("unknown system message type"));
}

#[tokio::test]
async fn na10_route_cancel_missing_request_id() {
    let runtime = setup_runtime().await;

    let request = CancelRequest {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        request_id: None,
    };
    let body = dactor::proto::encode_cancel_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CANCEL, body);

    let result = runtime.route_system_envelope(envelope).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("missing request_id"));
}

#[tokio::test]
async fn na10_route_connect_invalid_utf8() {
    let runtime = setup_runtime().await;

    let envelope = make_envelope(SYSTEM_MSG_TYPE_CONNECT_PEER, vec![0xFF, 0xFE]);

    let result = runtime.route_system_envelope(envelope).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn na10_route_without_system_actors_fails() {
    let runtime = RactorRuntime::new(); // NOT started

    let request = SpawnRequest {
        type_name: "test::Foo".into(),
        args_bytes: vec![],
        name: "foo".into(),
        request_id: "req-1".into(),
    };
    let body = dactor::proto::encode_spawn_request(&request);
    let envelope = make_envelope(SYSTEM_MSG_TYPE_SPAWN, body);

    let result = runtime.route_system_envelope(envelope).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("system actors not started"));
}
