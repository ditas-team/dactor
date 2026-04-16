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

/// Core actor traits and types (Actor, ActorRef, Handler, etc.).
pub mod actor;
/// Broadcast messaging for actor groups (BroadcastRef, tell, ask).
pub mod broadcast;
/// Batched transport sender for reducing per-message overhead (requires `serde` feature).
#[cfg(feature = "serde")]
pub mod batched_transport;
/// Circuit breaker interceptor for fault isolation.
pub mod circuit_breaker;
/// Clock abstraction for deterministic testing.
pub mod clock;
/// Cluster membership events and subscriptions.
pub mod cluster;
/// Dead letter handling for undeliverable messages.
pub mod dead_letter;
/// Message dispatch envelopes for tell, ask, stream, feed, and transform.
pub mod dispatch;
/// Error types for actor operations.
pub mod errors;
/// Named processing groups for actor pub/sub.
pub mod group;
/// Inbound and outbound message interceptors.
pub mod interceptor;
/// Mailbox capacity and overflow configuration.
pub mod mailbox;
/// Message trait and header types.
pub mod message;
#[cfg(feature = "metrics")]
/// Metrics collection interceptor and registry.
pub mod metrics;
/// Node and actor identity types.
pub mod node;
/// Per-destination outbound priority queue for remote messages.
pub mod outbound_queue;
/// Persistence support: journals, snapshots, and durable state.
pub mod persistence;
/// Actor pool routing and configuration.
pub mod pool;
/// Virtual actor pool with single-threaded routing task.
pub mod virtual_pool;
/// Protobuf serialization for system messages and wire envelope framing.
pub mod proto;
/// Named actor registry for service location.
pub mod registry;
/// Remote actor types, wire format, and cluster discovery.
pub mod remote;
/// Remote actor reference for cross-node communication.
pub mod remote_ref;
/// Shared runtime helpers for adapter implementations.
pub mod runtime_support;
/// Streaming primitives (StreamSender, StreamReceiver, batching).
pub mod stream;
/// Supervision strategies (OneForOne, AllForOne, RestForOne).
pub mod supervision;
/// System actors for remote operations (spawn, watch, cancel, directory).
pub mod system_actors;
/// Transport routing for incoming system messages to native actor mailboxes.
pub mod system_router;
/// Rate limiting for actors.
pub mod throttle;
/// Timer scheduling (send_after, send_interval).
pub mod timer;
/// Abstract transport for remote actor communication.
pub mod transport;
/// Type registry for remote message deserialization and actor factories.
pub mod type_registry;
/// Wire protocol version for cluster compatibility checks.
pub mod version;
/// Envelope-level interceptor for incoming remote messages.
pub mod wire_interceptor;
/// Worker reference for distributed actor pools (local + remote).
pub mod worker_ref;

#[cfg(feature = "test-support")]
pub mod test_support;

/// Convenience re-exports of the most commonly used types.
pub mod prelude {
    pub use crate::actor::*;
    pub use crate::broadcast::*;
    pub use crate::clock::*;
    pub use crate::cluster::*;
    pub use crate::errors::*;
    pub use crate::group::*;
    pub use crate::node::*;
    pub use crate::timer::*;
}

// Backward-compatible re-exports at crate root
pub use actor::cancel_after;
pub use actor::ReduceHandler;
pub use actor::{Actor, ActorContext, ActorError, ActorRef, SpawnConfig};
pub use actor::{AskReply, Handler, ExpandHandler, TransformHandler};
pub use async_trait::async_trait;
pub use broadcast::{BroadcastReceipt, BroadcastRef, BroadcastTellResult, BroadcastTellOutcome};
pub use group::ProcessingGroup;
#[cfg(feature = "serde")]
pub use batched_transport::{
    is_batch_envelope, unpack_batch, BatchedTransportSender, WireEnvelopeBatch, BATCH_MESSAGE_TYPE,
};
pub use circuit_breaker::{CircuitBreakerInterceptor, CircuitState};
pub use clock::{Clock, SystemClock};
pub use cluster::{
    AdapterCluster, ClusterEventEmitter, HealthChecker, HealthStatus, UnreachableHandler,
    perform_handshake,
};
pub use cluster::{ClusterEvent, ClusterEvents, HandshakeOutcome, NodeRejectionReason, SubscriptionId};
pub use dead_letter::{
    CollectingDeadLetterHandler, DeadLetterEvent, DeadLetterHandler, DeadLetterInfo,
    DeadLetterReason, LoggingDeadLetterHandler,
};
pub use errors::{ActorSendError, ClusterError, GroupError, RuntimeError};
pub use errors::{ErrorAction, ErrorCode, NotSupportedError};
pub use interceptor::{
    intercept_outbound_stream_item, Disposition, InboundContext, InboundInterceptor,
    InterceptResult, Outcome, SendMode,
};
pub use interceptor::{notify_drop, DropNotice, DropObserver};
pub use interceptor::{OutboundContext, OutboundInterceptor};
pub use mailbox::{MailboxConfig, MessageComparer, OverflowStrategy, StrictPriorityComparer};
pub use message::Message;
pub use message::{HeaderValue, Headers, MessageId, Priority, RuntimeHeaders};
#[cfg(feature = "metrics")]
pub use metrics::{
    ActorMetricsHandle, ActorMetricsSnapshot, MetricsInterceptor, MetricsRegistry, RuntimeMetrics,
};
pub use node::{ActorId, NodeId};
pub use outbound_queue::OutboundPriorityQueue;
pub use outbound_queue::{
    AgingWireComparer, EnvelopeMetadata, StrictPriorityWireComparer, WireEnvelopeComparer,
};
pub use persistence::{
    recover_durable_state, recover_event_sourced, DurableState, EventSourced, InMemoryStorage,
    InMemoryStorageProvider, JournalEntry, JournalStorage, PersistError, PersistFailurePolicy,
    PersistenceId, PersistentActor, RecoveryFailurePolicy, SaveConfig, SequenceId, SnapshotConfig,
    SnapshotEntry, SnapshotStorage, StateStorage, StorageProvider,
};
pub use pool::{Keyed, PoolConfig, PoolRef, PoolRouting};
pub use virtual_pool::VirtualPoolRef;
pub use registry::ActorRegistry;
#[cfg(feature = "serde")]
pub use remote::{build_ask_envelope, build_tell_envelope, build_wire_envelope, JsonSerializer};
pub use remote::{receive_envelope_body, receive_envelope_body_versioned};
pub use remote::{
    ClusterDiscovery, ClusterState, DiscoveryError, HeaderRegistry, MessageSerializer,
    MessageVersionHandler, PeerVersionInfo, RemoteMessage, SerializationError, StaticSeeds,
    WireEnvelope, WireHeaders,
};
pub use remote_ref::{
    ActorRefEnvelope, ActorRefTypeMismatch, RemoteActorRef, RemoteActorRefBuilder,
};
pub use stream::{BatchConfig, BatchReader, BatchWriter, StreamReceiver};
pub use stream::{BoxStream, StreamSendError, StreamSender};
pub use supervision::ChildTerminated;
pub use supervision::{AllForOne, OneForOne, RestForOne, SupervisionAction, SupervisionStrategy};
pub use system_actors::{
    CancelManager, CancelRequest, CancelResponse, NodeDirectory, PeerInfo, PeerStatus,
    SpawnManager, SpawnRequest, SpawnResponse, UnwatchRequest, WatchManager, WatchNotification,
    WatchRequest,
};
pub use system_actors::{
    validate_handshake, HandshakeRequest, HandshakeResponse, RejectionReason,
};
pub use system_actors::{
    is_system_message_type, SYSTEM_MSG_TYPE_CANCEL, SYSTEM_MSG_TYPE_CONNECT_PEER,
    SYSTEM_MSG_TYPE_DISCONNECT_PEER, SYSTEM_MSG_TYPE_SPAWN, SYSTEM_MSG_TYPE_UNWATCH,
    SYSTEM_MSG_TYPE_WATCH,
};
pub use system_router::{RoutingError, RoutingOutcome, SystemMessageRouter};
pub use throttle::ActorRateLimiter;
pub use timer::TimerHandle;
pub use timer::{send_after, send_interval};
pub use tokio_util::sync::CancellationToken;
pub use transport::{InMemoryTransport, Transport, TransportError, TransportRegistry};
#[cfg(feature = "serde")]
pub use type_registry::JsonActorFactory;
pub use type_registry::TypeRegistry;
pub use type_registry::{ActorFactory, ErasedActorFactory};
pub use wire_interceptor::{
    MaxBodySizeInterceptor, RateLimitWireInterceptor, WireDisposition, WireInterceptor,
    WireInterceptorPipeline, WireProcessResult, WireRejectError,
};
pub use worker_ref::WorkerRef;
pub use version::{DACTOR_WIRE_VERSION, ParseWireVersionError, WireVersion};

// Backward-compatible re-export of TestClock (feature-gated)
#[cfg(feature = "test-support")]
pub use test_support::test_clock::TestClock;

// Test runtime re-exports (feature-gated)
#[cfg(feature = "test-support")]
pub use test_support::test_runtime::{SpawnOptions, TestActorRef, TestRuntime};
