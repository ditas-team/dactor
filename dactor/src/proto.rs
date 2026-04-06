//! Protobuf serialization for system messages and wire envelope framing.
//!
//! System messages (spawn, watch, cancel, peer management) use a fixed protobuf
//! wire format for compact encoding and native schema evolution. Application
//! messages remain pluggable via [`MessageSerializer`](crate::remote::MessageSerializer).
//!
//! # Encoding system message bodies
//!
//! ```ignore
//! use dactor::proto;
//! let bytes = proto::encode_spawn_request(&request);
//! let decoded = proto::decode_spawn_request(&bytes)?;
//! ```
//!
//! # WireEnvelope framing
//!
//! ```ignore
//! let frame = proto::encode_wire_envelope(&envelope);
//! let envelope = proto::decode_wire_envelope(&frame)?;
//! ```

use prost::Message;

use crate::interceptor::SendMode;
use crate::node::{ActorId, NodeId};
use crate::remote::{SerializationError, WireEnvelope, WireHeaders};
use crate::system_actors::{
    CancelRequest, CancelResponse, SpawnRequest, SpawnResponse, UnwatchRequest, WatchNotification,
    WatchRequest,
};

// Include prost-generated types from build.rs.
// These are internal wire-format types — only the typed encode/decode
// helpers are part of the public API.
#[allow(clippy::enum_variant_names)]
pub(crate) mod generated {
    include!(concat!(env!("OUT_DIR"), "/dactor.system.rs"));
}

use generated::*;

// ---------------------------------------------------------------------------
// Size limit
// ---------------------------------------------------------------------------

/// Maximum allowed size for a system message protobuf payload (1 MB).
///
/// Enforced before decoding to prevent allocation-based DoS from untrusted
/// peers. Application messages are not affected — they use separate
/// [`MessageSerializer`](crate::remote::MessageSerializer) implementations.
pub const MAX_SYSTEM_MSG_SIZE: usize = 1024 * 1024;

/// Maximum allowed size for a WireEnvelope protobuf frame (4 MB).
///
/// Larger than system message limit because the body may contain a serialized
/// application message of arbitrary size.
pub const MAX_WIRE_ENVELOPE_SIZE: usize = 4 * 1024 * 1024;

fn check_size(bytes: &[u8], limit: usize, what: &str) -> Result<(), SerializationError> {
    if bytes.len() > limit {
        Err(SerializationError::new(format!(
            "{what}: payload too large ({} bytes, max {limit})",
            bytes.len()
        )))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ActorId conversions
// ---------------------------------------------------------------------------

fn actor_id_to_proto(id: &ActorId) -> ActorIdProto {
    ActorIdProto {
        node_id: id.node.0.clone(),
        local: id.local,
    }
}

fn actor_id_from_proto(proto: &ActorIdProto) -> Result<ActorId, SerializationError> {
    if proto.node_id.is_empty() {
        return Err(SerializationError::new("ActorId: node_id must not be empty"));
    }
    Ok(ActorId {
        node: NodeId(proto.node_id.clone()),
        local: proto.local,
    })
}

fn require_actor_id(
    field: &Option<ActorIdProto>,
    name: &str,
) -> Result<ActorId, SerializationError> {
    let proto = field
        .as_ref()
        .ok_or_else(|| SerializationError::new(format!("missing required field: {name}")))?;
    actor_id_from_proto(proto)
}

// ---------------------------------------------------------------------------
// SpawnRequest
// ---------------------------------------------------------------------------

/// Encode a [`SpawnRequest`] to protobuf bytes.
pub fn encode_spawn_request(req: SpawnRequest) -> Vec<u8> {
    let proto = SpawnRequestProto {
        type_name: req.type_name,
        args_bytes: req.args_bytes,
        name: req.name,
        request_id: req.request_id,
    };
    proto.encode_to_vec()
}

/// Decode a [`SpawnRequest`] from protobuf bytes.
pub fn decode_spawn_request(bytes: &[u8]) -> Result<SpawnRequest, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "SpawnRequest")?;
    let proto = SpawnRequestProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode SpawnRequest: {e}")))?;
    if proto.type_name.is_empty() {
        return Err(SerializationError::new("SpawnRequest: type_name must not be empty"));
    }
    if proto.request_id.is_empty() {
        return Err(SerializationError::new("SpawnRequest: request_id must not be empty"));
    }
    Ok(SpawnRequest {
        type_name: proto.type_name,
        args_bytes: proto.args_bytes,
        name: proto.name,
        request_id: proto.request_id,
    })
}

// ---------------------------------------------------------------------------
// SpawnResponse
// ---------------------------------------------------------------------------

/// Encode a [`SpawnResponse`] to protobuf bytes.
pub fn encode_spawn_response(resp: SpawnResponse) -> Vec<u8> {
    let proto = match resp {
        SpawnResponse::Success {
            request_id,
            actor_id,
        } => SpawnResponseProto {
            result: Some(spawn_response_proto::Result::Success(SpawnSuccessProto {
                request_id,
                actor_id: Some(actor_id_to_proto(&actor_id)),
            })),
        },
        SpawnResponse::Failure { request_id, error } => SpawnResponseProto {
            result: Some(spawn_response_proto::Result::Failure(SpawnFailureProto {
                request_id,
                error,
            })),
        },
    };
    proto.encode_to_vec()
}

/// Decode a [`SpawnResponse`] from protobuf bytes.
pub fn decode_spawn_response(bytes: &[u8]) -> Result<SpawnResponse, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "SpawnResponse")?;
    let proto = SpawnResponseProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode SpawnResponse: {e}")))?;
    match proto.result {
        Some(spawn_response_proto::Result::Success(s)) => Ok(SpawnResponse::Success {
            request_id: s.request_id,
            actor_id: require_actor_id(&s.actor_id, "SpawnSuccess.actor_id")?,
        }),
        Some(spawn_response_proto::Result::Failure(f)) => Ok(SpawnResponse::Failure {
            request_id: f.request_id,
            error: f.error,
        }),
        None => Err(SerializationError::new(
            "SpawnResponse: missing oneof result",
        )),
    }
}

// ---------------------------------------------------------------------------
// WatchRequest
// ---------------------------------------------------------------------------

/// Encode a [`WatchRequest`] to protobuf bytes.
pub fn encode_watch_request(req: WatchRequest) -> Vec<u8> {
    let proto = WatchRequestProto {
        target: Some(actor_id_to_proto(&req.target)),
        watcher: Some(actor_id_to_proto(&req.watcher)),
    };
    proto.encode_to_vec()
}

/// Decode a [`WatchRequest`] from protobuf bytes.
pub fn decode_watch_request(bytes: &[u8]) -> Result<WatchRequest, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "WatchRequest")?;
    let proto = WatchRequestProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode WatchRequest: {e}")))?;
    Ok(WatchRequest {
        target: require_actor_id(&proto.target, "WatchRequest.target")?,
        watcher: require_actor_id(&proto.watcher, "WatchRequest.watcher")?,
    })
}

// ---------------------------------------------------------------------------
// UnwatchRequest
// ---------------------------------------------------------------------------

/// Encode an [`UnwatchRequest`] to protobuf bytes.
pub fn encode_unwatch_request(req: UnwatchRequest) -> Vec<u8> {
    let proto = UnwatchRequestProto {
        target: Some(actor_id_to_proto(&req.target)),
        watcher: Some(actor_id_to_proto(&req.watcher)),
    };
    proto.encode_to_vec()
}

/// Decode an [`UnwatchRequest`] from protobuf bytes.
pub fn decode_unwatch_request(bytes: &[u8]) -> Result<UnwatchRequest, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "UnwatchRequest")?;
    let proto = UnwatchRequestProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode UnwatchRequest: {e}")))?;
    Ok(UnwatchRequest {
        target: require_actor_id(&proto.target, "UnwatchRequest.target")?,
        watcher: require_actor_id(&proto.watcher, "UnwatchRequest.watcher")?,
    })
}

// ---------------------------------------------------------------------------
// WatchNotification
// ---------------------------------------------------------------------------

/// Encode a [`WatchNotification`] to protobuf bytes.
pub fn encode_watch_notification(notif: WatchNotification) -> Vec<u8> {
    let proto = WatchNotificationProto {
        terminated: Some(actor_id_to_proto(&notif.terminated)),
        watcher: Some(actor_id_to_proto(&notif.watcher)),
    };
    proto.encode_to_vec()
}

/// Decode a [`WatchNotification`] from protobuf bytes.
pub fn decode_watch_notification(
    bytes: &[u8],
) -> Result<WatchNotification, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "WatchNotification")?;
    let proto = WatchNotificationProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode WatchNotification: {e}")))?;
    Ok(WatchNotification {
        terminated: require_actor_id(&proto.terminated, "WatchNotification.terminated")?,
        watcher: require_actor_id(&proto.watcher, "WatchNotification.watcher")?,
    })
}

// ---------------------------------------------------------------------------
// CancelRequest
// ---------------------------------------------------------------------------

/// Encode a [`CancelRequest`] to protobuf bytes.
pub fn encode_cancel_request(req: CancelRequest) -> Vec<u8> {
    let proto = CancelRequestProto {
        target: Some(actor_id_to_proto(&req.target)),
        request_id: req.request_id,
    };
    proto.encode_to_vec()
}

/// Decode a [`CancelRequest`] from protobuf bytes.
pub fn decode_cancel_request(bytes: &[u8]) -> Result<CancelRequest, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "CancelRequest")?;
    let proto = CancelRequestProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode CancelRequest: {e}")))?;
    Ok(CancelRequest {
        target: require_actor_id(&proto.target, "CancelRequest.target")?,
        request_id: proto.request_id,
    })
}

// ---------------------------------------------------------------------------
// CancelResponse
// ---------------------------------------------------------------------------

/// Encode a [`CancelResponse`] to protobuf bytes.
pub fn encode_cancel_response(resp: CancelResponse) -> Vec<u8> {
    let proto = match resp {
        CancelResponse::Acknowledged => CancelResponseProto {
            result: Some(cancel_response_proto::Result::Acknowledged(
                CancelAcknowledgedProto {},
            )),
        },
        CancelResponse::NotFound { reason } => CancelResponseProto {
            result: Some(cancel_response_proto::Result::NotFound(
                CancelNotFoundProto {
                    reason,
                },
            )),
        },
    };
    proto.encode_to_vec()
}

/// Decode a [`CancelResponse`] from protobuf bytes.
pub fn decode_cancel_response(bytes: &[u8]) -> Result<CancelResponse, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "CancelResponse")?;
    let proto = CancelResponseProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode CancelResponse: {e}")))?;
    match proto.result {
        Some(cancel_response_proto::Result::Acknowledged(_)) => {
            Ok(CancelResponse::Acknowledged)
        }
        Some(cancel_response_proto::Result::NotFound(nf)) => {
            Ok(CancelResponse::NotFound { reason: nf.reason })
        }
        None => Err(SerializationError::new(
            "CancelResponse: missing oneof result",
        )),
    }
}

// ---------------------------------------------------------------------------
// ConnectPeer / DisconnectPeer
// ---------------------------------------------------------------------------

/// Encode a connect-peer message to protobuf bytes.
pub fn encode_connect_peer(node_id: &NodeId, address: Option<&str>) -> Vec<u8> {
    let proto = ConnectPeerProto {
        node_id: node_id.0.clone(),
        address: address.map(|s| s.to_string()),
    };
    proto.encode_to_vec()
}

/// Decode a connect-peer message from protobuf bytes.
///
/// Returns `(node_id, optional_address)`.
pub fn decode_connect_peer(
    bytes: &[u8],
) -> Result<(NodeId, Option<String>), SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "ConnectPeer")?;
    let proto = ConnectPeerProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode ConnectPeer: {e}")))?;
    if proto.node_id.is_empty() {
        return Err(SerializationError::new("ConnectPeer: node_id must not be empty"));
    }
    Ok((NodeId(proto.node_id), proto.address))
}

/// Encode a disconnect-peer message to protobuf bytes.
pub fn encode_disconnect_peer(node_id: &NodeId) -> Vec<u8> {
    let proto = DisconnectPeerProto {
        node_id: node_id.0.clone(),
    };
    proto.encode_to_vec()
}

/// Decode a disconnect-peer message from protobuf bytes.
pub fn decode_disconnect_peer(bytes: &[u8]) -> Result<NodeId, SerializationError> {
    check_size(bytes, MAX_SYSTEM_MSG_SIZE, "DisconnectPeer")?;
    let proto = DisconnectPeerProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode DisconnectPeer: {e}")))?;
    if proto.node_id.is_empty() {
        return Err(SerializationError::new("DisconnectPeer: node_id must not be empty"));
    }
    Ok(NodeId(proto.node_id))
}

// ---------------------------------------------------------------------------
// SendMode conversions
// ---------------------------------------------------------------------------

fn send_mode_to_proto(mode: SendMode) -> i32 {
    match mode {
        SendMode::Tell => SendModeProto::SendModeTell as i32,
        SendMode::Ask => SendModeProto::SendModeAsk as i32,
        SendMode::Expand => SendModeProto::SendModeExpand as i32,
        SendMode::Reduce => SendModeProto::SendModeReduce as i32,
        SendMode::Transform => SendModeProto::SendModeTransform as i32,
    }
}

fn send_mode_from_proto(value: i32) -> Result<SendMode, SerializationError> {
    match SendModeProto::try_from(value) {
        Ok(SendModeProto::SendModeTell) => Ok(SendMode::Tell),
        Ok(SendModeProto::SendModeAsk) => Ok(SendMode::Ask),
        Ok(SendModeProto::SendModeExpand) => Ok(SendMode::Expand),
        Ok(SendModeProto::SendModeReduce) => Ok(SendMode::Reduce),
        Ok(SendModeProto::SendModeTransform) => Ok(SendMode::Transform),
        Err(_) => Err(SerializationError::new(format!(
            "unknown SendMode value: {value}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// WireEnvelope framing
// ---------------------------------------------------------------------------

/// Encode a [`WireEnvelope`] to protobuf bytes for transport framing.
pub fn encode_wire_envelope(env: &WireEnvelope) -> Vec<u8> {
    let proto = WireEnvelopeProto {
        target: Some(actor_id_to_proto(&env.target)),
        target_name: env.target_name.clone(),
        message_type: env.message_type.clone(),
        send_mode: send_mode_to_proto(env.send_mode),
        headers: env.headers.entries.clone(),
        body: env.body.clone(),
        request_id: env.request_id.map(|id| id.to_string()),
        version: env.version,
    };
    proto.encode_to_vec()
}

/// Decode a [`WireEnvelope`] from protobuf bytes.
pub fn decode_wire_envelope(bytes: &[u8]) -> Result<WireEnvelope, SerializationError> {
    check_size(bytes, MAX_WIRE_ENVELOPE_SIZE, "WireEnvelope")?;
    let proto = WireEnvelopeProto::decode(bytes)
        .map_err(|e| SerializationError::new(format!("decode WireEnvelope: {e}")))?;
    let target = require_actor_id(&proto.target, "WireEnvelope.target")?;
    let send_mode = send_mode_from_proto(proto.send_mode)?;
    let request_id = proto
        .request_id
        .map(|s| {
            uuid::Uuid::parse_str(&s)
                .map_err(|e| SerializationError::new(format!("invalid request_id UUID: {e}")))
        })
        .transpose()?;
    Ok(WireEnvelope {
        target,
        target_name: proto.target_name,
        message_type: proto.message_type,
        send_mode,
        headers: WireHeaders {
            entries: proto.headers,
        },
        body: proto.body,
        request_id,
        version: proto.version,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{ActorId, NodeId};

    fn test_actor_id() -> ActorId {
        ActorId {
            node: NodeId("test-node".into()),
            local: 42,
        }
    }

    fn test_actor_id2() -> ActorId {
        ActorId {
            node: NodeId("other-node".into()),
            local: 7,
        }
    }

    #[test]
    fn spawn_request_roundtrip() {
        let bytes = encode_spawn_request(SpawnRequest {
            type_name: "myapp::Counter".into(),
            args_bytes: vec![1, 2, 3],
            name: "counter-1".into(),
            request_id: "req-001".into(),
        });
        let decoded = decode_spawn_request(&bytes).unwrap();
        assert_eq!(decoded.type_name, "myapp::Counter");
        assert_eq!(decoded.args_bytes, vec![1, 2, 3]);
        assert_eq!(decoded.name, "counter-1");
        assert_eq!(decoded.request_id, "req-001");
    }

    #[test]
    fn spawn_response_success_roundtrip() {
        let bytes = encode_spawn_response(SpawnResponse::Success {
            request_id: "req-001".into(),
            actor_id: test_actor_id(),
        });
        let decoded = decode_spawn_response(&bytes).unwrap();
        match decoded {
            SpawnResponse::Success {
                request_id,
                actor_id,
            } => {
                assert_eq!(request_id, "req-001");
                assert_eq!(actor_id.local, 42);
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn spawn_response_failure_roundtrip() {
        let bytes = encode_spawn_response(SpawnResponse::Failure {
            request_id: "req-002".into(),
            error: "type not found".into(),
        });
        let decoded = decode_spawn_response(&bytes).unwrap();
        match decoded {
            SpawnResponse::Failure { request_id, error } => {
                assert_eq!(request_id, "req-002");
                assert_eq!(error, "type not found");
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn watch_request_roundtrip() {
        let bytes = encode_watch_request(WatchRequest {
            target: test_actor_id(),
            watcher: test_actor_id2(),
        });
        let decoded = decode_watch_request(&bytes).unwrap();
        assert_eq!(decoded.target.local, 42);
        assert_eq!(decoded.watcher.local, 7);
    }

    #[test]
    fn unwatch_request_roundtrip() {
        let bytes = encode_unwatch_request(UnwatchRequest {
            target: test_actor_id(),
            watcher: test_actor_id2(),
        });
        let decoded = decode_unwatch_request(&bytes).unwrap();
        assert_eq!(decoded.target.local, 42);
        assert_eq!(decoded.watcher.local, 7);
    }

    #[test]
    fn watch_notification_roundtrip() {
        let bytes = encode_watch_notification(WatchNotification {
            terminated: test_actor_id(),
            watcher: test_actor_id2(),
        });
        let decoded = decode_watch_notification(&bytes).unwrap();
        assert_eq!(decoded.terminated.local, 42);
        assert_eq!(decoded.watcher.local, 7);
    }

    #[test]
    fn cancel_request_with_id_roundtrip() {
        let bytes = encode_cancel_request(CancelRequest {
            target: test_actor_id(),
            request_id: Some("req-cancel".into()),
        });
        let decoded = decode_cancel_request(&bytes).unwrap();
        assert_eq!(decoded.target.local, 42);
        assert_eq!(decoded.request_id.as_deref(), Some("req-cancel"));
    }

    #[test]
    fn cancel_request_without_id_roundtrip() {
        let bytes = encode_cancel_request(CancelRequest {
            target: test_actor_id(),
            request_id: None,
        });
        let decoded = decode_cancel_request(&bytes).unwrap();
        assert!(decoded.request_id.is_none());
    }

    #[test]
    fn cancel_response_acknowledged_roundtrip() {
        let bytes = encode_cancel_response(CancelResponse::Acknowledged);
        let decoded = decode_cancel_response(&bytes).unwrap();
        assert!(matches!(decoded, CancelResponse::Acknowledged));
    }

    #[test]
    fn cancel_response_not_found_roundtrip() {
        let bytes = encode_cancel_response(CancelResponse::NotFound {
            reason: "no such request".into(),
        });
        let decoded = decode_cancel_response(&bytes).unwrap();
        match decoded {
            CancelResponse::NotFound { reason } => assert_eq!(reason, "no such request"),
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn connect_peer_roundtrip() {
        let bytes = encode_connect_peer(&NodeId("peer-1".into()), Some("192.168.1.1:9000"));
        let (node_id, address) = decode_connect_peer(&bytes).unwrap();
        assert_eq!(node_id.0, "peer-1");
        assert_eq!(address.as_deref(), Some("192.168.1.1:9000"));
    }

    #[test]
    fn connect_peer_no_address_roundtrip() {
        let bytes = encode_connect_peer(&NodeId("peer-2".into()), None);
        let (node_id, address) = decode_connect_peer(&bytes).unwrap();
        assert_eq!(node_id.0, "peer-2");
        assert!(address.is_none());
    }

    #[test]
    fn disconnect_peer_roundtrip() {
        let bytes = encode_disconnect_peer(&NodeId("peer-3".into()));
        let node_id = decode_disconnect_peer(&bytes).unwrap();
        assert_eq!(node_id.0, "peer-3");
    }

    #[test]
    fn wire_envelope_roundtrip() {
        let mut headers = WireHeaders::new();
        headers.insert("trace-id".into(), b"abc-123".to_vec());

        let original = WireEnvelope {
            target: test_actor_id(),
            target_name: "my-actor".into(),
            message_type: "myapp::Ping".into(),
            send_mode: SendMode::Ask,
            headers,
            body: vec![10, 20, 30],
            request_id: Some(uuid::Uuid::new_v4()),
            version: Some(2),
        };
        let bytes = encode_wire_envelope(&original);
        let decoded = decode_wire_envelope(&bytes).unwrap();
        assert_eq!(decoded.target.local, original.target.local);
        assert_eq!(decoded.target_name, original.target_name);
        assert_eq!(decoded.message_type, original.message_type);
        assert_eq!(decoded.send_mode, SendMode::Ask);
        assert_eq!(decoded.body, original.body);
        assert_eq!(decoded.request_id, original.request_id);
        assert_eq!(decoded.version, Some(2));
        assert_eq!(
            decoded.headers.get("trace-id"),
            Some(b"abc-123".as_slice())
        );
    }

    #[test]
    fn wire_envelope_minimal_roundtrip() {
        let original = WireEnvelope {
            target: test_actor_id(),
            target_name: String::new(),
            message_type: "myapp::Msg".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![],
            request_id: None,
            version: None,
        };
        let bytes = encode_wire_envelope(&original);
        let decoded = decode_wire_envelope(&bytes).unwrap();
        assert_eq!(decoded.send_mode, SendMode::Tell);
        assert!(decoded.request_id.is_none());
        assert!(decoded.version.is_none());
    }

    #[test]
    fn all_send_modes_roundtrip() {
        for mode in [
            SendMode::Tell,
            SendMode::Ask,
            SendMode::Expand,
            SendMode::Reduce,
            SendMode::Transform,
        ] {
            let env = WireEnvelope {
                target: test_actor_id(),
                target_name: String::new(),
                message_type: "test".into(),
                send_mode: mode,
                headers: WireHeaders::new(),
                body: vec![],
                request_id: None,
                version: None,
            };
            let decoded = decode_wire_envelope(&encode_wire_envelope(&env)).unwrap();
            assert_eq!(decoded.send_mode, mode, "SendMode roundtrip failed for {mode:?}");
        }
    }

    #[test]
    fn decode_empty_bytes_returns_error() {
        // Empty bytes are valid protobuf (all fields default) but SpawnRequest
        // will just have empty strings which is technically valid.
        // However, corrupt bytes should fail:
        assert!(decode_spawn_request(&[0xFF, 0xFF, 0xFF]).is_err());
        assert!(decode_watch_request(&[0xFF, 0xFF, 0xFF]).is_err());
        assert!(decode_cancel_request(&[0xFF, 0xFF, 0xFF]).is_err());
        assert!(decode_wire_envelope(&[0xFF, 0xFF, 0xFF]).is_err());
    }

    #[test]
    fn protobuf_is_compact() {
        let proto_bytes = encode_spawn_request(SpawnRequest {
            type_name: "myapp::Counter".into(),
            args_bytes: vec![1, 2, 3],
            name: "counter-1".into(),
            request_id: "req-001".into(),
        });
        // Protobuf should be significantly smaller than JSON
        assert!(
            proto_bytes.len() < 60,
            "protobuf should be compact, got {} bytes",
            proto_bytes.len()
        );
    }

    // --- Review-driven tests: empty field validation ---

    #[test]
    fn spawn_request_rejects_empty_type_name() {
        let bytes = encode_spawn_request(SpawnRequest {
            type_name: String::new(),
            args_bytes: vec![],
            name: "x".into(),
            request_id: "r1".into(),
        });
        let err = decode_spawn_request(&bytes).unwrap_err();
        assert!(err.message.contains("type_name"), "error: {}", err.message);
    }

    #[test]
    fn spawn_request_rejects_empty_request_id() {
        let bytes = encode_spawn_request(SpawnRequest {
            type_name: "myapp::T".into(),
            args_bytes: vec![],
            name: "x".into(),
            request_id: String::new(),
        });
        let err = decode_spawn_request(&bytes).unwrap_err();
        assert!(err.message.contains("request_id"), "error: {}", err.message);
    }

    #[test]
    fn connect_peer_rejects_empty_node_id() {
        let bytes = encode_connect_peer(&NodeId(String::new()), None);
        let err = decode_connect_peer(&bytes).unwrap_err();
        assert!(err.message.contains("node_id"), "error: {}", err.message);
    }

    #[test]
    fn disconnect_peer_rejects_empty_node_id() {
        let bytes = encode_disconnect_peer(&NodeId(String::new()));
        let err = decode_disconnect_peer(&bytes).unwrap_err();
        assert!(err.message.contains("node_id"), "error: {}", err.message);
    }

    #[test]
    fn actor_id_rejects_empty_node_id() {
        // A WatchRequest with an empty node_id in the target should fail.
        use prost::Message;
        let proto = WatchRequestProto {
            target: Some(ActorIdProto {
                node_id: String::new(),
                local: 1,
            }),
            watcher: Some(ActorIdProto {
                node_id: "ok".into(),
                local: 2,
            }),
        };
        let bytes = proto.encode_to_vec();
        let err = decode_watch_request(&bytes).unwrap_err();
        assert!(err.message.contains("node_id"), "error: {}", err.message);
    }

    // --- Review-driven tests: size limits ---

    #[test]
    fn decode_rejects_oversized_payload() {
        let huge = vec![0u8; MAX_SYSTEM_MSG_SIZE + 1];
        let err = decode_spawn_request(&huge).unwrap_err();
        assert!(err.message.contains("too large"), "error: {}", err.message);
    }

    #[test]
    fn decode_wire_envelope_rejects_oversized_payload() {
        let huge = vec![0u8; MAX_WIRE_ENVELOPE_SIZE + 1];
        let err = decode_wire_envelope(&huge).unwrap_err();
        assert!(err.message.contains("too large"), "error: {}", err.message);
    }

    // --- Review-driven tests: malformed UUID ---

    #[test]
    fn wire_envelope_rejects_malformed_uuid() {
        use prost::Message;
        let proto = WireEnvelopeProto {
            target: Some(ActorIdProto {
                node_id: "n1".into(),
                local: 1,
            }),
            target_name: String::new(),
            message_type: "test".into(),
            send_mode: 0,
            headers: Default::default(),
            body: vec![],
            request_id: Some("not-a-uuid".into()),
            version: None,
        };
        let bytes = proto.encode_to_vec();
        let err = decode_wire_envelope(&bytes).unwrap_err();
        assert!(err.message.contains("UUID"), "error: {}", err.message);
    }
}
