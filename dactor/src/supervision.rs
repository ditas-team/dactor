//! Supervision primitives for the actor system.
//!
//! Provides [`ChildTerminated`], the notification delivered to watchers when a
//! watched actor stops.  Actors that call `watch()` on the runtime must
//! implement `Handler<ChildTerminated>` to receive these notifications.

use crate::message::Message;
use crate::node::ActorId;

/// Notification delivered when a watched actor terminates.
///
/// Actors that call `watch()` must implement `Handler<ChildTerminated>`
/// to receive these notifications.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChildTerminated {
    /// The ID of the actor that terminated.
    pub child_id: ActorId,
    /// The name the actor was spawned with.
    pub child_name: String,
    /// `None` for graceful shutdown, `Some(reason)` for failure.
    pub reason: Option<String>,
}

impl Message for ChildTerminated {
    type Reply = ();
}
