//! # dactor-kameo
//!
//! Kameo adapter for the [`dactor`] distributed actor framework.
//!
//! This crate provides [`KameoRuntime`], an implementation of
//! [`dactor::ActorRuntime`] backed by kameo actors.
//! Actors are spawned as real kameo actors via kameo's `Spawn::spawn`,
//! and messages are delivered through kameo's mailbox system.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use dactor_kameo::KameoRuntime;
//! use dactor::{ActorRuntime, ActorRef};
//!
//! #[tokio::main]
//! async fn main() {
//!     let runtime = KameoRuntime::new();
//!     let actor = runtime.spawn("greeter", |msg: String| {
//!         println!("Got: {msg}");
//!     });
//!     actor.send("hello".into()).unwrap();
//! }
//! ```

pub mod cluster;
pub mod runtime;

pub use cluster::KameoClusterEvents;
pub use runtime::{KameoActorRef, KameoRuntime, KameoTimerHandle};

// Re-export the core dactor crate for convenience
pub use dactor;
