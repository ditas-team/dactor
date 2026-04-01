//! Coerce adapter for the dactor distributed actor framework.
//!
//! Provides `CoerceRuntime`, an implementation backed by the coerce-rs
//! actor framework. Currently uses an internal test runtime as a placeholder
//! until the coerce-rt dependency is integrated.
//!
//! Coerce's `Handler<M>` pattern maps nearly 1:1 to dactor's API,
//! making this the most natural adapter.

pub mod runtime;

pub use runtime::{CoerceActorRef, CoerceRuntime, SpawnOptions};

// Re-export dactor core for convenience.
pub use dactor;
