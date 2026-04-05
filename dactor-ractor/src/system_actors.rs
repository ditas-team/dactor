//! Native ractor actor implementations for dactor system actors.
//!
//! Each system actor (SpawnManager, WatchManager, CancelManager, NodeDirectory)
//! is wrapped in a real `ractor::Actor` with its own mailbox. This enables:
//!
//! - **Message-based access** — system actor operations arrive as mailbox
//!   messages, processed single-threaded (no `&mut self` contention).
//! - **Transport integration** — incoming `WireEnvelope` system messages can
//!   be routed directly to the actor's mailbox.
//! - **Backpressure** — mailbox depth limits prevent system actor overload.
//! - **Supervision** — system actors can be supervised and restarted.

use dactor::node::{ActorId, NodeId};
use dactor::system_actors::*;
use dactor::type_registry::TypeRegistry;

use std::sync::Arc;
use tokio::sync::oneshot;

/// Result type for spawn requests.
pub type SpawnResult = Result<(ActorId, Box<dyn std::any::Any + Send>), SpawnResponse>;

/// Factory function type for creating actors from serialized bytes.
pub type FactoryFn = Box<
    dyn Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, dactor::remote::SerializationError>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// NA1: SpawnManager actor
// ---------------------------------------------------------------------------

/// Message enum for the SpawnManager actor.
pub enum SpawnManagerMsg {
    /// Process a remote spawn request.
    HandleRequest {
        request: SpawnRequest,
        reply: oneshot::Sender<SpawnResult>,
    },
    /// Register a factory for a type name.
    RegisterFactory {
        type_name: String,
        factory: FactoryFn,
        reply: oneshot::Sender<()>,
    },
    /// Query spawned actors list.
    GetSpawnedActors {
        reply: oneshot::Sender<Vec<ActorId>>,
    },
}

/// Native ractor actor wrapping [`SpawnManager`].
pub struct SpawnManagerActor;

/// Internal state for SpawnManagerActor.
pub struct SpawnManagerState {
    manager: SpawnManager,
    node_id: NodeId,
    /// Shared ID counter — must be the same `Arc` used by the runtime
    /// to prevent local/remote ActorId collisions.
    next_local: Arc<std::sync::atomic::AtomicU64>,
}

impl ractor::Actor for SpawnManagerActor {
    type Msg = SpawnManagerMsg;
    type State = SpawnManagerState;
    type Arguments = (NodeId, TypeRegistry, Arc<std::sync::atomic::AtomicU64>);

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        Ok(SpawnManagerState {
            manager: SpawnManager::new(args.1),
            node_id: args.0,
            next_local: args.2,
        })
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        match message {
            SpawnManagerMsg::HandleRequest { request, reply } => {
                let result = match state.manager.create_actor(&request) {
                    Ok(actor) => {
                        let local = state
                            .next_local
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let actor_id = ActorId {
                            node: state.node_id.clone(),
                            local,
                        };
                        state.manager.record_spawn(actor_id.clone());
                        Ok((actor_id, actor))
                    }
                    Err(e) => Err(SpawnResponse::Failure {
                        request_id: request.request_id.clone(),
                        error: e.to_string(),
                    }),
                };
                let _ = reply.send(result);
            }
            SpawnManagerMsg::RegisterFactory {
                type_name,
                factory,
                reply,
            } => {
                state
                    .manager
                    .type_registry_mut()
                    .register_factory(type_name, factory);
                let _ = reply.send(());
            }
            SpawnManagerMsg::GetSpawnedActors { reply } => {
                let _ = reply.send(state.manager.spawned_actors().to_vec());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NA2: WatchManager actor
// ---------------------------------------------------------------------------

/// Message enum for the WatchManager actor.
pub enum WatchManagerMsg {
    /// Register a remote watch.
    Watch { target: ActorId, watcher: ActorId },
    /// Remove a remote watch.
    Unwatch { target: ActorId, watcher: ActorId },
    /// Actor terminated — return notifications for remote watchers.
    OnTerminated {
        terminated: ActorId,
        reply: oneshot::Sender<Vec<WatchNotification>>,
    },
    /// Query watched count.
    GetWatchedCount {
        reply: oneshot::Sender<usize>,
    },
}

/// Native ractor actor wrapping [`WatchManager`].
pub struct WatchManagerActor;

impl ractor::Actor for WatchManagerActor {
    type Msg = WatchManagerMsg;
    type State = WatchManager;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        Ok(WatchManager::new())
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        match message {
            WatchManagerMsg::Watch { target, watcher } => {
                state.watch(target, watcher);
            }
            WatchManagerMsg::Unwatch { target, watcher } => {
                state.unwatch(&target, &watcher);
            }
            WatchManagerMsg::OnTerminated { terminated, reply } => {
                let notifications = state.on_terminated(&terminated);
                let _ = reply.send(notifications);
            }
            WatchManagerMsg::GetWatchedCount { reply } => {
                let _ = reply.send(state.watched_count());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NA3: CancelManager actor
// ---------------------------------------------------------------------------

/// Message enum for the CancelManager actor.
pub enum CancelManagerMsg {
    /// Register a cancellation token for a request.
    Register {
        request_id: String,
        token: tokio_util::sync::CancellationToken,
    },
    /// Cancel a request by ID.
    Cancel {
        request_id: String,
        reply: oneshot::Sender<CancelResponse>,
    },
    /// Clean up after a request completes normally.
    Complete { request_id: String },
    /// Query active count.
    GetActiveCount {
        reply: oneshot::Sender<usize>,
    },
}

/// Native ractor actor wrapping [`CancelManager`].
pub struct CancelManagerActor;

impl ractor::Actor for CancelManagerActor {
    type Msg = CancelManagerMsg;
    type State = CancelManager;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        Ok(CancelManager::new())
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        match message {
            CancelManagerMsg::Register { request_id, token } => {
                state.register(request_id, token);
            }
            CancelManagerMsg::Cancel { request_id, reply } => {
                let response = state.cancel(&request_id);
                let _ = reply.send(response);
            }
            CancelManagerMsg::Complete { request_id } => {
                state.remove(&request_id);
            }
            CancelManagerMsg::GetActiveCount { reply } => {
                let _ = reply.send(state.active_count());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NA4: NodeDirectory actor
// ---------------------------------------------------------------------------

/// Message enum for the NodeDirectory actor.
pub enum NodeDirectoryMsg {
    /// Register a peer node as connected.
    ConnectPeer {
        peer_id: NodeId,
        address: Option<String>,
    },
    /// Mark a peer as disconnected.
    DisconnectPeer { peer_id: NodeId },
    /// Check if a peer is connected.
    IsConnected {
        peer_id: NodeId,
        reply: oneshot::Sender<bool>,
    },
    /// Query peer count.
    GetPeerCount {
        reply: oneshot::Sender<usize>,
    },
    /// Query connected count.
    GetConnectedCount {
        reply: oneshot::Sender<usize>,
    },
    /// Query peer info (address, status).
    GetPeerInfo {
        peer_id: NodeId,
        reply: oneshot::Sender<Option<PeerInfo>>,
    },
}

/// Native ractor actor wrapping [`NodeDirectory`].
pub struct NodeDirectoryActor;

impl ractor::Actor for NodeDirectoryActor {
    type Msg = NodeDirectoryMsg;
    type State = NodeDirectory;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        Ok(NodeDirectory::new())
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        match message {
            NodeDirectoryMsg::ConnectPeer { peer_id, address } => {
                if let Some(existing) = state.get_peer(&peer_id) {
                    let resolved = address.or_else(|| existing.address.clone());
                    state.remove_peer(&peer_id);
                    state.add_peer(peer_id.clone(), resolved);
                } else {
                    state.add_peer(peer_id.clone(), address);
                }
                state.set_status(&peer_id, PeerStatus::Connected);
            }
            NodeDirectoryMsg::DisconnectPeer { peer_id } => {
                state.set_status(&peer_id, PeerStatus::Disconnected);
            }
            NodeDirectoryMsg::IsConnected { peer_id, reply } => {
                let _ = reply.send(state.is_connected(&peer_id));
            }
            NodeDirectoryMsg::GetPeerCount { reply } => {
                let _ = reply.send(state.peer_count());
            }
            NodeDirectoryMsg::GetConnectedCount { reply } => {
                let _ = reply.send(state.connected_count());
            }
            NodeDirectoryMsg::GetPeerInfo { peer_id, reply } => {
                let _ = reply.send(state.get_peer(&peer_id).cloned());
            }
        }
        Ok(())
    }
}
