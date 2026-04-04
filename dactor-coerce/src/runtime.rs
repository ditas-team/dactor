//! Coerce adapter runtime.
//!
//! Currently wraps `TestRuntime` as a placeholder. The actual `coerce-rt`
//! integration will replace the internal implementation while keeping
//! the same public API.
//!
//! # Why a stub?
//!
//! The coerce-rs crate (`coerce-rt`) may have dependency or maintenance
//! issues. This stub lets us lock down the API surface and pass
//! conformance tests now, then swap in the real engine later.

use dactor::actor::*;
use dactor::errors::ActorSendError;
use dactor::interceptor::*;
use dactor::mailbox::MailboxConfig;
use dactor::message::*;
use dactor::node::*;
use dactor::stream::*;
use dactor::system_actors::{
    CancelManager, CancelResponse, NodeDirectory, PeerStatus, SpawnManager, SpawnRequest,
    SpawnResponse, WatchManager, WatchNotification,
};
use dactor::test_support::test_runtime::{TestActorRef, TestRuntime};
use dactor::type_registry::TypeRegistry;

use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor with the coerce adapter.
pub struct SpawnOptions {
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
    pub mailbox: MailboxConfig,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::Unbounded,
        }
    }
}

// ---------------------------------------------------------------------------
// CoerceRuntime
// ---------------------------------------------------------------------------

/// Coerce adapter runtime.
///
/// Currently backed by [`TestRuntime`] as a placeholder.
/// The `coerce-rt` dependency will be integrated in a future PR,
/// replacing the internals while preserving this public API.
///
/// ## System Actors
///
/// The runtime includes system actors for remote operations:
/// - [`SpawnManager`] — handles remote actor spawn requests
/// - [`WatchManager`] — handles remote watch/unwatch subscriptions
/// - [`CancelManager`] — handles remote cancellation requests
/// - [`NodeDirectory`] — tracks peer node connection state
pub struct CoerceRuntime {
    inner: TestRuntime,
    node_id: NodeId,
    /// Separate local ID counter for remote spawn requests.
    ///
    /// Local spawns (via `spawn()`) use `TestRuntime`'s internal counter.
    /// Remote spawns (via `handle_spawn_request()`) use this counter with
    /// an offset to avoid collisions. When the real coerce-rt engine
    /// replaces TestRuntime, a single counter will be used.
    next_remote_local: AtomicU64,
    /// Manages remote actor spawn requests for this node.
    spawn_manager: SpawnManager,
    /// Manages remote watch/unwatch subscriptions for this node.
    watch_manager: WatchManager,
    /// Manages remote cancellation requests for this node.
    cancel_manager: CancelManager,
    /// Tracks peer node connection state.
    node_directory: NodeDirectory,
}

impl CoerceRuntime {
    /// Offset for remote spawn IDs to avoid collisions with local spawns.
    const REMOTE_ID_OFFSET: u64 = 1_000_000;

    /// Create a new `CoerceRuntime`.
    pub fn new() -> Self {
        let node_id = NodeId("coerce-node".into());
        Self {
            inner: TestRuntime::with_node_id(node_id.clone()),
            node_id,
            next_remote_local: AtomicU64::new(Self::REMOTE_ID_OFFSET),
            spawn_manager: SpawnManager::new(TypeRegistry::new()),
            watch_manager: WatchManager::new(),
            cancel_manager: CancelManager::new(),
            node_directory: NodeDirectory::new(),
        }
    }

    /// Create a new `CoerceRuntime` with a specific node ID.
    pub fn with_node_id(node_id: NodeId) -> Self {
        Self {
            inner: TestRuntime::with_node_id(node_id.clone()),
            node_id,
            next_remote_local: AtomicU64::new(Self::REMOTE_ID_OFFSET),
            spawn_manager: SpawnManager::new(TypeRegistry::new()),
            watch_manager: WatchManager::new(),
            cancel_manager: CancelManager::new(),
            node_directory: NodeDirectory::new(),
        }
    }

    /// Returns the node ID of this runtime.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Spawn an actor whose `Deps` type is `()`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> CoerceActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        CoerceActorRef {
            inner: self.inner.spawn(name, args),
        }
    }

    /// Spawn an actor with explicit dependencies.
    pub fn spawn_with_deps<A>(&self, name: &str, args: A::Args, deps: A::Deps) -> CoerceActorRef<A>
    where
        A: Actor + 'static,
    {
        CoerceActorRef {
            inner: self.inner.spawn_with_deps(name, args, deps),
        }
    }

    /// Spawn an actor with spawn options (interceptors, mailbox config).
    pub fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> CoerceActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        // Convert our SpawnOptions into TestRuntime's SpawnOptions.
        let inner_opts = dactor::test_support::test_runtime::SpawnOptions {
            interceptors: options.interceptors,
            mailbox: options.mailbox,
        };
        CoerceActorRef {
            inner: self.inner.spawn_with_options(name, args, inner_opts),
        }
    }

    /// Add a global outbound interceptor.
    ///
    /// Must be called before any actors are spawned.
    pub fn add_outbound_interceptor(&mut self, interceptor: Box<dyn OutboundInterceptor>) {
        self.inner.add_outbound_interceptor(interceptor);
    }

    // -----------------------------------------------------------------------
    // SA10: SpawnManager wiring
    // -----------------------------------------------------------------------

    /// Access the spawn manager.
    pub fn spawn_manager(&self) -> &SpawnManager {
        &self.spawn_manager
    }

    /// Access the spawn manager mutably (for registering actor factories).
    pub fn spawn_manager_mut(&mut self) -> &mut SpawnManager {
        &mut self.spawn_manager
    }

    /// Register an actor type for remote spawning on this node.
    ///
    /// The factory closure deserializes actor `Args` from bytes and returns
    /// the constructed actor as `Box<dyn Any + Send>`. The runtime is
    /// responsible for actually spawning the returned actor.
    pub fn register_factory(
        &mut self,
        type_name: impl Into<String>,
        factory: impl Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
            + Send
            + Sync
            + 'static,
    ) {
        self.spawn_manager
            .type_registry_mut()
            .register_factory(type_name, factory);
    }

    /// Process a remote spawn request.
    ///
    /// Looks up the actor type in the registry, deserializes Args from bytes,
    /// and returns the constructed actor along with its assigned [`ActorId`].
    ///
    /// The returned `ActorId` is pre-assigned for the remote spawn flow where
    /// the runtime controls ID assignment. The caller must use this ID when
    /// registering the spawned actor (not via the regular `spawn()` path,
    /// which assigns its own IDs).
    ///
    /// **Note:** Currently uses the simple factory API (`TypeRegistry::create_actor`).
    /// For actors with non-trivial `Deps`, use `spawn_manager_mut()` to access
    /// `TypeRegistry::create_actor_with_deps()` directly.
    ///
    /// Returns `Ok((actor_id, actor))` on success, or `Err(SpawnResponse::Failure)`
    /// if the type is not found or deserialization fails.
    pub fn handle_spawn_request(
        &mut self,
        request: &SpawnRequest,
    ) -> Result<(ActorId, Box<dyn std::any::Any + Send>), SpawnResponse> {
        match self.spawn_manager.create_actor(request) {
            Ok(actor) => {
                let local = self.next_remote_local.fetch_add(1, Ordering::SeqCst);
                let actor_id = ActorId {
                    node: self.node_id.clone(),
                    local,
                };
                self.spawn_manager.record_spawn(actor_id.clone());
                Ok((actor_id, actor))
            }
            Err(e) => Err(SpawnResponse::Failure {
                request_id: request.request_id.clone(),
                error: e.to_string(),
            }),
        }
    }

    // -----------------------------------------------------------------------
    // SA10: WatchManager wiring
    // -----------------------------------------------------------------------

    /// Access the watch manager (for remote watch subscriptions).
    pub fn watch_manager(&self) -> &WatchManager {
        &self.watch_manager
    }

    /// Access the watch manager mutably.
    pub fn watch_manager_mut(&mut self) -> &mut WatchManager {
        &mut self.watch_manager
    }

    /// Register a remote watch: a remote watcher wants to know when a local
    /// actor terminates.
    pub fn remote_watch(&mut self, target: ActorId, watcher: ActorId) {
        self.watch_manager.watch(target, watcher);
    }

    /// Remove a remote watch subscription.
    pub fn remote_unwatch(&mut self, target: &ActorId, watcher: &ActorId) {
        self.watch_manager.unwatch(target, watcher);
    }

    /// Called when a local actor terminates. Returns notifications for all
    /// remote watchers that should be sent to their respective nodes.
    ///
    /// **Note:** This must be called explicitly by the integration layer.
    /// It is not yet automatically wired into coerce's actor stop lifecycle.
    pub fn notify_terminated(&mut self, terminated: &ActorId) -> Vec<WatchNotification> {
        self.watch_manager.on_terminated(terminated)
    }

    // -----------------------------------------------------------------------
    // SA10: CancelManager wiring
    // -----------------------------------------------------------------------

    /// Access the cancel manager.
    pub fn cancel_manager(&self) -> &CancelManager {
        &self.cancel_manager
    }

    /// Access the cancel manager mutably.
    pub fn cancel_manager_mut(&mut self) -> &mut CancelManager {
        &mut self.cancel_manager
    }

    /// Register a cancellation token for a request (for remote cancel support).
    pub fn register_cancel(&mut self, request_id: String, token: CancellationToken) {
        self.cancel_manager.register(request_id, token);
    }

    /// Process a remote cancellation request.
    pub fn cancel_request(&mut self, request_id: &str) -> CancelResponse {
        self.cancel_manager.cancel(request_id)
    }

    /// Clean up a cancellation token after its request completes normally.
    ///
    /// Should be called when a remote ask/stream/feed completes successfully
    /// to prevent stale tokens from accumulating.
    pub fn complete_request(&mut self, request_id: &str) {
        self.cancel_manager.remove(request_id);
    }

    // -----------------------------------------------------------------------
    // SA10: NodeDirectory wiring
    // -----------------------------------------------------------------------

    /// Access the node directory.
    pub fn node_directory(&self) -> &NodeDirectory {
        &self.node_directory
    }

    /// Access the node directory mutably.
    pub fn node_directory_mut(&mut self) -> &mut NodeDirectory {
        &mut self.node_directory
    }

    /// Register a peer node in the directory.
    ///
    /// If the peer already exists, updates its status to `Connected` and
    /// preserves the existing address when `address` is `None`.
    pub fn connect_peer(&mut self, peer_id: NodeId, address: Option<String>) {
        if let Some(existing) = self.node_directory.get_peer(&peer_id) {
            let resolved_address = address.or_else(|| existing.address.clone());
            self.node_directory.remove_peer(&peer_id);
            self.node_directory.add_peer(peer_id.clone(), resolved_address);
        } else {
            self.node_directory.add_peer(peer_id.clone(), address);
        }
        self.node_directory.set_status(&peer_id, PeerStatus::Connected);
    }

    /// Mark a peer as disconnected.
    pub fn disconnect_peer(&mut self, peer_id: &NodeId) {
        self.node_directory.set_status(peer_id, PeerStatus::Disconnected);
    }

    /// Check if a peer node is connected.
    pub fn is_peer_connected(&self, peer_id: &NodeId) -> bool {
        self.node_directory.is_connected(peer_id)
    }
}

impl Default for CoerceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CoerceActorRef
// ---------------------------------------------------------------------------

/// A reference to an actor managed by the coerce adapter.
///
/// Currently wraps [`TestActorRef`] as a placeholder.
pub struct CoerceActorRef<A: Actor> {
    inner: TestActorRef<A>,
}

impl<A: Actor> Clone for CoerceActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<A: Actor + 'static> ActorRef<A> for CoerceActorRef<A> {
    fn id(&self) -> ActorId {
        self.inner.id()
    }

    fn name(&self) -> String {
        self.inner.name()
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    fn stop(&self) {
        self.inner.stop()
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        self.inner.tell(msg)
    }

    fn ask<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message,
    {
        self.inner.ask(msg, cancel)
    }

    fn stream<M>(
        &self,
        msg: M,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        self.inner.stream(msg, buffer, batch_config, cancel)
    }

    fn feed<Item, Reply>(
        &self,
        input: BoxStream<Item>,
        buffer: usize,
        batch_config: Option<BatchConfig>,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: FeedHandler<Item, Reply>,
        Item: Send + 'static,
        Reply: Send + 'static,
    {
        self.inner.feed(input, buffer, batch_config, cancel)
    }
}
