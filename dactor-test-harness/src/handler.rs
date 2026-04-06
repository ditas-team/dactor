//! Trait for handling actor management commands in test nodes.
//!
//! Adapters (e.g. ractor, kameo) implement [`CommandHandler`] so that a
//! [`TestNode`](crate::TestNode) can spawn, tell, ask, and stop actors
//! through the gRPC control channel without depending on a specific runtime.

/// Handler for actor management commands dispatched by the test node gRPC server.
///
/// Each method maps to an RPC in the `TestNodeService` proto definition.
/// Implementations should be thread-safe and maintain their own actor registry.
#[async_trait::async_trait]
pub trait CommandHandler: Send + Sync + 'static {
    /// Human-readable adapter name (e.g. "ractor", "kameo", "coerce").
    fn adapter_name(&self) -> &str;

    /// Spawn an actor of the given type with the given name.
    /// Returns the actor ID on success.
    async fn spawn_actor(
        &self,
        actor_type: &str,
        actor_name: &str,
        args: &[u8],
    ) -> Result<String, String>;

    /// Fire-and-forget message to an actor.
    async fn tell_actor(
        &self,
        actor_name: &str,
        message_type: &str,
        payload: &[u8],
    ) -> Result<(), String>;

    /// Request-reply message to an actor. Returns the serialized reply.
    async fn ask_actor(
        &self,
        actor_name: &str,
        message_type: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, String>;

    /// Stop an actor by name.
    async fn stop_actor(&self, actor_name: &str) -> Result<(), String>;

    /// Return the number of live actors managed by this handler.
    fn actor_count(&self) -> u32 {
        0
    }
}
