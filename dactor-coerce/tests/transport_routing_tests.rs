//! Tests for NA10: transport routing in the coerce adapter.
//!
//! Validates that CoerceRuntime's SystemMessageRouter implementation correctly
//! deserializes WireEnvelopes and routes them to native system actor mailboxes.

use dactor::interceptor::SendMode;
use dactor::node::{ActorId, NodeId};
use dactor::remote::{MessageSerializer, SerializationError, WireEnvelope, WireHeaders};
use dactor::system_actors::*;
use dactor::system_router::{RoutingOutcome, SystemMessageRouter};

use dactor_coerce::CoerceRuntime;

// ---------------------------------------------------------------------------
// Test serializer
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

async fn setup_runtime() -> CoerceRuntime {
    let mut runtime = CoerceRuntime::new();
    runtime.start_system_actors();

    // Register a factory via the native actor
    let refs = runtime.system_actor_refs().unwrap();
    refs.spawn_manager
        .send(dactor_coerce::system_actors::RegisterFactory {
            type_name: "test::Widget".into(),
            factory: Box::new(|bytes: &[u8]| {
                let val: i32 = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(e.to_string()))?;
                Ok(Box::new(val))
            }),
        })
        .await
        .expect("register factory");

    runtime
}

// ---------------------------------------------------------------------------
// SpawnRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_coerce_route_spawn_request_success() {
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
        RoutingOutcome::SpawnCompleted { actor_id } => {
            assert_eq!(actor_id.node.0, "coerce-node");
        }
        other => panic!("expected SpawnCompleted, got {other:?}"),
    }
}

#[tokio::test]
async fn na10_coerce_route_spawn_request_unknown_type() {
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
        .expect("routing should succeed");

    assert!(matches!(outcome, RoutingOutcome::SpawnFailed { .. }));
}

// ---------------------------------------------------------------------------
// WatchRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_coerce_route_watch_request() {
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

    // Verify the watch was registered
    let refs = runtime.system_actor_refs().unwrap();
    let count = refs
        .watch_manager
        .send(dactor_coerce::system_actors::GetWatchedCount)
        .await
        .expect("send");
    assert_eq!(count, 1);
}

// ---------------------------------------------------------------------------
// CancelRequest routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_coerce_route_cancel_not_found() {
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

// ---------------------------------------------------------------------------
// ConnectPeer / DisconnectPeer routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_coerce_route_connect_disconnect_peer() {
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

    // Small delay for async processing in coerce
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify peer was registered
    let refs = runtime.system_actor_refs().unwrap();
    let connected = refs
        .node_directory
        .send(dactor_coerce::system_actors::IsConnected(NodeId(
            "peer-node-1".into(),
        )))
        .await
        .expect("send");
    assert!(connected);

    // Disconnect peer
    let envelope = make_envelope(SYSTEM_MSG_TYPE_DISCONNECT_PEER, b"peer-node-1".to_vec());
    let outcome = runtime
        .route_system_envelope(envelope, &serializer)
        .await
        .expect("routing should succeed");
    assert!(matches!(outcome, RoutingOutcome::Acknowledged));

    // Small delay for async processing
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let connected = refs
        .node_directory
        .send(dactor_coerce::system_actors::IsConnected(NodeId(
            "peer-node-1".into(),
        )))
        .await
        .expect("send");
    assert!(!connected);
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn na10_coerce_route_unknown_message_type_rejected() {
    let runtime = setup_runtime().await;
    let serializer = TestSerializer;

    let envelope = make_envelope("myapp::FooMessage", b"hello".to_vec());

    let result = runtime.route_system_envelope(envelope, &serializer).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("unknown system message type"));
}

#[tokio::test]
async fn na10_coerce_route_without_system_actors_fails() {
    let runtime = CoerceRuntime::new(); // NOT started
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
