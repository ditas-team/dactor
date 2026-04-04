use crate::interceptor::SendMode;
use crate::node::ActorId;
use std::any::Any;

/// Reason a message became a dead letter.
#[derive(Debug, Clone)]
pub enum DeadLetterReason {
    /// The target actor has stopped.
    ActorStopped,
    /// The target actor was not found.
    ActorNotFound,
    /// The mailbox was full and overflow strategy rejected the message.
    MailboxFull,
    /// An interceptor dropped the message or stream item.
    DroppedByInterceptor {
        /// Name of the interceptor that dropped the message.
        interceptor: String,
    },
}

impl std::fmt::Display for DeadLetterReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActorStopped => write!(f, "actor stopped"),
            Self::ActorNotFound => write!(f, "actor not found"),
            Self::MailboxFull => write!(f, "mailbox full"),
            Self::DroppedByInterceptor { interceptor } => {
                write!(f, "dropped by interceptor '{}'", interceptor)
            }
        }
    }
}

/// Information about a dead letter — a message that could not be delivered.
#[derive(Debug)]
pub struct DeadLetterEvent {
    /// The intended target actor.
    pub target_id: ActorId,
    /// The target actor's name (if known).
    pub target_name: Option<String>,
    /// The Rust type name of the message.
    pub message_type: &'static str,
    /// How the message was sent.
    pub send_mode: SendMode,
    /// Why the message could not be delivered.
    pub reason: DeadLetterReason,
    /// The message body (type-erased). May be None if the message was consumed.
    pub message: Option<Box<dyn Any + Send>>,
}

/// Handler for dead letter events. Registered on the runtime.
///
/// The default implementation logs dead letters via `tracing::warn!`.
/// Applications can provide custom handlers for monitoring, alerting,
/// or forwarding dead letters to a retry queue.
pub trait DeadLetterHandler: Send + Sync + 'static {
    /// Called when a message cannot be delivered.
    fn on_dead_letter(&self, event: DeadLetterEvent);
}

/// Default dead letter handler that logs via tracing.
pub struct LoggingDeadLetterHandler;

impl DeadLetterHandler for LoggingDeadLetterHandler {
    fn on_dead_letter(&self, event: DeadLetterEvent) {
        tracing::warn!(
            target = %event.target_id,
            message_type = event.message_type,
            send_mode = ?event.send_mode,
            reason = %event.reason,
            "dead letter"
        );
    }
}

/// A dead letter handler that collects events for testing.
pub struct CollectingDeadLetterHandler {
    events: std::sync::Mutex<Vec<DeadLetterInfo>>,
}

/// Simplified dead letter info for test assertions (without the type-erased message).
#[derive(Debug, Clone)]
pub struct DeadLetterInfo {
    /// The intended target actor.
    pub target_id: ActorId,
    /// The Rust type name of the message.
    pub message_type: String,
    /// How the message was sent.
    pub send_mode: SendMode,
    /// Why the message could not be delivered.
    pub reason: DeadLetterReason,
}

impl CollectingDeadLetterHandler {
    /// Create a new empty collecting handler.
    pub fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Get all collected dead letter events.
    pub fn events(&self) -> Vec<DeadLetterInfo> {
        self.events.lock().unwrap().clone()
    }

    /// Number of collected events.
    pub fn count(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Clear collected events.
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

impl Default for CollectingDeadLetterHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadLetterHandler for CollectingDeadLetterHandler {
    fn on_dead_letter(&self, event: DeadLetterEvent) {
        self.events.lock().unwrap().push(DeadLetterInfo {
            target_id: event.target_id,
            message_type: event.message_type.to_string(),
            send_mode: event.send_mode,
            reason: event.reason,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;

    #[test]
    fn test_dead_letter_reason_display() {
        assert_eq!(
            format!("{}", DeadLetterReason::ActorStopped),
            "actor stopped"
        );
        assert_eq!(format!("{}", DeadLetterReason::MailboxFull), "mailbox full");
        assert_eq!(
            format!(
                "{}",
                DeadLetterReason::DroppedByInterceptor {
                    interceptor: "auth".into()
                }
            ),
            "dropped by interceptor 'auth'"
        );
    }

    #[test]
    fn test_logging_handler_does_not_panic() {
        let handler = LoggingDeadLetterHandler;
        handler.on_dead_letter(DeadLetterEvent {
            target_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            target_name: Some("test-actor".into()),
            message_type: "MyMsg",
            send_mode: SendMode::Tell,
            reason: DeadLetterReason::ActorStopped,
            message: None,
        });
    }

    #[test]
    fn test_collecting_handler() {
        let handler = CollectingDeadLetterHandler::new();

        handler.on_dead_letter(DeadLetterEvent {
            target_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            target_name: Some("actor-1".into()),
            message_type: "Increment",
            send_mode: SendMode::Tell,
            reason: DeadLetterReason::ActorStopped,
            message: None,
        });

        handler.on_dead_letter(DeadLetterEvent {
            target_id: ActorId {
                node: NodeId("n1".into()),
                local: 2,
            },
            target_name: Some("actor-2".into()),
            message_type: "GetCount",
            send_mode: SendMode::Ask,
            reason: DeadLetterReason::MailboxFull,
            message: None,
        });

        assert_eq!(handler.count(), 2);
        let events = handler.events();
        assert_eq!(events[0].message_type, "Increment");
        assert_eq!(events[1].reason.to_string(), "mailbox full");

        handler.clear();
        assert_eq!(handler.count(), 0);
    }

    #[test]
    fn test_dead_letter_with_message_body() {
        let handler = CollectingDeadLetterHandler::new();
        handler.on_dead_letter(DeadLetterEvent {
            target_id: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            target_name: None,
            message_type: "MyMsg",
            send_mode: SendMode::Tell,
            reason: DeadLetterReason::ActorNotFound,
            message: Some(Box::new(42u64)),
        });
        assert_eq!(handler.count(), 1);
    }
}
