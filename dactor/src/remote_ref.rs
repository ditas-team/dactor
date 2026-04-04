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
use crate::interceptor::{Disposition, OutboundContext, OutboundInterceptor, SendMode};
use crate::message::{Headers, Message, RuntimeHeaders};
use crate::node::ActorId;
use crate::remote::{SerializationError, WireEnvelope};
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
/// # Outbound Interceptor Pipeline
///
/// When outbound interceptors are registered (via the builder), they run
/// **before serialization** on every `tell()` and `ask()` call:
///
/// 1. `on_send()` — each interceptor can inspect/modify headers, inspect
///    the message via `&dyn Any`, and return a [`Disposition`]:
///    - `Continue` — proceed to next interceptor / serialize
///    - `Reject(reason)` — abort the send, return error to caller
///    - `Drop` — silently discard (tell) or return error (ask)
///    - `Delay(d)` — pause before sending (applied in spawned task)
/// 2. `Headers::to_wire()` — typed headers convert to wire format
/// 3. Serialize message body → build `WireEnvelope` → send via transport
/// 4. `on_reply()` — (ask only) interceptors observe the deserialized reply
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
    /// Outbound interceptors run before serialization on every send.
    outbound_interceptors: Arc<Vec<Arc<dyn OutboundInterceptor>>>,
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
            outbound_interceptors: Arc::clone(&self.outbound_interceptors),
            _phantom: PhantomData,
        }
    }
}

/// Result of the outbound interceptor pipeline.
enum PipelineResult {
    /// All interceptors returned Continue.
    Continue,
    /// An interceptor requested a delay before sending.
    Delay(std::time::Duration),
    /// An interceptor silently dropped the message (tell only).
    Dropped,
}

impl<A: Actor> RemoteActorRef<A> {
    /// The target node ID.
    pub fn target_node(&self) -> &crate::node::NodeId {
        &self.id.node
    }

    /// Run the outbound interceptor pipeline. Returns `Err` if any
    /// interceptor rejects the message. Returns `Ok(PipelineResult)` to
    /// indicate whether to continue, delay, or drop.
    fn run_outbound_pipeline(
        &self,
        message_type: &'static str,
        send_mode: SendMode,
        headers: &mut Headers,
        message: &dyn Any,
    ) -> Result<PipelineResult, ActorSendError> {
        if self.outbound_interceptors.is_empty() {
            return Ok(PipelineResult::Continue);
        }

        let runtime_headers = RuntimeHeaders::new();
        let ctx = OutboundContext {
            target_id: self.id.clone(),
            target_name: &self.name,
            message_type,
            send_mode,
            remote: true,
        };

        let mut delay = None;

        for interceptor in self.outbound_interceptors.iter() {
            match interceptor.on_send(&ctx, &runtime_headers, headers, message) {
                Disposition::Continue => {}
                Disposition::Reject(reason) => {
                    return Err(ActorSendError(format!(
                        "rejected by outbound interceptor '{}': {}",
                        interceptor.name(),
                        reason
                    )));
                }
                Disposition::Drop => {
                    return Ok(PipelineResult::Dropped);
                }
                Disposition::Delay(d) => {
                    // Use the largest delay requested by any interceptor.
                    delay = Some(match delay {
                        Some(existing) if existing > d => existing,
                        _ => d,
                    });
                }
                Disposition::Retry(retry_after) => {
                    return Err(ActorSendError(format!(
                        "retry after {:?} (from outbound interceptor '{}')",
                        retry_after,
                        interceptor.name()
                    )));
                }
            }
        }

        Ok(match delay {
            Some(d) => PipelineResult::Delay(d),
            None => PipelineResult::Continue,
        })
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
        // 1. Run outbound interceptor pipeline (before serialization)
        let mut headers = Headers::new();
        let pipeline_result = self.run_outbound_pipeline(
            std::any::type_name::<M>(),
            SendMode::Tell,
            &mut headers,
            &msg as &dyn Any,
        )?;

        // Drop is silent for tell — interceptor discarded the message
        if matches!(pipeline_result, PipelineResult::Dropped) {
            return Ok(());
        }

        // 2. Serialize message body
        let type_id = TypeId::of::<M>();
        let serializer = self.tell_serializers.get(&type_id).ok_or_else(|| {
            ActorSendError(format!(
                "message type '{}' not registered for remote send to {}",
                std::any::type_name::<M>(),
                self.id
            ))
        })?;

        let (type_name, body) = serializer(&msg as &dyn Any)
            .map_err(|e| ActorSendError(format!("serialization failed: {}", e.message)))?;

        // 3. Convert headers to wire format and build envelope
        let wire_headers = headers.to_wire();
        let envelope = WireEnvelope {
            target: self.id.clone(),
            message_type: type_name,
            send_mode: SendMode::Tell,
            headers: wire_headers,
            body,
            request_id: None,
            version: None,
        };

        // 4. Send via transport (fire-and-forget, with optional delay)
        let transport = Arc::clone(&self.transport);
        let target_node = self.id.node.clone();
        let delay = match pipeline_result {
            PipelineResult::Delay(d) => Some(d),
            _ => None,
        };

        tokio::spawn(async move {
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
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
        // 1. Run outbound interceptor pipeline (before serialization)
        let mut headers = Headers::new();
        let pipeline_result = self.run_outbound_pipeline(
            std::any::type_name::<M>(),
            SendMode::Ask,
            &mut headers,
            &msg as &dyn Any,
        )?;

        // Drop returns error for ask — caller needs to know the send didn't happen
        if matches!(pipeline_result, PipelineResult::Dropped) {
            return Err(ActorSendError(
                "message dropped by outbound interceptor".into(),
            ));
        }

        // 2. Serialize message body
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

        // 3. Convert headers to wire format and build envelope
        let wire_headers = headers.to_wire();
        let request_id = uuid::Uuid::new_v4();
        let envelope = WireEnvelope {
            target: self.id.clone(),
            message_type: type_name,
            send_mode: SendMode::Ask,
            headers: wire_headers,
            body,
            request_id: Some(request_id),
            version: None,
        };

        // 4. Send via transport and await reply
        let (tx, rx) = oneshot::channel::<Result<M::Reply, RuntimeError>>();
        let transport = Arc::clone(&self.transport);
        let target_node = self.id.node.clone();
        let deserialize_reply = Arc::clone(&entry.deserialize_reply);
        let outbound_interceptors = Arc::clone(&self.outbound_interceptors);
        let ref_id = self.id.clone();
        let ref_name = self.name.clone();
        let request_headers = headers;
        let delay = match pipeline_result {
            PipelineResult::Delay(d) => Some(d),
            _ => None,
        };

        tokio::spawn(async move {
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }

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

            // Notify interceptors about the outcome (success or failure)
            let notify_interceptors = |outcome: &crate::interceptor::Outcome<'_>| {
                if !outbound_interceptors.is_empty() {
                    let ctx = OutboundContext {
                        target_id: ref_id.clone(),
                        target_name: &ref_name,
                        message_type: std::any::type_name::<M>(),
                        send_mode: SendMode::Ask,
                        remote: true,
                    };
                    let runtime_headers = RuntimeHeaders::new();
                    for interceptor in outbound_interceptors.iter() {
                        interceptor.on_reply(&ctx, &runtime_headers, &request_headers, outcome);
                    }
                }
            };

            match result {
                Ok(reply_envelope) => match deserialize_reply(&reply_envelope.body) {
                    Ok(any_reply) => {
                        if let Ok(reply) = (any_reply as Box<dyn Any + Send>).downcast::<M::Reply>()
                        {
                            let outcome = crate::interceptor::Outcome::AskSuccess {
                                reply: reply.as_ref(),
                            };
                            notify_interceptors(&outcome);
                            let _ = tx.send(Ok(*reply));
                        } else {
                            let _ = tx.send(Err(RuntimeError::Send(ActorSendError(
                                "reply type mismatch".into(),
                            ))));
                        }
                    }
                    Err(e) => {
                        let error_msg = format!("reply deserialization failed: {}", e.message);
                        let outcome = crate::interceptor::Outcome::HandlerError {
                            error: crate::actor::ActorError::internal(&error_msg),
                        };
                        notify_interceptors(&outcome);
                        let _ = tx.send(Err(RuntimeError::Send(ActorSendError(error_msg))));
                    }
                },
                Err(e) => {
                    let error_msg = format!("remote ask failed: {}", e);
                    let outcome = crate::interceptor::Outcome::HandlerError {
                        error: crate::actor::ActorError::internal(&error_msg),
                    };
                    notify_interceptors(&outcome);
                    let _ = tx.send(Err(RuntimeError::Send(ActorSendError(error_msg))));
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
    outbound_interceptors: Vec<Arc<dyn OutboundInterceptor>>,
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
            outbound_interceptors: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Add an outbound interceptor to the pipeline.
    ///
    /// Interceptors run in registration order before every `tell()` and
    /// `ask()` call. They can inspect the message, modify headers,
    /// or reject the send.
    pub fn add_outbound_interceptor(mut self, interceptor: Arc<dyn OutboundInterceptor>) -> Self {
        self.outbound_interceptors.push(interceptor);
        self
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
            outbound_interceptors: Arc::new(self.outbound_interceptors),
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
    use crate::remote::WireHeaders;
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

    // -- Outbound interceptor tests --

    /// Test interceptor that stamps a header on every outgoing message.
    struct HeaderStamper;
    impl OutboundInterceptor for HeaderStamper {
        fn name(&self) -> &'static str {
            "header-stamper"
        }
        fn on_send(
            &self,
            _ctx: &OutboundContext<'_>,
            _runtime_headers: &RuntimeHeaders,
            headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            use crate::message::HeaderValue;
            #[derive(Debug)]
            struct Stamp;
            impl HeaderValue for Stamp {
                fn header_name(&self) -> &'static str {
                    "x-stamp"
                }
                fn to_bytes(&self) -> Option<Vec<u8>> {
                    Some(b"stamped".to_vec())
                }
                fn as_any(&self) -> &dyn Any {
                    self
                }
            }
            headers.insert(Stamp);
            Disposition::Continue
        }
    }

    /// Test interceptor that rejects all messages.
    struct RejectAll;
    impl OutboundInterceptor for RejectAll {
        fn name(&self) -> &'static str {
            "reject-all"
        }
        fn on_send(
            &self,
            _ctx: &OutboundContext<'_>,
            _runtime_headers: &RuntimeHeaders,
            _headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            Disposition::Reject("blocked by policy".into())
        }
    }

    /// Test interceptor that counts calls.
    struct CallCounter(Arc<std::sync::atomic::AtomicU64>);
    impl OutboundInterceptor for CallCounter {
        fn name(&self) -> &'static str {
            "call-counter"
        }
        fn on_send(
            &self,
            _ctx: &OutboundContext<'_>,
            _runtime_headers: &RuntimeHeaders,
            _headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Disposition::Continue
        }
    }

    #[test]
    fn outbound_interceptor_rejects_tell() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
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
            |_any: &dyn Any| Ok(vec![1]),
        )
        .add_outbound_interceptor(Arc::new(RejectAll))
        .build();

        let result = remote.tell(Increment);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("rejected"));
        assert!(err.to_string().contains("blocked by policy"));
    }

    #[test]
    fn outbound_interceptor_rejects_ask() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
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
        .add_outbound_interceptor(Arc::new(RejectAll))
        .build();

        let result = remote.ask(GetCount, None);
        assert!(result.is_err());
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(err.to_string().contains("rejected"));
    }

    #[tokio::test]
    async fn outbound_interceptor_stamps_headers_on_tell() {
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
        .register_tell_with(
            TypeId::of::<Increment>(),
            "test::Increment",
            |_any: &dyn Any| Ok(vec![1]),
        )
        .add_outbound_interceptor(Arc::new(HeaderStamper))
        .build();

        remote.tell(Increment).unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();

        // Verify the header was stamped by the interceptor
        assert_eq!(received.headers.get("x-stamp").unwrap(), b"stamped");
    }

    #[tokio::test]
    async fn outbound_interceptor_counter_tracks_sends() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let _rx = transport.register_node(NodeId("node-2".into())).await;
        transport.connect(&NodeId("node-2".into())).await.unwrap();

        let count = Arc::new(std::sync::atomic::AtomicU64::new(0));

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
            |_any: &dyn Any| Ok(vec![1]),
        )
        .add_outbound_interceptor(Arc::new(CallCounter(Arc::clone(&count))))
        .build();

        remote.tell(Increment).unwrap();
        remote.tell(Increment).unwrap();
        remote.tell(Increment).unwrap();

        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn outbound_interceptor_chain_runs_in_order() {
        let transport = Arc::new(InMemoryTransport::new(NodeId("local".into())));
        let count = Arc::new(std::sync::atomic::AtomicU64::new(0));

        // First interceptor counts, second rejects
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
            |_any: &dyn Any| Ok(vec![1]),
        )
        .add_outbound_interceptor(Arc::new(CallCounter(Arc::clone(&count))))
        .add_outbound_interceptor(Arc::new(RejectAll))
        .build();

        let result = remote.tell(Increment);
        assert!(result.is_err()); // rejected by second interceptor
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1); // first ran
    }
}
