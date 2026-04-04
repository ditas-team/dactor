use std::fmt;
use std::time::Duration;

/// Error category codes inspired by gRPC status codes.
/// Used in `ActorError` to classify the failure type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ErrorCode {
    /// Unexpected internal error.
    Internal,
    /// Invalid argument provided by the caller.
    InvalidArgument,
    /// The requested actor or resource was not found.
    NotFound,
    /// The actor is temporarily unavailable (e.g., overloaded).
    Unavailable,
    /// The operation timed out.
    Timeout,
    /// The caller lacks permission for this operation.
    PermissionDenied,
    /// A precondition for the operation was not met.
    FailedPrecondition,
    /// A resource limit was exceeded (e.g., rate limit, quota).
    ResourceExhausted,
    /// The operation is not implemented by this actor/adapter.
    Unimplemented,
    /// Unknown error category.
    Unknown,
    /// The operation was cancelled.
    Cancelled,
}

/// Error returned when sending a message to an actor fails.
///
/// Common causes: the actor has stopped, the mailbox is full (bounded),
/// or the mailbox channel is closed.
#[derive(Debug, Clone)]
pub struct ActorSendError(pub String);

impl fmt::Display for ActorSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "actor send failed: {}", self.0)
    }
}

impl std::error::Error for ActorSendError {}

/// Error returned by processing group operations such as join, leave,
/// broadcast, or member enumeration.
#[derive(Debug, Clone)]
pub struct GroupError(pub String);

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "group error: {}", self.0)
    }
}

impl std::error::Error for GroupError {}

/// Error returned by cluster event subscription and unsubscription operations.
#[derive(Debug, Clone)]
pub struct ClusterError(pub String);

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cluster error: {}", self.0)
    }
}

impl std::error::Error for ClusterError {}

/// What the runtime should do after a handler error or panic.
///
/// Returned by [`Actor::on_error`](crate::actor::Actor::on_error) to let
/// the actor choose its own recovery strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    /// Skip the failed message and continue processing the next one.
    Resume,
    /// Restart the actor (re-run `create` + `on_start`), then continue.
    Restart,
    /// Stop the actor permanently.
    Stop,
    /// Propagate the error to the actor's supervisor (parent).
    Escalate,
}

/// Error returned when a runtime capability is not supported by the adapter.
#[derive(Debug, Clone)]
pub struct NotSupportedError {
    /// The capability that is not supported.
    pub capability: String,
    /// Detailed description of why it is not supported.
    pub message: String,
}

impl fmt::Display for NotSupportedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not supported: {} — {}", self.capability, self.message)
    }
}

impl std::error::Error for NotSupportedError {}

/// Unified error returned by runtime operations (ask, stream, feed, etc.).
///
/// Covers all failure modes: delivery errors, timeouts, interceptor
/// rejections, and handler-level actor errors.
#[derive(Debug)]
pub enum RuntimeError {
    /// Message delivery failed (actor stopped, mailbox full, etc.)
    Send(ActorSendError),
    /// The target actor was not found or has stopped.
    ActorNotFound(String),
    /// The operation timed out.
    Timeout,
    /// The operation was rejected by an interceptor.
    Rejected {
        /// Name of the interceptor that rejected the message.
        interceptor: String,
        /// Reason the message was rejected.
        reason: String,
    },
    /// An interceptor suggests the caller retry after the given duration.
    /// The message was NOT delivered.
    RetryAfter {
        /// Name of the interceptor that requested the retry.
        interceptor: String,
        /// Suggested delay before retrying.
        retry_after: Duration,
    },
    /// A handler-level error occurred.
    Actor(crate::actor::ActorError),
    /// The operation was cancelled via CancellationToken.
    Cancelled,
    /// The requested capability is not supported by this adapter.
    NotSupported(NotSupportedError),
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
            Self::Cancelled => write!(f, "operation cancelled"),
            Self::NotSupported(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<ActorSendError> for RuntimeError {
    fn from(e: ActorSendError) -> Self {
        Self::Send(e)
    }
}
