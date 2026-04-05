//! # dactor-coerce
//!
//! Coerce adapter for the [`dactor`] distributed actor framework (v0.2 API).
//!
//! This crate provides [`CoerceRuntime`], which spawns dactor actors as real
//! coerce-rs actors. Messages are delivered through coerce's mailbox as
//! type-erased dispatch envelopes, supporting multiple `Handler<M>` impls
//! per actor.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use dactor::prelude::*;
//! use dactor::message::Message;
//! use dactor_coerce::CoerceRuntime;
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
//!     let runtime = CoerceRuntime::new();
//!     let actor = runtime.spawn::<MyActor>("greeter", ());
//!     actor.tell(Greet("hello".into())).unwrap();
//! }
//! ```

pub mod cluster;
pub mod runtime;

pub use cluster::CoerceClusterEvents;
pub use runtime::{CoerceActorRef, CoerceRuntime, SpawnOptions};

// Re-export the core dactor crate for convenience.
pub use dactor;
