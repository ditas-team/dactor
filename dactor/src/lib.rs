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
//! - [`ActorRef`] — Handle to a running actor for fire-and-forget messaging
//! - [`ActorRuntime`] — Actor spawning, timers, and processing groups
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
pub mod errors;
pub mod cluster;
pub mod interceptor;
pub mod message;
pub mod timer;
pub mod clock;
pub mod node;

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
pub use actor::{ActorRef, ActorRuntime};
pub use actor::{Actor, ActorContext, ActorError, SpawnConfig};
pub use actor::{AskReply, Handler, TypedActorRef};
pub use message::Message;
pub use message::{Headers, HeaderValue, RuntimeHeaders, MessageId, Envelope, Priority};
pub use errors::{ActorSendError, ClusterError, GroupError, RuntimeError};
pub use errors::ErrorAction;
pub use cluster::{ClusterEvent, ClusterEvents, SubscriptionId};
pub use timer::TimerHandle;
pub use clock::{Clock, SystemClock};
pub use node::{NodeId, ActorId};
pub use interceptor::{InboundInterceptor, InboundContext, Disposition, Outcome, SendMode};

// Backward-compatible re-export of TestClock (feature-gated)
#[cfg(feature = "test-support")]
pub use test_support::test_clock::TestClock;

// V2 test runtime re-exports (feature-gated)
#[cfg(feature = "test-support")]
pub use test_support::v2_test_runtime::{V2TestRuntime, V2ActorRef, SpawnOptions};
