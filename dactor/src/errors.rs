use std::fmt;
use std::time::Duration;

/// Error returned by `ActorRef::send()`.
#[derive(Debug, Clone)]
pub struct ActorSendError(pub String);

impl fmt::Display for ActorSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "actor send failed: {}", self.0)
    }
}

impl std::error::Error for ActorSendError {}

/// Error returned by processing group operations.
#[derive(Debug, Clone)]
pub struct GroupError(pub String);

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "group error: {}", self.0)
    }
}

impl std::error::Error for GroupError {}

/// Error returned by `ClusterEvents` operations.
#[derive(Debug, Clone)]
pub struct ClusterError(pub String);

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cluster error: {}", self.0)
    }
}

impl std::error::Error for ClusterError {}

/// What the runtime should do after a handler error or panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    Resume,
    Restart,
    Stop,
    Escalate,
}

/// Unified error returned by runtime operations (ask, stream, feed, etc.)
#[derive(Debug)]
pub enum RuntimeError {
    /// Message delivery failed (actor stopped, mailbox full, etc.)
    Send(ActorSendError),
    /// The target actor was not found or has stopped.
    ActorNotFound(String),
    /// The operation timed out.
    Timeout,
    /// The operation was rejected by an interceptor.
    Rejected { interceptor: String, reason: String },
    /// An interceptor suggests the caller retry after the given duration.
    /// The message was NOT delivered.
    RetryAfter { interceptor: String, retry_after: Duration },
    /// A handler-level error occurred.
    Actor(crate::actor::ActorError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send(e) => write!(f, "send error: {}", e),
            Self::ActorNotFound(id) => write!(f, "actor not found: {}", id),
            Self::Timeout => write!(f, "operation timed out"),
            Self::Rejected { interceptor, reason } => {
                write!(f, "rejected by '{}': {}", interceptor, reason)
            }
            Self::RetryAfter { interceptor, retry_after } => {
                write!(f, "retry after {:?} (suggested by '{}')", retry_after, interceptor)
            }
            Self::Actor(e) => write!(f, "actor error: {}", e),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<ActorSendError> for RuntimeError {
    fn from(e: ActorSendError) -> Self {
        Self::Send(e)
    }
}
