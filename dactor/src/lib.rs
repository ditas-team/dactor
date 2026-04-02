//! # dactor
//!
//! An abstract framework for distributed actors in Rust.
//!
//! `dactor` provides framework-agnostic traits for building actor-based
//! systems. It defines the core abstractions for actor spawning, message
//! delivery, timer scheduling, processing groups, and cluster membership
//! events — without coupling to any specific actor framework.
//!
//! ## Core Traits
//!
//! - [`ActorRef`] — Typed handle to a running actor (tell, ask, stream, feed)
//! - [`Actor`] — Core actor trait with lifecycle hooks
//! - [`Handler`] — Per-message-type handler trait
//! - [`ClusterEvents`] — Subscribe to node join/leave notifications
//! - [`TimerHandle`] — Cancellable scheduled timer
//! - [`Clock`] — Time abstraction for deterministic testing
//!
//! ## Adapter Crates
//!
//! Use `dactor` with a concrete actor framework via an adapter:
//! - [`dactor-ractor`](https://crates.io/crates/dactor-ractor) — ractor adapter
//! - [`dactor-kameo`](https://crates.io/crates/dactor-kameo) — kameo adapter

pub mod actor;
pub mod dispatch;
pub mod errors;
pub mod cluster;
pub mod dead_letter;
pub mod interceptor;
pub mod mailbox;
pub mod message;
pub mod persistence;
pub mod stream;
pub mod supervision;
pub mod metrics;
pub mod throttle;
pub mod timer;
pub mod clock;
pub mod node;
pub mod remote;

#[cfg(feature = "test-support")]
pub mod test_support;

/// Convenience re-exports of the most commonly used types.
pub mod prelude {
    pub use crate::actor::*;
    pub use crate::errors::*;
    pub use crate::cluster::*;
    pub use crate::timer::*;
    pub use crate::clock::*;
    pub use crate::node::*;
}

// Backward-compatible re-exports at crate root
pub use async_trait::async_trait;
pub use actor::{Actor, ActorContext, ActorError, ActorRef, SpawnConfig};
pub use actor::{AskReply, Handler, StreamHandler};
pub use actor::{FeedMessage, FeedHandler};
pub use actor::cancel_after;
pub use tokio_util::sync::CancellationToken;
pub use message::Message;
pub use message::{Headers, HeaderValue, RuntimeHeaders, MessageId, Envelope, Priority, ContentLength};
pub use errors::{ActorSendError, ClusterError, GroupError, RuntimeError};
pub use errors::{ErrorAction, ErrorCode, NotSupportedError};
pub use cluster::{ClusterEvent, ClusterEvents, SubscriptionId};
pub use timer::TimerHandle;
pub use clock::{Clock, SystemClock};
pub use node::{NodeId, ActorId};
pub use supervision::ChildTerminated;
pub use interceptor::{InboundInterceptor, InboundContext, Disposition, Outcome, SendMode};
pub use interceptor::{OutboundInterceptor, OutboundContext};
pub use dead_letter::{
    DeadLetterHandler, DeadLetterEvent, DeadLetterReason,
    LoggingDeadLetterHandler, CollectingDeadLetterHandler, DeadLetterInfo,
};
pub use throttle::ActorRateLimiter;
pub use metrics::{MetricsInterceptor, MetricsStore, ActorMetrics};
pub use mailbox::{MailboxConfig, OverflowStrategy};
pub use stream::{BoxStream, StreamSendError, StreamSender};
pub use stream::{StreamReceiver, BatchConfig, BatchWriter, BatchReader};
pub use remote::{
    RemoteMessage, MessageSerializer, SerializationError,
    WireEnvelope, WireHeaders, MessageVersionHandler,
    ClusterState, ClusterDiscovery, StaticSeeds,
};
pub use persistence::{
    PersistenceId, SequenceId, JournalEntry, SnapshotEntry,
    PersistError, RecoveryFailurePolicy, PersistFailurePolicy,
    SnapshotConfig, SaveConfig,
    JournalStorage, SnapshotStorage, StateStorage,
    InMemoryStorage,
};

// Backward-compatible re-export of TestClock (feature-gated)
#[cfg(feature = "test-support")]
pub use test_support::test_clock::TestClock;

// Test runtime re-exports (feature-gated)
#[cfg(feature = "test-support")]
pub use test_support::test_runtime::{TestRuntime, TestActorRef, SpawnOptions};
