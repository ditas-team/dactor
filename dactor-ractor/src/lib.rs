//! # dactor-ractor
//!
//! Ractor adapter for the [`dactor`] distributed actor framework (v0.2 API).
//!
//! This crate provides [`RactorRuntime`], which spawns dactor actors as real
//! ractor actors via [`ractor::Actor::spawn`]. Messages are delivered through
//! ractor's mailbox as type-erased dispatch envelopes, supporting multiple
//! `Handler<M>` impls per actor.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use dactor::prelude::*;
//! use dactor::message::Message;
//! use dactor_ractor::RactorRuntime;
//! use async_trait::async_trait;
//!
//! struct MyActor;
//!
//! impl Actor for MyActor {
//!     type Args = ();
//!     type Deps = ();
//!     fn create(_: (), _: ()) -> Self { MyActor }
//! }
//!
//! struct Greet(String);
//! impl Message for Greet { type Reply = (); }
//!
//! #[async_trait]
//! impl Handler<Greet> for MyActor {
//!     async fn handle(&mut self, msg: Greet, _ctx: &mut ActorContext) {
//!         println!("Got: {}", msg.0);
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let runtime = RactorRuntime::new();
//!     let actor = runtime.spawn::<MyActor>("greeter", ());
//!     actor.tell(Greet("hello".into())).unwrap();
//! }
//! ```

pub mod cluster;
pub mod runtime;
pub mod system_actors;

pub use cluster::RactorClusterEvents;
pub use runtime::{RactorActorRef, RactorRuntime, RactorSystemActorRefs, SpawnOptions};
pub use system_actors::{
    CancelManagerActor, NodeDirectoryActor, SpawnManagerActor, WatchManagerActor,
};

// Re-export the core dactor crate for convenience
pub use dactor;
