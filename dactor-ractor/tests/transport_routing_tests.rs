//! Tests for NA10: transport routing in the ractor adapter.
//!
//! Validates that RactorRuntime's SystemMessageRouter implementation correctly
//! deserializes WireEnvelopes and routes them to native system actor mailboxes.

use dactor::interceptor::SendMode;
use dactor::node::{ActorId, NodeId};
use dactor::remote::{MessageSerializer, SerializationError, WireEnvelope, WireHeaders};
use dactor::system_actors::*;
use dactor::system_router::{RoutingOutcome, SystemMessageRouter};

use dactor_ractor::RactorRuntime;

// ---------------------------------------------------------------------------
// Test serializer — uses serde_json for SpawnRequest / WatchRequest / etc.
// ---------------------------------------------------------------------------

struct TestSerializer;

impl MessageSerializer for TestSerializer {
    fn name(&self) -> &'static str {
        "test-json"
    }

    fn serialize(&self, value: &dyn std::any::Any) -> Result<Vec<u8>, SerializationError> {
        if let Some(v) = value.downcast_ref::<SpawnRequest>() {
            serde_json::to_vec(v).map_err(|e| SerializationError::new(e.to_string()))
        } else if let Some(v) = value.downcast_ref::<WatchRequest>() {
            serde_json::to_vec(v).map_err(|e| SerializationError::new(e.to_string()))
        } else if let Some(v) = value.downcast_ref::<UnwatchRequest>() {
            serde_json::to_vec(v).map_err(|e| SerializationError::new(e.to_string()))
        } else if let Some(v) = value.downcast_ref::<CancelRequest>() {
            serde_json::to_vec(v).map_err(|e| SerializationError::new(e.to_string()))
        } else {
            Err(SerializationError::new("unsupported type"))
        }
    }

    fn deserialize(
        &self,
        bytes: &[u8],
        type_name: &str,
    ) -> Result<Box<dyn std::any::Any + Send>, SerializationError> {
        match type_name {
            SYSTEM_MSG_TYPE_SPAWN => {
                let v: SpawnRequest = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(e.to_string()))?;
                Ok(Box::new(v))
            }
            SYSTEM_MSG_TYPE_WATCH => {
                let v: WatchRequest = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(e.to_string()))?;
                Ok(Box::new(v))
            }
            SYSTEM_MSG_TYPE_UNWATCH => {
                let v: UnwatchRequest = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(e.to_string()))?;
                Ok(Box::new(v))
            }
            SYSTEM_MSG_TYPE_CANCEL => {
                let v: CancelRequest = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(e.to_string()))?;
                Ok(Box::new(v))
            }
            _ => Err(SerializationError::new(format!(
                "unknown type: {type_name}"
            ))),
        }
    }
}

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
    let serializer = TestSerializer;

    let request = SpawnRequest {
        type_name: "test::Widget".into(),
        args_bytes: serde_json::to_vec(&99i32).unwrap(),
        name: "widget-1".into(),
        request_id: "req-1".into(),
    };
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_SPAWN, body);

    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
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
    let serializer = TestSerializer;

    let request = SpawnRequest {
        type_name: "unknown::Type".into(),
        args_bytes: vec![],
        name: "fail".into(),
        request_id: "req-fail".into(),
    };
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_SPAWN, body);

    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
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
    let serializer = TestSerializer;

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
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_WATCH, body);

    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
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
    let serializer = TestSerializer;

    let target = ActorId {
        node: NodeId("test-node".into()),
        local: 1,
    };
    let watcher = ActorId {
        node: NodeId("remote-node".into()),
        local: 10,
    };

    // First watch, then unwatch
    let watch_body = serializer
        .serialize(&WatchRequest {
            target: target.clone(),
            watcher: watcher.clone(),
        } as &dyn std::any::Any)
        .unwrap();
    runtime
        .route_system_envelope(
            make_envelope(SYSTEM_MSG_TYPE_WATCH, watch_body),
            &serializer,
        )
        .await
        .unwrap();

    let unwatch_body = serializer
        .serialize(&UnwatchRequest {
            target: target.clone(),
            watcher: watcher.clone(),
        } as &dyn std::any::Any)
        .unwrap();
    let outcome = runtime
        .route_system_envelope(
            make_envelope(SYSTEM_MSG_TYPE_UNWATCH, unwatch_body),
            &serializer,
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
    let serializer = TestSerializer;

    let request = CancelRequest {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        request_id: Some("nonexistent".into()),
    };
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CANCEL, body);

    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
        .await
        .expect("routing should succeed");

    assert!(matches!(outcome, RoutingOutcome::CancelNotFound { .. }));
}

#[tokio::test]
async fn na10_route_cancel_acknowledged() {
    let runtime = setup_runtime().await;
    let serializer = TestSerializer;

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
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CANCEL, body);

    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
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
    let serializer = TestSerializer;

    // Connect peer
    let mut headers = WireHeaders::new();
    headers.insert("address".into(), b"10.0.0.1:4697".to_vec());
    let mut envelope = make_envelope(SYSTEM_MSG_TYPE_CONNECT_PEER, b"peer-node-1".to_vec());
    envelope.headers = headers;

    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
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
    let envelope = make_envelope(SYSTEM_MSG_TYPE_DISCONNECT_PEER, b"peer-node-1".to_vec());
    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
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
    let serializer = TestSerializer;

    let envelope = make_envelope("myapp::FooMessage", b"hello".to_vec());

    let result = runtime.route_system_envelope(envelope, &serializer).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("unknown system message type"));
}

#[tokio::test]
async fn na10_route_cancel_missing_request_id() {
    let runtime = setup_runtime().await;
    let serializer = TestSerializer;

    let request = CancelRequest {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        request_id: None,
    };
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_CANCEL, body);

    let result = runtime.route_system_envelope(envelope, &serializer).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("missing request_id"));
}

#[tokio::test]
async fn na10_route_connect_invalid_utf8() {
    let runtime = setup_runtime().await;
    let serializer = TestSerializer;

    let envelope = make_envelope(SYSTEM_MSG_TYPE_CONNECT_PEER, vec![0xFF, 0xFE]);

    let result = runtime.route_system_envelope(envelope, &serializer).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("invalid ConnectPeer body"));
}

#[tokio::test]
async fn na10_route_without_system_actors_fails() {
    let runtime = RactorRuntime::new(); // NOT started
    let serializer = TestSerializer;

    let request = SpawnRequest {
        type_name: "test::Foo".into(),
        args_bytes: vec![],
        name: "foo".into(),
        request_id: "req-1".into(),
    };
    let body = serializer.serialize(&request as &dyn std::any::Any).unwrap();
    let envelope = make_envelope(SYSTEM_MSG_TYPE_SPAWN, body);

    let result = runtime.route_system_envelope(envelope, &serializer).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("system actors not started"));
}
