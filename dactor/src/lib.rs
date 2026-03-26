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

// ── Traits ───────────────────────────────────────────────────────
pub use traits::runtime::{
    ActorRef, ActorRuntime, ActorSendError, ClusterError, ClusterEvent,
    ClusterEvents, GroupError, SubscriptionId, TimerHandle,
};
pub use traits::clock::{Clock, SystemClock, TestClock};

// ── Types ────────────────────────────────────────────────────────
pub use types::node::NodeId;

// ── Test support ─────────────────────────────────────────────────
pub mod test_support;

// ── Internal modules ─────────────────────────────────────────────
mod traits;
mod types;
