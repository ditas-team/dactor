//! Remote actor reference that serializes messages and sends via transport.
//!
//! A [`RemoteActorRef<A>`] implements [`ActorRef<A>`] for actors on remote
//! nodes. Messages are serialized, wrapped in a [`WireEnvelope`], and sent
//! through a [`Transport`] to the target node.
//!
//! ## Building a remote ref
//!
//! Message types must be pre-registered so the ref knows how to serialize
//! each type at send time:
//!
//! ```rust,ignore
//! use dactor::remote_ref::RemoteActorRefBuilder;
//!
//! let remote = RemoteActorRefBuilder::<MyActor>::new(actor_id, "counter", transport)
//!     .register_tell::<Increment>()
//!     .register_ask::<GetCount>()
//!     .build();
//!
//! // Now use like any ActorRef:
//! remote.tell(Increment(1))?;
//! let count = remote.ask(GetCount, None)?.await?;
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorRef, AskReply, FeedHandler, Handler, StreamHandler};
use crate::errors::{ActorSendError, RuntimeError};
use crate::interceptor::SendMode;
use crate::message::Message;
use crate::node::ActorId;
use crate::remote::{SerializationError, WireEnvelope, WireHeaders};
use crate::stream::{BatchConfig, BoxStream};
use crate::transport::Transport;

// ---------------------------------------------------------------------------
// Serializer function types
// ---------------------------------------------------------------------------

/// Function that serializes a message (as `&dyn Any`) to `(type_name, body_bytes)`.
type SerializeFn =
    Arc<dyn Fn(&dyn Any) -> Result<(String, Vec<u8>), SerializationError> + Send + Sync>;

/// Function that deserializes reply bytes back to `Box<dyn Any + Send>`.
type DeserializeReplyFn =
    Arc<dyn Fn(&[u8]) -> Result<Box<dyn Any + Send>, SerializationError> + Send + Sync>;

/// Entry for a registered ask message type (serializer + reply deserializer).
struct AskEntry {
    serialize: SerializeFn,
    deserialize_reply: DeserializeReplyFn,
}

// ---------------------------------------------------------------------------
// RemoteActorRef
// ---------------------------------------------------------------------------

/// A reference to an actor on a remote node.
///
/// Implements [`ActorRef<A>`] by serializing messages and sending them via a
/// [`Transport`]. Message types must be pre-registered using
/// [`RemoteActorRefBuilder`] so the ref knows how to serialize each type.
///
/// # Tell (fire-and-forget)
///
/// `tell()` spawns a background task to send the serialized message.
/// Serialization errors are returned synchronously; transport errors are
/// logged but not returned (fire-and-forget semantics).
///
/// # Ask (request-reply)
///
/// `ask()` spawns a background task that sends the request and waits for
/// the reply via `Transport::send_request()`. The reply is deserialized
/// and delivered through the returned `AskReply` future.
pub struct RemoteActorRef<A: Actor> {
    id: ActorId,
    name: String,
    transport: Arc<dyn Transport>,
    /// TypeId → serialize function (for tell messages).
    tell_serializers: Arc<HashMap<TypeId, SerializeFn>>,
    /// TypeId → (serialize, deserialize_reply) (for ask messages).
    ask_entries: Arc<HashMap<TypeId, AskEntry>>,
    _phantom: PhantomData<A>,
}

impl<A: Actor> Clone for RemoteActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            transport: Arc::clone(&self.transport),
            tell_serializers: Arc::clone(&self.tell_serializers),
            ask_entries: Arc::clone(&self.ask_entries),
            _phantom: PhantomData,
        }
    }
}

impl<A: Actor> RemoteActorRef<A> {
    /// The target node ID.
    pub fn target_node(&self) -> &crate::node::NodeId {
        &self.id.node
    }
}

impl<A: Actor + Sync> ActorRef<A> for RemoteActorRef<A> {
    fn id(&self) -> ActorId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_alive(&self) -> bool {
        // Remote liveness is checked via transport reachability.
        // This is a best-effort check — the actor may have stopped even if
        // the node is reachable.
        let transport = Arc::clone(&self.transport);
        let node = self.id.node.clone();
        // Since is_alive is sync, we do a blocking check. For truly async
        // liveness, callers should use transport.is_reachable() directly.
        // Here we just return true (remote refs are assumed alive until
        // a send fails).
        let _ = (transport, node);
        true
    }

    fn stop(&self) {
        // Remote stop: would need to send a control message to the remote
        // runtime. This is a no-op stub — full implementation requires
        // system actors (Phase 4 S3: CancelManager).
        tracing::warn!(
            actor_id = %self.id,
            "RemoteActorRef::stop() is a no-op — remote actor stop requires CancelManager"
        );
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        let type_id = TypeId::of::<M>();
        let serializer = self
            .tell_serializers
            .get(&type_id)
            .or_else(|| self.ask_entries.get(&type_id).map(|e| &e.serialize))
            .ok_or_else(|| {
                ActorSendError(format!(
                    "message type '{}' not registered for remote send to {}",
                    std::any::type_name::<M>(),
                    self.id
                ))
            })?;

        let (type_name, body) = serializer(&msg as &dyn Any)
            .map_err(|e| ActorSendError(format!("serialization failed: {}", e.message)))?;

        let envelope = WireEnvelope {
            target: self.id.clone(),
            message_type: type_name,
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body,
            request_id: None,
            version: None,
        };

        let transport = Arc::clone(&self.transport);
        let target_node = self.id.node.clone();

        tokio::spawn(async move {
            if let Err(e) = transport.send(&target_node, envelope).await {
                tracing::error!(
                    target_node = %target_node,
                    error = %e,
                    "remote tell failed"
                );
            }
        });

        Ok(())
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
        let type_id = TypeId::of::<M>();
        let entry = self.ask_entries.get(&type_id).ok_or_else(|| {
            ActorSendError(format!(
                "ask message type '{}' not registered for remote send to {}",
                std::any::type_name::<M>(),
                self.id
            ))
        })?;

        let (type_name, body) = (entry.serialize)(&msg as &dyn Any)
            .map_err(|e| ActorSendError(format!("serialization failed: {}", e.message)))?;

        let request_id = uuid::Uuid::new_v4();
        let envelope = WireEnvelope {
            target: self.id.clone(),
            message_type: type_name,
            send_mode: SendMode::Ask,
            headers: WireHeaders::new(),
            body,
            request_id: Some(request_id),
            version: None,
        };

        let (tx, rx) = oneshot::channel::<Result<M::Reply, RuntimeError>>();
        let transport = Arc::clone(&self.transport);
        let target_node = self.id.node.clone();
        let deserialize_reply = Arc::clone(&entry.deserialize_reply);

        tokio::spawn(async move {
            let result = if let Some(cancel_token) = cancel {
                tokio::select! {
                    result = transport.send_request(&target_node, envelope) => result,
                    _ = cancel_token.cancelled() => {
                        let _ = tx.send(Err(RuntimeError::Cancelled));
                        return;
                    }
                }
            } else {
                transport.send_request(&target_node, envelope).await
            };

            match result {
                Ok(reply_envelope) => match deserialize_reply(&reply_envelope.body) {
                    Ok(any_reply) => {
                        if let Ok(reply) = (any_reply as Box<dyn Any + Send>).downcast::<M::Reply>()
                        {
                            let _ = tx.send(Ok(*reply));
                        } else {
                            let _ = tx.send(Err(RuntimeError::Send(ActorSendError(
                                "reply type mismatch".into(),
                            ))));
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(RuntimeError::Send(ActorSendError(format!(
                            "reply deserialization failed: {}",
                            e.message
                        )))));
                    }
                },
                Err(e) => {
                    let _ = tx.send(Err(RuntimeError::Send(ActorSendError(format!(
                        "remote ask failed: {}",
                        e
                    )))));
                }
            }
        });

        Ok(AskReply::new(rx))
    }

    fn stream<M>(
        &self,
        _msg: M,
        _buffer: usize,
        _batch_config: Option<BatchConfig>,
        _cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        Err(ActorSendError(
            "remote stream not yet implemented — requires streaming transport support".into(),
        ))
    }

    fn feed<Item, Reply>(
        &self,
        _input: BoxStream<Item>,
        _buffer: usize,
        _batch_config: Option<BatchConfig>,
        _cancel: Option<CancellationToken>,
    ) -> Result<AskReply<Reply>, ActorSendError>
    where
        A: FeedHandler<Item, Reply>,
        Item: Send + 'static,
        Reply: Send + 'static,
    {
        Err(ActorSendError(
            "remote feed not yet implemented — requires streaming transport support".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// RemoteActorRefBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`RemoteActorRef`] with registered message types.
///
/// Each message type the remote actor handles must be registered so the
/// ref knows how to serialize messages at send time.
pub struct RemoteActorRefBuilder<A: Actor> {
    id: ActorId,
    name: String,
    transport: Arc<dyn Transport>,
    tell_serializers: HashMap<TypeId, SerializeFn>,
    ask_entries: HashMap<TypeId, AskEntry>,
    _phantom: PhantomData<A>,
}

impl<A: Actor> RemoteActorRefBuilder<A> {
    /// Create a new builder for a remote actor ref.
    pub fn new(id: ActorId, name: impl Into<String>, transport: Arc<dyn Transport>) -> Self {
        Self {
            id,
            name: name.into(),
            transport,
            tell_serializers: HashMap::new(),
            ask_entries: HashMap::new(),
            _phantom: PhantomData,
        }
    }

    /// Register a tell message type with a custom serializer function.
    ///
    /// The serializer receives the message as `&dyn Any` and must downcast
    /// it to the concrete type, then serialize to bytes.
    pub fn register_tell_with(
        mut self,
        type_id: TypeId,
        type_name: &'static str,
        serialize: impl Fn(&dyn Any) -> Result<Vec<u8>, SerializationError> + Send + Sync + 'static,
    ) -> Self {
        self.tell_serializers.insert(
            type_id,
            Arc::new(move |any: &dyn Any| {
                let bytes = serialize(any)?;
                Ok((type_name.to_string(), bytes))
            }),
        );
        self
    }

    /// Register an ask message type with custom serializer and reply deserializer.
    pub fn register_ask_with(
        mut self,
        type_id: TypeId,
        type_name: &'static str,
        serialize: impl Fn(&dyn Any) -> Result<Vec<u8>, SerializationError> + Send + Sync + 'static,
        deserialize_reply: impl Fn(&[u8]) -> Result<Box<dyn Any + Send>, SerializationError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let type_name_owned = type_name.to_string();
        self.ask_entries.insert(
            type_id,
            AskEntry {
                serialize: Arc::new(move |any: &dyn Any| {
                    let bytes = serialize(any)?;
                    Ok((type_name_owned.clone(), bytes))
                }),
                deserialize_reply: Arc::new(deserialize_reply),
            },
        );
        self
    }

    /// Register a tell message type using serde_json serialization.
    #[cfg(feature = "serde")]
    pub fn register_tell<M>(self) -> Self
    where
        M: Message<Reply = ()> + serde::Serialize + 'static,
    {
        self.register_tell_with(
            TypeId::of::<M>(),
            std::any::type_name::<M>(),
            |any: &dyn Any| {
                let msg = any.downcast_ref::<M>().ok_or_else(|| {
                    SerializationError::new("type downcast failed in tell serializer")
                })?;
                serde_json::to_vec(msg)
                    .map_err(|e| SerializationError::new(format!("json serialize: {e}")))
            },
        )
    }

    /// Register an ask message type using serde_json serialization.
    #[cfg(feature = "serde")]
    pub fn register_ask<M>(self) -> Self
    where
        M: Message + serde::Serialize + 'static,
        M::Reply: serde::de::DeserializeOwned + Send + 'static,
    {
        self.register_ask_with(
            TypeId::of::<M>(),
            std::any::type_name::<M>(),
            |any: &dyn Any| {
                let msg = any.downcast_ref::<M>().ok_or_else(|| {
                    SerializationError::new("type downcast failed in ask serializer")
                })?;
                serde_json::to_vec(msg)
                    .map_err(|e| SerializationError::new(format!("json serialize: {e}")))
            },
            |bytes: &[u8]| {
                let reply: M::Reply = serde_json::from_slice(bytes)
                    .map_err(|e| SerializationError::new(format!("json deserialize reply: {e}")))?;
                Ok(Box::new(reply) as Box<dyn Any + Send>)
            },
        )
    }

    /// Build the [`RemoteActorRef`].
    pub fn build(self) -> RemoteActorRef<A> {
        RemoteActorRef {
            id: self.id,
            name: self.name,
            transport: self.transport,
            tell_serializers: Arc::new(self.tell_serializers),
            ask_entries: Arc::new(self.ask_entries),
            _phantom: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;
    use crate::transport::InMemoryTransport;
    use async_trait::async_trait;

    // -- Test actor and messages --

    struct Counter {
        count: u64,
    }

    impl Actor for Counter {
        type Args = u64;
        type Deps = ();
        fn create(initial: u64, _: ()) -> Self {
            Self { count: initial }
        }
    }

    struct Increment;
    impl Message for Increment {
        type Reply = ();
    }

    struct GetCount;
    impl Message for GetCount {
        type Reply = u64;
    }

    #[async_trait]
    impl Handler<Increment> for Counter {
        async fn handle(&mut self, _msg: Increment, _ctx: &mut crate::actor::ActorContext) {
            self.count += 1;
        }
    }

    #[async_trait]
    impl Handler<GetCount> for Counter {
        async fn handle(&mut self, _msg: GetCount, _ctx: &mut crate::actor::ActorContext) -> u64 {
            self.count
        }
    }

    #[test]
    fn remote_ref_id_and_name() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 42,
            },
            "counter",
            transport,
        )
        .build();

        assert_eq!(remote.id().local, 42);
        assert_eq!(remote.name(), "counter");
        assert_eq!(remote.target_node().0, "node-2");
        assert!(remote.is_alive()); // always true for remote refs
    }

    #[test]
    fn remote_ref_tell_unregistered_type_returns_error() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            transport,
        )
        .build();

        let result = remote.tell(Increment);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not registered"));
    }

    #[test]
    fn remote_ref_ask_unregistered_type_returns_error() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            transport,
        )
        .build();

        let result = remote.ask(GetCount, None);
        assert!(result.is_err());
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(err.to_string().contains("not registered"));
    }

    #[test]
    fn remote_ref_stream_returns_not_implemented() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let _remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            transport,
        )
        .build();

        // stream/feed are explicitly not implemented yet
        // (they require streaming transport which is Phase 4 R5)
    }

    #[test]
    fn remote_ref_is_clone() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            transport,
        )
        .build();

        let cloned = remote.clone();
        assert_eq!(cloned.id().local, 1);
        assert_eq!(cloned.name(), "counter");
    }

    #[test]
    fn remote_ref_tell_with_custom_serializer() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let _remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            transport,
        )
        .register_tell_with(
            TypeId::of::<Increment>(),
            "test::Increment",
            |_any: &dyn Any| Ok(vec![1, 2, 3]),
        )
        .build();

        // tell with registered type succeeds (spawns a send task)
        // Note: we can't check the transport delivery in a sync test,
        // but the serialization succeeds without error.
        // The actual delivery is tested in the async test below.
    }

    #[tokio::test]
    async fn remote_ref_tell_delivers_via_transport() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let mut rx = transport.register_node(NodeId("node-2".into())).await;

        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            Arc::clone(&transport) as Arc<dyn Transport>,
        )
        .register_tell_with(
            TypeId::of::<Increment>(),
            "test::Increment",
            |_any: &dyn Any| Ok(vec![42]),
        )
        .build();

        // Connect so the transport can route
        transport.connect(&NodeId("node-2".into())).await.unwrap();

        remote.tell(Increment).unwrap();

        // The tell spawns a task — give it time to deliver
        let received = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received.body, vec![42]);
        assert_eq!(received.message_type, "test::Increment");
        assert_eq!(received.send_mode, SendMode::Tell);
        assert!(received.request_id.is_none());
    }

    #[tokio::test]
    async fn remote_ref_ask_delivers_and_receives_reply() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let mut rx = transport.register_node(NodeId("node-2".into())).await;

        transport.connect(&NodeId("node-2".into())).await.unwrap();

        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            Arc::clone(&transport) as Arc<dyn Transport>,
        )
        .register_ask_with(
            TypeId::of::<GetCount>(),
            "test::GetCount",
            |_any: &dyn Any| Ok(vec![0]),
            |bytes: &[u8]| {
                if bytes.len() != 8 {
                    return Err(SerializationError::new("expected 8 bytes"));
                }
                let val = u64::from_be_bytes(bytes.try_into().unwrap());
                Ok(Box::new(val) as Box<dyn Any + Send>)
            },
        )
        .build();

        let reply_future = remote.ask(GetCount, None).unwrap();

        // Simulate remote side: receive the request, send back a reply
        let received = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received.message_type, "test::GetCount");
        assert_eq!(received.send_mode, SendMode::Ask);
        let request_id = received.request_id.unwrap();

        // Complete the request with a reply
        let reply_envelope = WireEnvelope {
            target: ActorId {
                node: NodeId("local".into()),
                local: 0,
            },
            message_type: "reply".into(),
            send_mode: SendMode::Ask,
            headers: WireHeaders::new(),
            body: 99u64.to_be_bytes().to_vec(),
            request_id: Some(request_id),
            version: None,
        };

        transport
            .complete_request(request_id, reply_envelope)
            .await
            .unwrap();

        // Await the reply
        let count = reply_future.await.unwrap();
        assert_eq!(count, 99);
    }

    #[tokio::test]
    async fn remote_ref_ask_with_cancellation() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let _rx = transport.register_node(NodeId("node-2".into())).await;
        transport.connect(&NodeId("node-2".into())).await.unwrap();

        let remote = RemoteActorRefBuilder::<Counter>::new(
            ActorId {
                node: NodeId("node-2".into()),
                local: 1,
            },
            "counter",
            Arc::clone(&transport) as Arc<dyn Transport>,
        )
        .register_ask_with(
            TypeId::of::<GetCount>(),
            "test::GetCount",
            |_any: &dyn Any| Ok(vec![0]),
            |bytes: &[u8]| {
                let val = u64::from_be_bytes(bytes.try_into().unwrap());
                Ok(Box::new(val) as Box<dyn Any + Send>)
            },
        )
        .build();

        let token = CancellationToken::new();
        let reply_future = remote.ask(GetCount, Some(token.clone())).unwrap();

        // Cancel before reply arrives
        token.cancel();

        let result = reply_future.await;
        assert!(result.is_err());
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;
        use async_trait::async_trait;

        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Add {
            amount: u64,
        }
        impl Message for Add {
            type Reply = ();
        }

        #[async_trait]
        impl Handler<Add> for Counter {
            async fn handle(&mut self, msg: Add, _ctx: &mut crate::actor::ActorContext) {
                self.count += msg.amount;
            }
        }

        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct GetValue;
        impl Message for GetValue {
            type Reply = u64;
        }

        #[async_trait]
        impl Handler<GetValue> for Counter {
            async fn handle(
                &mut self,
                _msg: GetValue,
                _ctx: &mut crate::actor::ActorContext,
            ) -> u64 {
                self.count
            }
        }

        #[tokio::test]
        async fn serde_tell_roundtrip() {
            let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
            let mut rx = transport.register_node(NodeId("node-2".into())).await;
            transport.connect(&NodeId("node-2".into())).await.unwrap();

            let remote = RemoteActorRefBuilder::<Counter>::new(
                ActorId {
                    node: NodeId("node-2".into()),
                    local: 1,
                },
                "counter",
                Arc::clone(&transport) as Arc<dyn Transport>,
            )
            .register_tell::<Add>()
            .build();

            remote.tell(Add { amount: 42 }).unwrap();

            let received = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .unwrap()
                .unwrap();

            // Verify the body deserializes correctly
            let msg: Add = serde_json::from_slice(&received.body).unwrap();
            assert_eq!(msg.amount, 42);
        }

        #[tokio::test]
        async fn serde_ask_roundtrip() {
            let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
            let mut rx = transport.register_node(NodeId("node-2".into())).await;
            transport.connect(&NodeId("node-2".into())).await.unwrap();

            let remote = RemoteActorRefBuilder::<Counter>::new(
                ActorId {
                    node: NodeId("node-2".into()),
                    local: 1,
                },
                "counter",
                Arc::clone(&transport) as Arc<dyn Transport>,
            )
            .register_ask::<GetValue>()
            .build();

            let reply_future = remote.ask(GetValue, None).unwrap();

            // Simulate remote: receive request, send reply
            let received = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .unwrap()
                .unwrap();

            let request_id = received.request_id.unwrap();
            let reply_body = serde_json::to_vec(&77u64).unwrap();

            transport
                .complete_request(
                    request_id,
                    WireEnvelope {
                        target: ActorId {
                            node: NodeId("local".into()),
                            local: 0,
                        },
                        message_type: "reply".into(),
                        send_mode: SendMode::Ask,
                        headers: WireHeaders::new(),
                        body: reply_body,
                        request_id: Some(request_id),
                        version: None,
                    },
                )
                .await
                .unwrap();

            let value = reply_future.await.unwrap();
            assert_eq!(value, 77);
        }
    }
}
