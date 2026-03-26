//! # dactor-ractor
//!
//! Ractor adapter for the [`dactor`] distributed actor framework.
//!
//! This crate provides [`RactorRuntime`], an implementation of
//! [`dactor::ActorRuntime`] backed by ractor actors.
//! Actors are spawned as real ractor actors via [`ractor::Actor::spawn`],
//! and messages are delivered through ractor's mailbox system.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use dactor_ractor::RactorRuntime;
//! use dactor::{ActorRuntime, ActorRef};
//!
//! #[tokio::main]
//! async fn main() {
//!     let runtime = RactorRuntime::new();
//!     let actor = runtime.spawn("greeter", |msg: String| {
//!         println!("Got: {msg}");
//!     });
//!     actor.send("hello".into()).unwrap();
//! }
//! ```

pub mod cluster;
pub mod runtime;

pub use cluster::RactorClusterEvents;
pub use runtime::{RactorActorRef, RactorRuntime, RactorTimerHandle};

// Re-export the core dactor crate for convenience
pub use dactor;
