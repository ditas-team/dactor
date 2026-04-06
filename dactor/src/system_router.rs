//! Transport routing for incoming system messages.
//!
//! When a [`WireEnvelope`] arrives on a node, the transport layer must decide
//! whether it targets a user actor or a system actor. System messages carry
//! well-known [`message_type`](WireEnvelope::message_type) values (defined in
//! [`system_actors`](crate::system_actors)) and must be routed to the
//! corresponding system actor mailbox.
//!
//! The [`SystemMessageRouter`] trait is implemented by each adapter runtime
//! to dispatch incoming system envelopes to native system actors.
//!
//! # Example
//!
//! ```ignore
//! let outcome = runtime.route_system_envelope(envelope).await?;
//! match outcome {
//!     RoutingOutcome::SpawnCompleted { actor_id } => { /* ... */ }
//!     RoutingOutcome::Acknowledged => { /* tell-style, no reply */ }
//!     _ => {}
//! }
//! ```

use crate::node::ActorId;
use crate::remote::WireEnvelope;
use crate::system_actors;

use async_trait::async_trait;

// ---------------------------------------------------------------------------
// RoutingError
// ---------------------------------------------------------------------------

/// Error from system message routing.
#[derive(Debug, Clone)]
pub struct RoutingError {
    /// Description of the routing failure.
    pub message: String,
}

impl RoutingError {
    /// Create a new routing error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "routing error: {}", self.message)
    }
}

impl std::error::Error for RoutingError {}

// ---------------------------------------------------------------------------
// RoutingOutcome
// ---------------------------------------------------------------------------

/// Result of successfully routing a system message.
#[derive(Debug, Clone)]
pub enum RoutingOutcome {
    /// A spawn request was processed and an actor was created.
    SpawnCompleted {
        /// Request ID for correlating the reply back to the sender.
        request_id: String,
        /// The ID of the newly spawned actor.
        actor_id: ActorId,
    },
    /// A spawn request failed on the remote side.
    SpawnFailed {
        /// The request ID from the original SpawnRequest.
        request_id: String,
        /// Error description.
        error: String,
    },
    /// A tell-style system message was delivered (watch, unwatch, connect,
    /// disconnect). No reply payload.
    Acknowledged,
    /// A cancel request was processed and acknowledged.
    CancelAcknowledged,
    /// A cancel request targeted a request that was not found.
    CancelNotFound {
        /// Description of what was not found.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// SystemMessageRouter
// ---------------------------------------------------------------------------

/// Routes incoming [`WireEnvelope`] system messages to the correct native
/// system actor mailbox.
///
/// Each adapter runtime implements this trait. The transport layer calls
/// [`route_system_envelope`](SystemMessageRouter::route_system_envelope)
/// for every incoming envelope whose `message_type` is a well-known system
/// message type (see [`is_system_message_type`](system_actors::is_system_message_type)).
///
/// System message bodies are deserialized using the fixed protobuf wire
/// format (see [`proto`](crate::proto)). Application messages remain
/// pluggable via [`MessageSerializer`](crate::remote::MessageSerializer).
#[async_trait]
pub trait SystemMessageRouter: Send + Sync {
    /// Route a system message envelope to the appropriate system actor.
    ///
    /// System message bodies are deserialized internally using protobuf —
    /// no external serializer is needed.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError`] if:
    /// - The `message_type` is not a recognized system message type
    /// - System actors have not been started yet
    /// - Deserialization of the protobuf body fails
    /// - The target system actor's mailbox is closed
    async fn route_system_envelope(
        &self,
        envelope: WireEnvelope,
    ) -> Result<RoutingOutcome, RoutingError>;
}

/// Validates that an envelope carries a known system message type and returns
/// an error if not. Useful for router implementations.
pub fn validate_system_message_type(message_type: &str) -> Result<(), RoutingError> {
    if system_actors::is_system_message_type(message_type) {
        Ok(())
    } else {
        Err(RoutingError::new(format!(
            "unknown system message type: {message_type}"
        )))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_actors::*;

    #[test]
    fn is_system_message_type_recognizes_all_types() {
        assert!(is_system_message_type(SYSTEM_MSG_TYPE_SPAWN));
        assert!(is_system_message_type(SYSTEM_MSG_TYPE_WATCH));
        assert!(is_system_message_type(SYSTEM_MSG_TYPE_UNWATCH));
        assert!(is_system_message_type(SYSTEM_MSG_TYPE_CANCEL));
        assert!(is_system_message_type(SYSTEM_MSG_TYPE_CONNECT_PEER));
        assert!(is_system_message_type(SYSTEM_MSG_TYPE_DISCONNECT_PEER));
    }

    #[test]
    fn is_system_message_type_rejects_unknown() {
        assert!(!is_system_message_type("myapp::Counter"));
        assert!(!is_system_message_type(""));
        assert!(!is_system_message_type("dactor::system_actors::Unknown"));
    }

    #[test]
    fn validate_system_message_type_ok_for_known() {
        assert!(validate_system_message_type(SYSTEM_MSG_TYPE_SPAWN).is_ok());
        assert!(validate_system_message_type(SYSTEM_MSG_TYPE_CANCEL).is_ok());
    }

    #[test]
    fn validate_system_message_type_err_for_unknown() {
        let err = validate_system_message_type("unknown::Type").unwrap_err();
        assert!(err.message.contains("unknown system message type"));
    }

    #[test]
    fn routing_error_display() {
        let err = RoutingError::new("system actors not started");
        assert_eq!(format!("{err}"), "routing error: system actors not started");
    }

    #[test]
    fn routing_outcome_variants() {
        let spawn_ok = RoutingOutcome::SpawnCompleted {
            request_id: "r1".into(),
            actor_id: crate::node::ActorId {
                node: crate::node::NodeId("n1".into()),
                local: 42,
            },
        };
        assert!(matches!(spawn_ok, RoutingOutcome::SpawnCompleted { .. }));

        let spawn_fail = RoutingOutcome::SpawnFailed {
            request_id: "r1".into(),
            error: "type not found".into(),
        };
        assert!(matches!(spawn_fail, RoutingOutcome::SpawnFailed { .. }));

        assert!(matches!(RoutingOutcome::Acknowledged, RoutingOutcome::Acknowledged));
        assert!(matches!(RoutingOutcome::CancelAcknowledged, RoutingOutcome::CancelAcknowledged));

        let not_found = RoutingOutcome::CancelNotFound {
            reason: "no such request".into(),
        };
        assert!(matches!(not_found, RoutingOutcome::CancelNotFound { .. }));
    }
}
