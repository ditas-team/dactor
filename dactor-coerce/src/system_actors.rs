//! Native coerce actor implementations for dactor system actors.
//!
//! Each system actor (SpawnManager, WatchManager, CancelManager, NodeDirectory)
//! is wrapped in a real coerce `Actor` with its own mailbox. Messages are
//! defined as separate types implementing `coerce::actor::message::Message`,
//! enabling coerce's native notify/send semantics.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::*;
use dactor::type_registry::TypeRegistry;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use coerce::actor::context::ActorContext;
use coerce::actor::message::{Handler, Message};
use coerce::actor::Actor;

// ---------------------------------------------------------------------------
// CP5: SpawnManager actor
// ---------------------------------------------------------------------------

/// Reply for spawn requests — wraps the outcome as a plain type so domain
/// failures (unknown type, bad args) stay as reply data.
///
/// The actor box is wrapped in `Mutex<Option<…>>` to satisfy `Sync`, which
/// coerce requires for all `Message::Result` types. Callers extract the
/// value via [`SpawnOutcome::take_actor`].
pub enum SpawnOutcome {
    /// Actor created successfully.
    Success {
        actor_id: ActorId,
        actor: Mutex<Option<Box<dyn std::any::Any + Send>>>,
    },
    /// Spawn failed (unknown type, deserialization error).
    Failure(SpawnResponse),
}

impl SpawnOutcome {
    /// Construct a `Success` variant, wrapping the actor in a `Mutex`.
    pub fn success(actor_id: ActorId, actor: Box<dyn std::any::Any + Send>) -> Self {
        Self::Success {
            actor_id,
            actor: Mutex::new(Some(actor)),
        }
    }

    /// Take the actor box out of a `Success` variant. Returns `None` if
    /// already taken or if the outcome is `Failure`.
    pub fn take_actor(&self) -> Option<Box<dyn std::any::Any + Send>> {
        match self {
            Self::Success { actor, .. } => actor.lock().unwrap_or_else(|e| e.into_inner()).take(),
            Self::Failure(_) => None,
        }
    }
}

/// Reply wrapper for CancelResponse.
pub struct CancelOutcome(pub CancelResponse);

/// Factory function type for creating actors from serialized bytes.
pub type FactoryFn = Box<
    dyn Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
        + Send
        + Sync,
>;

/// Native coerce actor wrapping [`SpawnManager`].
pub struct SpawnManagerActor {
    manager: SpawnManager,
    node_id: NodeId,
    next_local: Arc<AtomicU64>,
}

impl SpawnManagerActor {
    /// Create a new `SpawnManagerActor`.
    pub fn new(node_id: NodeId, registry: TypeRegistry, next_local: Arc<AtomicU64>) -> Self {
        Self {
            manager: SpawnManager::new(registry),
            node_id,
            next_local,
        }
    }
}

#[async_trait::async_trait]
impl Actor for SpawnManagerActor {}

/// Message: process a remote spawn request.
pub struct HandleSpawnRequest(pub SpawnRequest);

impl Message for HandleSpawnRequest {
    type Result = SpawnOutcome;
}

#[async_trait::async_trait]
impl Handler<HandleSpawnRequest> for SpawnManagerActor {
    async fn handle(&mut self, msg: HandleSpawnRequest, _ctx: &mut ActorContext) -> SpawnOutcome {
        match self.manager.create_actor(&msg.0) {
            Ok(actor) => {
                let local = self.next_local.fetch_add(1, Ordering::SeqCst);
                let actor_id = ActorId {
                    node: self.node_id.clone(),
                    local,
                };
                self.manager.record_spawn(actor_id.clone());
                SpawnOutcome::success(actor_id, actor)
            }
            Err(e) => SpawnOutcome::Failure(SpawnResponse::Failure {
                request_id: msg.0.request_id.clone(),
                error: e.to_string(),
            }),
        }
    }
}

/// Message: register a factory for a type name.
pub struct RegisterFactory {
    pub type_name: String,
    pub factory: FactoryFn,
}

impl Message for RegisterFactory {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<RegisterFactory> for SpawnManagerActor {
    async fn handle(&mut self, msg: RegisterFactory, _ctx: &mut ActorContext) {
        self.manager
            .type_registry_mut()
            .register_factory(msg.type_name, msg.factory);
    }
}

/// Message: query spawned actors list.
pub struct GetSpawnedActors;

impl Message for GetSpawnedActors {
    type Result = Vec<ActorId>;
}

#[async_trait::async_trait]
impl Handler<GetSpawnedActors> for SpawnManagerActor {
    async fn handle(&mut self, _msg: GetSpawnedActors, _ctx: &mut ActorContext) -> Vec<ActorId> {
        self.manager.spawned_actors().to_vec()
    }
}

// ---------------------------------------------------------------------------
// CP5: WatchManager actor
// ---------------------------------------------------------------------------

/// Native coerce actor wrapping [`WatchManager`].
pub struct WatchManagerActor {
    manager: WatchManager,
}

impl WatchManagerActor {
    /// Create a new `WatchManagerActor`.
    pub fn new() -> Self {
        Self {
            manager: WatchManager::new(),
        }
    }
}

impl Default for WatchManagerActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Actor for WatchManagerActor {}

/// Message: register a remote watch.
pub struct RemoteWatch {
    pub target: ActorId,
    pub watcher: ActorId,
}

impl Message for RemoteWatch {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<RemoteWatch> for WatchManagerActor {
    async fn handle(&mut self, msg: RemoteWatch, _ctx: &mut ActorContext) {
        self.manager.watch(msg.target, msg.watcher);
    }
}

/// Message: remove a remote watch.
pub struct RemoteUnwatch {
    pub target: ActorId,
    pub watcher: ActorId,
}

impl Message for RemoteUnwatch {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<RemoteUnwatch> for WatchManagerActor {
    async fn handle(&mut self, msg: RemoteUnwatch, _ctx: &mut ActorContext) {
        self.manager.unwatch(&msg.target, &msg.watcher);
    }
}

/// Message: actor terminated — return notifications for remote watchers.
pub struct OnTerminated(pub ActorId);

impl Message for OnTerminated {
    type Result = Vec<WatchNotification>;
}

#[async_trait::async_trait]
impl Handler<OnTerminated> for WatchManagerActor {
    async fn handle(
        &mut self,
        msg: OnTerminated,
        _ctx: &mut ActorContext,
    ) -> Vec<WatchNotification> {
        self.manager.on_terminated(&msg.0)
    }
}

/// Message: query watched count.
pub struct GetWatchedCount;

impl Message for GetWatchedCount {
    type Result = usize;
}

#[async_trait::async_trait]
impl Handler<GetWatchedCount> for WatchManagerActor {
    async fn handle(&mut self, _msg: GetWatchedCount, _ctx: &mut ActorContext) -> usize {
        self.manager.watched_count()
    }
}

// ---------------------------------------------------------------------------
// CP5: CancelManager actor
// ---------------------------------------------------------------------------

/// Native coerce actor wrapping [`CancelManager`].
pub struct CancelManagerActor {
    manager: CancelManager,
}

impl CancelManagerActor {
    /// Create a new `CancelManagerActor`.
    pub fn new() -> Self {
        Self {
            manager: CancelManager::new(),
        }
    }
}

impl Default for CancelManagerActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Actor for CancelManagerActor {}

/// Message: register a cancellation token for a request.
pub struct RegisterCancel {
    pub request_id: String,
    pub token: tokio_util::sync::CancellationToken,
}

impl Message for RegisterCancel {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<RegisterCancel> for CancelManagerActor {
    async fn handle(&mut self, msg: RegisterCancel, _ctx: &mut ActorContext) {
        self.manager.register(msg.request_id, msg.token);
    }
}

/// Message: cancel a request by ID.
pub struct CancelById(pub String);

impl Message for CancelById {
    type Result = CancelOutcome;
}

#[async_trait::async_trait]
impl Handler<CancelById> for CancelManagerActor {
    async fn handle(&mut self, msg: CancelById, _ctx: &mut ActorContext) -> CancelOutcome {
        CancelOutcome(self.manager.cancel(&msg.0))
    }
}

/// Message: clean up after a request completes normally.
pub struct CompleteRequest(pub String);

impl Message for CompleteRequest {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<CompleteRequest> for CancelManagerActor {
    async fn handle(&mut self, msg: CompleteRequest, _ctx: &mut ActorContext) {
        self.manager.remove(&msg.0);
    }
}

/// Message: query active count.
pub struct GetActiveCount;

impl Message for GetActiveCount {
    type Result = usize;
}

#[async_trait::async_trait]
impl Handler<GetActiveCount> for CancelManagerActor {
    async fn handle(&mut self, _msg: GetActiveCount, _ctx: &mut ActorContext) -> usize {
        self.manager.active_count()
    }
}

// ---------------------------------------------------------------------------
// CP5: NodeDirectory actor
// ---------------------------------------------------------------------------

/// Native coerce actor wrapping [`NodeDirectory`].
pub struct NodeDirectoryActor {
    directory: NodeDirectory,
}

impl NodeDirectoryActor {
    /// Create a new `NodeDirectoryActor`.
    pub fn new() -> Self {
        Self {
            directory: NodeDirectory::new(),
        }
    }
}

impl Default for NodeDirectoryActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Actor for NodeDirectoryActor {}

/// Message: register a peer node as connected.
pub struct ConnectPeer {
    pub peer_id: NodeId,
    pub address: Option<String>,
}

impl Message for ConnectPeer {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<ConnectPeer> for NodeDirectoryActor {
    async fn handle(&mut self, msg: ConnectPeer, _ctx: &mut ActorContext) {
        if let Some(existing) = self.directory.get_peer(&msg.peer_id) {
            let resolved = msg.address.or_else(|| existing.address.clone());
            self.directory.remove_peer(&msg.peer_id);
            self.directory.add_peer(msg.peer_id.clone(), resolved);
        } else {
            self.directory.add_peer(msg.peer_id.clone(), msg.address);
        }
        self.directory
            .set_status(&msg.peer_id, PeerStatus::Connected);
    }
}

/// Message: mark a peer as disconnected.
pub struct DisconnectPeer(pub NodeId);

impl Message for DisconnectPeer {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<DisconnectPeer> for NodeDirectoryActor {
    async fn handle(&mut self, msg: DisconnectPeer, _ctx: &mut ActorContext) {
        self.directory
            .set_status(&msg.0, PeerStatus::Disconnected);
    }
}

/// Message: check if a peer is connected.
pub struct IsConnected(pub NodeId);

impl Message for IsConnected {
    type Result = bool;
}

#[async_trait::async_trait]
impl Handler<IsConnected> for NodeDirectoryActor {
    async fn handle(&mut self, msg: IsConnected, _ctx: &mut ActorContext) -> bool {
        self.directory.is_connected(&msg.0)
    }
}

/// Message: query peer count.
pub struct GetPeerCount;

impl Message for GetPeerCount {
    type Result = usize;
}

#[async_trait::async_trait]
impl Handler<GetPeerCount> for NodeDirectoryActor {
    async fn handle(&mut self, _msg: GetPeerCount, _ctx: &mut ActorContext) -> usize {
        self.directory.peer_count()
    }
}

/// Message: query connected count.
pub struct GetConnectedCount;

impl Message for GetConnectedCount {
    type Result = usize;
}

#[async_trait::async_trait]
impl Handler<GetConnectedCount> for NodeDirectoryActor {
    async fn handle(&mut self, _msg: GetConnectedCount, _ctx: &mut ActorContext) -> usize {
        self.directory.connected_count()
    }
}

/// Message: query peer info.
pub struct GetPeerInfo(pub NodeId);

impl Message for GetPeerInfo {
    type Result = Option<PeerInfo>;
}

#[async_trait::async_trait]
impl Handler<GetPeerInfo> for NodeDirectoryActor {
    async fn handle(&mut self, msg: GetPeerInfo, _ctx: &mut ActorContext) -> Option<PeerInfo> {
        self.directory.get_peer(&msg.0).cloned()
    }
}
