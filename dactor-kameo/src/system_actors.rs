//! Native kameo actor implementations for dactor system actors.
//!
//! Each system actor (SpawnManager, WatchManager, CancelManager, NodeDirectory)
//! is wrapped in a real `kameo::Actor` with its own mailbox. Messages are
//! defined as separate types implementing `kameo::message::Message`, enabling
//! kameo's native tell/ask semantics.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::*;
use dactor::type_registry::TypeRegistry;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// NA5: SpawnManager actor
// ---------------------------------------------------------------------------

/// Result type for spawn requests.
pub type SpawnResult = Result<(ActorId, Box<dyn std::any::Any + Send>), SpawnResponse>;

/// Factory function type for creating actors from serialized bytes.
pub type FactoryFn = Box<
    dyn Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
        + Send
        + Sync,
>;

/// Native kameo actor wrapping [`SpawnManager`].
pub struct SpawnManagerActor {
    manager: SpawnManager,
    node_id: NodeId,
    /// Shared ID counter — must be the same `Arc` used by the runtime
    /// to prevent local/remote ActorId collisions.
    next_local: Arc<AtomicU64>,
}

impl kameo::Actor for SpawnManagerActor {
    type Args = (NodeId, TypeRegistry, Arc<AtomicU64>);
    type Error = kameo::error::Infallible;

    async fn on_start(
        args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            manager: SpawnManager::new(args.1),
            node_id: args.0,
            next_local: args.2,
        })
    }
}

/// Message: process a remote spawn request.
pub struct HandleSpawnRequest(pub SpawnRequest);

impl kameo::message::Message<HandleSpawnRequest> for SpawnManagerActor {
    type Reply = SpawnResult;

    async fn handle(
        &mut self,
        msg: HandleSpawnRequest,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match self.manager.create_actor(&msg.0) {
            Ok(actor) => {
                let local = self.next_local.fetch_add(1, Ordering::SeqCst);
                let actor_id = ActorId {
                    node: self.node_id.clone(),
                    local,
                };
                self.manager.record_spawn(actor_id.clone());
                Ok((actor_id, actor))
            }
            Err(e) => Err(SpawnResponse::Failure {
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

impl kameo::message::Message<RegisterFactory> for SpawnManagerActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterFactory,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.manager
            .type_registry_mut()
            .register_factory(msg.type_name, msg.factory);
    }
}

/// Message: query spawned actors list.
pub struct GetSpawnedActors;

impl kameo::message::Message<GetSpawnedActors> for SpawnManagerActor {
    type Reply = Vec<ActorId>;

    async fn handle(
        &mut self,
        _msg: GetSpawnedActors,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.manager.spawned_actors().to_vec()
    }
}

// ---------------------------------------------------------------------------
// NA6: WatchManager actor
// ---------------------------------------------------------------------------

/// Native kameo actor wrapping [`WatchManager`].
pub struct WatchManagerActor {
    manager: WatchManager,
}

impl kameo::Actor for WatchManagerActor {
    type Args = ();
    type Error = kameo::error::Infallible;

    async fn on_start(
        _args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            manager: WatchManager::new(),
        })
    }
}

/// Message: register a remote watch.
pub struct RemoteWatch {
    pub target: ActorId,
    pub watcher: ActorId,
}

impl kameo::message::Message<RemoteWatch> for WatchManagerActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RemoteWatch,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.manager.watch(msg.target, msg.watcher);
    }
}

/// Message: remove a remote watch.
pub struct RemoteUnwatch {
    pub target: ActorId,
    pub watcher: ActorId,
}

impl kameo::message::Message<RemoteUnwatch> for WatchManagerActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RemoteUnwatch,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.manager.unwatch(&msg.target, &msg.watcher);
    }
}

/// Message: actor terminated — return notifications for remote watchers.
pub struct OnTerminated(pub ActorId);

impl kameo::message::Message<OnTerminated> for WatchManagerActor {
    type Reply = Vec<WatchNotification>;

    async fn handle(
        &mut self,
        msg: OnTerminated,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.manager.on_terminated(&msg.0)
    }
}

/// Message: query watched count.
pub struct GetWatchedCount;

impl kameo::message::Message<GetWatchedCount> for WatchManagerActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetWatchedCount,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.manager.watched_count()
    }
}

// ---------------------------------------------------------------------------
// NA7: CancelManager actor
// ---------------------------------------------------------------------------

/// Native kameo actor wrapping [`CancelManager`].
pub struct CancelManagerActor {
    manager: CancelManager,
}

impl kameo::Actor for CancelManagerActor {
    type Args = ();
    type Error = kameo::error::Infallible;

    async fn on_start(
        _args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            manager: CancelManager::new(),
        })
    }
}

/// Message: register a cancellation token for a request.
pub struct RegisterCancel {
    pub request_id: String,
    pub token: tokio_util::sync::CancellationToken,
}

impl kameo::message::Message<RegisterCancel> for CancelManagerActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterCancel,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.manager.register(msg.request_id, msg.token);
    }
}

/// Message: cancel a request by ID.
pub struct CancelById(pub String);

impl kameo::message::Message<CancelById> for CancelManagerActor {
    type Reply = Result<CancelResponse, kameo::error::Infallible>;

    async fn handle(
        &mut self,
        msg: CancelById,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.manager.cancel(&msg.0))
    }
}

/// Message: clean up after a request completes normally.
pub struct CompleteRequest(pub String);

impl kameo::message::Message<CompleteRequest> for CancelManagerActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CompleteRequest,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.manager.remove(&msg.0);
    }
}

/// Message: query active count.
pub struct GetActiveCount;

impl kameo::message::Message<GetActiveCount> for CancelManagerActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetActiveCount,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.manager.active_count()
    }
}

// ---------------------------------------------------------------------------
// NA8: NodeDirectory actor
// ---------------------------------------------------------------------------

/// Native kameo actor wrapping [`NodeDirectory`].
pub struct NodeDirectoryActor {
    directory: NodeDirectory,
}

impl kameo::Actor for NodeDirectoryActor {
    type Args = ();
    type Error = kameo::error::Infallible;

    async fn on_start(
        _args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            directory: NodeDirectory::new(),
        })
    }
}

/// Message: register a peer node as connected.
pub struct ConnectPeer {
    pub peer_id: NodeId,
    pub address: Option<String>,
}

impl kameo::message::Message<ConnectPeer> for NodeDirectoryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ConnectPeer,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
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

impl kameo::message::Message<DisconnectPeer> for NodeDirectoryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: DisconnectPeer,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.directory
            .set_status(&msg.0, PeerStatus::Disconnected);
    }
}

/// Message: check if a peer is connected.
pub struct IsConnected(pub NodeId);

impl kameo::message::Message<IsConnected> for NodeDirectoryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsConnected,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.directory.is_connected(&msg.0)
    }
}

/// Message: query peer count.
pub struct GetPeerCount;

impl kameo::message::Message<GetPeerCount> for NodeDirectoryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetPeerCount,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.directory.peer_count()
    }
}

/// Message: query connected count.
pub struct GetConnectedCount;

impl kameo::message::Message<GetConnectedCount> for NodeDirectoryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetConnectedCount,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.directory.connected_count()
    }
}

/// Message: query peer info.
pub struct GetPeerInfo(pub NodeId);

impl kameo::message::Message<GetPeerInfo> for NodeDirectoryActor {
    type Reply = Option<PeerInfo>;

    async fn handle(
        &mut self,
        msg: GetPeerInfo,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.directory.get_peer(&msg.0).cloned()
    }
}
