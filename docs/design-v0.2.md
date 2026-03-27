# dactor v0.2 — Design Document

> **Goal:** Refactor dactor from a minimal trait extraction into a professional,
> production-grade abstract actor framework, informed by Erlang/OTP, Akka,
> ractor, kameo, Actix, and Coerce.

---

## 0. Design Principle: Superset with Graceful Degradation

### Inclusion Rule

**dactor abstracts the superset of capabilities supported by 2 or more actor
frameworks.** If a behavior is common to at least two of the surveyed
frameworks, dactor models it as a first-class trait or type. Individual adapters
that don't natively support a capability have two options:

1. **Adapter-layer implementation** — the adapter implements the capability
   using custom logic (e.g., ractor doesn't have bounded mailboxes, but the
   adapter can wrap a bounded channel).
2. **`NotSupported` error** — the adapter returns `Err(NotSupported)` at
   runtime, signaling the caller that this capability is unavailable with the
   chosen backend.

This ensures the **core API is rich and forward-looking** while each adapter
remains honest about what it can deliver.

### Capability Inclusion Matrix

The table below counts how many of the 6 surveyed frameworks support each
capability. **≥ 2** means it qualifies for inclusion in dactor.

| Capability | Erlang | Akka | Ractor | Kameo | Actix | Coerce | Count | Include? |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Tell (fire-and-forget) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | **6** | ✅ |
| Ask (request-reply) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | **6** | ✅ |
| Typed messages | — | ✓ | ✓ | ✓ | ✓ | ✓ | **5** | ✅ |
| Actor identity (ID) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | **6** | ✅ |
| Lifecycle hooks | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | **6** | ✅ |
| Supervision | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | **6** | ✅ |
| DeathWatch / monitoring | ✓ | ✓ | ✓ | ✓ | — | — | **4** | ✅ |
| Timers (send_after/interval) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | **6** | ✅ |
| Processing groups | ✓ | ✓ | ✓ | — | — | ✓ | **4** | ✅ |
| Actor registry (named lookup) | ✓ | ✓ | ✓ | — | ✓ | ✓ | **5** | ✅ (v0.4) |
| Mailbox configuration | — | ✓ | — | ✓ | ✓ | — | **3** | ✅ |
| Interceptors / middleware | — | ✓ | — | — | ✓ | ✓ | **3** | ✅ |
| Message envelope / metadata | ✓ | ✓ | — | — | — | — | **2** | ✅ |
| Cluster events | ✓ | ✓ | ✓ | ✓ | — | ✓ | **5** | ✅ |
| Distribution (remote actors) | ✓ | ✓ | ✓ | ✓ | — | ✓ | **5** | ✅ (future) |
| Clock abstraction | ✓ | ✓ | — | — | — | — | **2** | ✅ |
| Streaming responses | ✓ | ✓ | — | — | — | — | **2** | ✅ |
| Hot code upgrade | ✓ | — | — | — | — | — | **1** | ❌ |

### `NotSupported` Error

All trait methods that might not be supported by every adapter return a
`Result` type. A new error variant is introduced:

```rust
/// Error indicating that the adapter does not support this operation.
#[derive(Debug, Clone)]
pub struct NotSupportedError {
    /// Name of the operation that is not supported.
    pub operation: &'static str,
    /// Name of the adapter/runtime that doesn't support it.
    pub adapter: &'static str,
    /// Optional detail message.
    pub detail: Option<String>,
}

impl fmt::Display for NotSupportedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is not supported by {}", self.operation, self.adapter)?;
        if let Some(ref detail) = self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for NotSupportedError {}
```

A unified error enum encompasses all runtime errors:

```rust
/// Unified error type for all ActorRuntime operations.
#[derive(Debug)]
pub enum RuntimeError {
    /// The actor's mailbox is closed or the send failed.
    Send(ActorSendError),
    /// Processing group operation failed.
    Group(GroupError),
    /// Cluster event operation failed.
    Cluster(ClusterError),
    /// The requested operation is not supported by this adapter.
    NotSupported(NotSupportedError),
}
```

### Adapter Support Matrix (Planned)

For each feature and each adapter, there are exactly three possibilities:

- ✅ **Library Native** — the underlying actor library (ractor / kameo) directly supports this feature; the adapter maps to the library's API
- ⚙️ **Adapter Implemented** — the library does *not* support this feature, but the adapter crate implements it with custom logic
- ❌ **Not Supported** — the feature cannot be provided; returns `RuntimeError::NotSupported` at runtime

| Capability | dactor-ractor | dactor-kameo | Notes |
|---|:---:|:---:|---|
| `tell()` | ✅ Library | ✅ Library | ractor `cast()` / kameo `tell().try_send()` |
| `tell_envelope()` | ⚙️ Adapter | ⚙️ Adapter | Neither library has envelopes; adapter unwraps, runs interceptors, forwards body |
| `ask()` | ✅ Library | ✅ Library | ractor `call()` / kameo `ask()` |
| `stream()` | ⚙️ Adapter | ⚙️ Adapter | Neither library has streaming; adapter creates `mpsc` channel, passes `StreamSender` to actor |
| `ActorRef::id()` | ✅ Library | ✅ Library | ractor `get_id()` / kameo `id()` |
| `ActorRef::is_alive()` | ✅ Library | ✅ Library | Check actor cell / ref validity |
| Lifecycle hooks | ✅ Library | ✅ Library | ractor `pre_start`/`post_stop` / kameo `on_start`/`on_stop` |
| Supervision | ✅ Library | ✅ Library | ractor parent-child / kameo `on_link_died` |
| `watch()` / `unwatch()` | ✅ Library | ✅ Library | ractor supervisor notifications / kameo actor linking |
| `MailboxConfig::Unbounded` | ✅ Library | ✅ Library | Default for both |
| `MailboxConfig::Bounded` | ⚙️ Adapter | ✅ Library | ractor: adapter wraps with bounded `tokio::sync::mpsc`; kameo: `spawn_bounded()` |
| `OverflowStrategy::Block` | ⚙️ Adapter | ✅ Library | ractor: bounded channel blocks; kameo: native bounded behavior |
| `OverflowStrategy::RejectWithError` | ⚙️ Adapter | ✅ Library | ractor: `try_send()` on adapter channel; kameo: `try_send()` native |
| `OverflowStrategy::DropNewest` | ⚙️ Adapter | ⚙️ Adapter | Both: `try_send()`, silently discard on full |
| `OverflowStrategy::DropOldest` | ❌ Not Supported | ❌ Not Supported | Neither library exposes queue eviction |
| Interceptors (global) | ⚙️ Adapter | ⚙️ Adapter | Neither library has interceptors; adapter runs chain before dispatch |
| Interceptors (per-actor) | ⚙️ Adapter | ⚙️ Adapter | Stored in actor wrapper by adapter, run per message |
| Processing groups | ⚙️ Adapter | ⚙️ Adapter | Neither library has groups; adapter maintains type-erased registry |
| Cluster events | ⚙️ Adapter | ⚙️ Adapter | Neither library has unified cluster events; adapter provides callback system |

---

## 1. Research Summary: Common Behaviors Across Actor Frameworks

| Concept | Erlang/OTP | Akka (JVM) | Ractor (Rust) | Kameo (Rust) | Actix (Rust) | Coerce (Rust) |
|---|---|---|---|---|---|---|
| **Message passing** | `Pid ! Msg` (async) | tell `!` / ask `?` | `cast()` / `call()` | `tell()` / `ask()` | `do_send()` / `send()` | `notify()` / `send()` |
| **Typed messages** | Dynamic (any term) | Typed behaviors | `ActorRef<M>` | `Message<M>` trait | `Handler<M>` trait | `Handler<M>` trait |
| **Lifecycle hooks** | `init`, `terminate`, `handle_info` | `preStart`, `postStop`, `preRestart` | `pre_start`, `post_start`, `post_stop` | `on_start`, `on_stop`, `on_panic` | `started()`, `stopped()` | Lifecycle events |
| **Supervision** | Supervisor trees with strategies | Resume/Restart/Stop/Escalate | Parent-child supervision | `on_link_died` linking | Built-in supervision | Supervision + clustering |
| **DeathWatch** | `monitor/2` | `context.watch()` → `Terminated` | Supervisor notifications | Actor linking | — | — |
| **Interceptors** | — | `Behaviors.intercept` | — | — | Middleware (web) | Metrics/tracing |
| **Message envelope** | Built-in (pid, ref, msg) | Envelope with metadata | Plain typed msg | Plain typed msg | Plain typed msg | Plain typed msg |
| **Timers** | `send_after`, `send_interval` | Scheduler | tokio tasks | tokio tasks | `run_interval` | tokio tasks |
| **Processing groups** | `pg` module | Cluster-aware routing | Named groups | — | — | Pub/sub, sharding |
| **Actor registry** | `register/2` (named) | Receptionist | Named registry | — | Registry | Actor system |
| **Mailbox config** | Per-process (unbounded) | Bounded/custom | Unbounded | Bounded (default) | Bounded | Unbounded |
| **Distribution** | Native (Erlang nodes) | Akka Cluster/Remoting | `ractor_cluster` | libp2p / Kademlia | — | Cluster, remote actors |
| **Clock/time** | `erlang:monotonic_time` | Scheduler | — | — | — | — |
| **Streaming responses** | Multi-part `gen_server` reply | Akka Streams `Source` | — | — | — | — |

### Key Takeaways

1. **Every framework** has tell (fire-and-forget) — this is the fundamental operation.
2. **Most frameworks** also support ask (request-reply) — we should abstract it.
3. **All production frameworks** have lifecycle hooks — we need `on_start`/`on_stop`.
4. **Supervision** is universal in Erlang, Akka, ractor, kameo — we should model it.
5. **Message envelopes** with headers exist in Erlang and Akka (2 frameworks) — qualifies for inclusion under the superset rule.
6. **Interceptors/middleware** exist in Akka, Actix, and Coerce (3 frameworks) — qualifies for inclusion.
7. **Test support behind feature flags** is standard practice in Rust crates.
8. **Superset rule applied:** every capability above is supported by ≥ 2 frameworks (see §0). Adapters return `NotSupported` for features they can't provide.
9. **Streaming responses** exist in Erlang (multi-part `gen_server` replies) and Akka (Akka Streams `Source`) — qualifies under the superset rule. Combined with the ubiquity of gRPC server-streaming and Rust's async `Stream` trait, this is a high-value addition.

---

## 2. Current dactor Architecture (v0.1)

```
dactor/
├── traits/
│   ├── runtime.rs    → ActorRuntime, ActorRef<M>, ClusterEvents, TimerHandle
│   └── clock.rs      → Clock, SystemClock, TestClock
├── types/
│   └── node.rs       → NodeId
└── test_support/
    ├── test_runtime.rs → TestRuntime, TestActorRef, TestClusterEvents
    └── test_clock.rs   → re-export of TestClock
```

### Problems

1. **`TestClock` lives in `traits/clock.rs`** — test utilities mixed with production code
2. **No feature gate** on test_support — always compiled
3. **Messages are plain `M`** — no metadata, no tracing context, no correlation
4. **No interceptor pipeline** — can't inject logging, metrics, or context propagation
5. **Only fire-and-forget** — no ask/reply pattern
6. **No lifecycle hooks** — actors are just closures with no start/stop semantics
7. **No supervision** — no way to monitor or restart actors
8. **No actor identity** — actors have no ID, can't be compared or addressed by name
9. **No mailbox configuration** — adapter decides (ractor=unbounded, kameo=bounded)
10. **`NodeId` uses `serde`** — unnecessary dependency for the core crate unless needed

---

## 3. Proposed Architecture (v0.2)

```
dactor/
├── src/
│   ├── lib.rs
│   ├── actor.rs           → ActorRef, ActorId, ActorRuntime trait
│   ├── message.rs         → Envelope<M>, MessageHeaders, Header trait
│   ├── interceptor.rs     → Interceptor trait, InterceptorChain
│   ├── lifecycle.rs       → ActorLifecycle hooks
│   ├── supervision.rs     → Supervisor trait, SupervisionStrategy
│   ├── clock.rs           → Clock, SystemClock (production only)
│   ├── cluster.rs         → ClusterEvents, ClusterEvent, NodeId
│   ├── timer.rs           → TimerHandle
│   ├── mailbox.rs         → MailboxConfig
│   └── errors.rs          → All error types
├── src/test_support/      → behind #[cfg(feature = "test-support")]
│   ├── mod.rs
│   ├── test_runtime.rs
│   └── test_clock.rs
```

### 3.1 Message Envelope

**Rationale:** Every distributed system eventually needs message metadata — trace
IDs, correlation IDs, deadlines, security context. Baking this into the
framework from day one avoids a breaking change later.

```rust
/// Type-erased header value. Adapters can store trace context,
/// correlation IDs, deadlines, auth tokens, etc.
pub trait HeaderValue: Send + Sync + 'static {
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A collection of typed headers attached to a message.
///
/// Headers use `TypeId`-keyed storage so that each header type can be
/// inserted and retrieved without string lookups or downcasting guesswork.
#[derive(Default)]
pub struct Headers { /* TypeMap internally */ }

impl Headers {
    pub fn insert<H: HeaderValue>(&mut self, value: H);
    pub fn get<H: HeaderValue>(&self) -> Option<&H>;
    pub fn remove<H: HeaderValue>(&mut self) -> Option<H>;
    pub fn is_empty(&self) -> bool;
}

/// An envelope wrapping a message body with typed headers.
pub struct Envelope<M> {
    pub headers: Headers,
    pub body: M,
}

impl<M> From<M> for Envelope<M> {
    fn from(body: M) -> Self {
        Envelope { headers: Headers::default(), body }
    }
}
```

**Built-in header types** (provided by dactor, opt-in):

```rust
/// W3C-compatible trace context for distributed tracing.
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub trace_flags: u8,
}

/// Correlation ID for request tracking across actors.
pub struct CorrelationId(pub String);

/// Deadline after which the message should be discarded.
pub struct Deadline(pub std::time::Instant);
```

### 3.2 Interceptor Pipeline

**Rationale:** Akka has `Behaviors.intercept`, HTTP frameworks have middleware.
An interceptor pipeline lets users add cross-cutting concerns (logging,
metrics, tracing, auth) without modifying actor code.

```rust
/// Outcome of an interceptor's processing.
pub enum Disposition {
    /// Continue to the next interceptor / deliver the message.
    Continue,
    /// Drop the message silently (e.g., rate-limiting, auth failure).
    Drop,
}

/// An interceptor that can observe or modify messages in transit.
///
/// Interceptors form an ordered pipeline. Each interceptor sees the
/// envelope before the actor's handler and can modify headers, log,
/// record metrics, or drop the message entirely.
pub trait Interceptor: Send + Sync + 'static {
    /// Called before the message is delivered to the actor.
    fn on_receive(&self, headers: &mut Headers) -> Disposition {
        Disposition::Continue
    }

    /// Called after the actor's handler returns (for post-processing).
    fn on_complete(&self, headers: &Headers) {}
}
```

**Example: Logging interceptor**

```rust
struct LoggingInterceptor;

impl Interceptor for LoggingInterceptor {
    fn on_receive(&self, headers: &mut Headers) -> Disposition {
        if let Some(cid) = headers.get::<CorrelationId>() {
            tracing::info!(correlation_id = %cid.0, "message received");
        }
        Disposition::Continue
    }
}
```

**Registration:**

```rust
// On the runtime:
runtime.add_interceptor(LoggingInterceptor);

// Or per-actor at spawn time:
runtime.spawn_with_config("my-actor", config, handler);
```

### 3.3 ActorRef & ActorId

**Rationale:** Every framework gives actors identity. Without an ID, you can't
implement supervision, death watch, logging, or debugging.

```rust
/// Unique identifier for an actor within a runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorId(pub u64);

impl fmt::Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Actor({})", self.0)
    }
}

pub trait ActorRef<M: Send + 'static>: Clone + Send + Sync + 'static {
    /// The actor's unique identity.
    fn id(&self) -> ActorId;

    /// Fire-and-forget: deliver a raw message.
    fn tell(&self, msg: M) -> Result<(), ActorSendError>;

    /// Fire-and-forget with an envelope (headers + body).
    /// Adapters that don't support envelopes may ignore headers and
    /// forward only the body, or return `NotSupported` if the envelope
    /// cannot be delivered at all.
    fn tell_envelope(&self, envelope: Envelope<M>) -> Result<(), RuntimeError>;

    /// Check if the actor is still alive.
    /// Returns `Err(NotSupported)` if the adapter cannot determine liveness.
    fn is_alive(&self) -> Result<bool, NotSupportedError>;
}
```

> **Note:** `send()` is renamed to `tell()` to align with Erlang/Akka/kameo
> terminology. A deprecated `send()` alias can ease migration.

### 3.4 Ask Pattern (Request-Reply)

**Rationale:** ractor, kameo, Akka, and Actix all support ask. Not having it
forces users to implement reply channels manually.

The ask pattern is modeled as a **separate trait** so adapters that don't
support it can omit the implementation:

```rust
/// Extension trait for request-reply messaging.
///
/// Adapters that support ask natively (ractor `call`, kameo `ask`)
/// implement this trait. Adapters that don't support it should provide
/// a blanket implementation returning `NotSupported`.
pub trait AskRef<M, R>: ActorRef<M>
where
    M: Send + 'static,
    R: Send + 'static,
{
    /// Send a message and await a reply.
    /// Returns `Err(RuntimeError::NotSupported)` if the adapter doesn't
    /// support request-reply messaging.
    fn ask(&self, msg: M) -> Result<tokio::sync::oneshot::Receiver<R>, RuntimeError>;
}
```

### 3.5 Streaming (Request-Stream)

**Rationale:** Erlang supports multi-part `gen_server` replies where a server
sends chunked results back to the caller over time. Akka has first-class
support via Akka Streams `Source`, tightly integrated with actors. gRPC server
streaming is the dominant RPC pattern for streaming data. In Rust, the async
`Stream` trait (`futures_core::Stream`) is the standard abstraction, and
`tokio::sync::mpsc` channels convert naturally into streams via
`tokio_stream::wrappers::ReceiverStream`.

dactor should provide a `stream()` method on actor references that sends a
request to an actor and returns an async stream of response items. This enables
use cases like:

- Paginated data retrieval
- Real-time event feeds / subscriptions
- Long-running computation with progressive results
- Fan-out aggregation with incremental delivery

**Core types:**

```rust
use std::pin::Pin;
use futures_core::Stream;

/// A pinned, boxed, Send-safe async stream of items.
/// This is the return type from `StreamRef::stream()` — the caller
/// consumes it with `while let Some(item) = stream.next().await`.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// A sender handle given to the actor's stream handler.
/// The actor pushes items into this sender; the caller receives them
/// as an async stream on the other end.
///
/// Backed by a bounded `mpsc` channel for backpressure.
pub struct StreamSender<T: Send + 'static> {
    inner: tokio::sync::mpsc::Sender<T>,
}

impl<T: Send + 'static> StreamSender<T> {
    /// Send an item to the stream consumer.
    /// Returns `Err` if the consumer has dropped the stream.
    pub async fn send(&self, item: T) -> Result<(), StreamSendError> {
        self.inner.send(item).await
            .map_err(|_| StreamSendError::ConsumerDropped)
    }

    /// Try to send without blocking. Returns `Err` if the channel is
    /// full or the consumer has dropped.
    pub fn try_send(&self, item: T) -> Result<(), StreamSendError> {
        self.inner.try_send(item)
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) =>
                    StreamSendError::Full,
                tokio::sync::mpsc::error::TrySendError::Closed(_) =>
                    StreamSendError::ConsumerDropped,
            })
    }

    /// Check if the consumer is still listening.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

#[derive(Debug)]
pub enum StreamSendError {
    /// The consumer dropped the stream (no longer reading).
    ConsumerDropped,
    /// The channel buffer is full (backpressure).
    Full,
}
```

**Extension trait on `ActorRef`:**

```rust
/// Extension trait for request-stream messaging.
///
/// The caller sends a request message and receives a stream of response
/// items. The actor's handler receives the message together with a
/// `StreamSender<R>` and pushes items into it.
///
/// Adapters implement this using a bounded `mpsc` channel: the adapter
/// creates the channel, wraps the `Receiver` into a `BoxStream`, and
/// passes the `Sender` (as `StreamSender<R>`) to the actor alongside
/// the request message.
pub trait StreamRef<M, R>: ActorRef<M>
where
    M: Send + 'static,
    R: Send + 'static,
{
    /// Send a request and receive a stream of responses.
    ///
    /// `buffer` controls the channel capacity (backpressure). A typical
    /// default is 16 or 32.
    ///
    /// Returns `Err(RuntimeError::NotSupported)` if the adapter doesn't
    /// support streaming.
    fn stream(
        &self,
        msg: M,
        buffer: usize,
    ) -> Result<BoxStream<R>, RuntimeError>;
}
```

**How it works (adapter implementation pattern):**

```
Caller                       Adapter Layer                     Actor
  │                               │                              │
  │  stream(request, buf=16)      │                              │
  │──────────────────────────────►│                              │
  │                               │  create mpsc(16)             │
  │                               │  tx = StreamSender(sender)   │
  │                               │  rx = ReceiverStream(recv)   │
  │                               │                              │
  │                               │  deliver (request, tx)       │
  │                               │─────────────────────────────►│
  │◄─ return BoxStream(rx)        │                              │
  │                               │                              │
  │  .next().await ◄──────────────│◄── tx.send(item_1) ─────────│
  │  .next().await ◄──────────────│◄── tx.send(item_2) ─────────│
  │  .next().await ◄──────────────│◄── tx.send(item_3) ─────────│
  │  None (stream ends) ◄────────│◄── drop(tx) ─────────────────│
```

**Backpressure:** The bounded channel naturally provides backpressure. If the
caller is slow to consume, the actor's `tx.send().await` will suspend until
the caller reads an item, preventing unbounded memory growth.

**Cancellation:** When the caller drops the `BoxStream`, the `Receiver` is
dropped, closing the channel. The actor's next `tx.send()` returns
`StreamSendError::ConsumerDropped`, signaling it to stop producing.

**Example usage (caller side):**

```rust
use dactor::{ActorRuntime, StreamRef};
use tokio_stream::StreamExt;

async fn get_logs(runtime: &impl ActorRuntime, actor: &impl StreamRef<GetLogs, LogEntry>) {
    let mut stream = actor.stream(GetLogs { since: yesterday() }, 32).unwrap();

    while let Some(entry) = stream.next().await {
        println!("{}: {}", entry.timestamp, entry.message);
    }
}
```

**Example usage (actor handler side):**

```rust
// The actor receives a tuple of (request, StreamSender)
// when dispatched via stream(). The adapter wraps the handler.
async fn handle_get_logs(request: GetLogs, tx: StreamSender<LogEntry>) {
    for entry in database.query_logs(request.since).await {
        if tx.send(entry).await.is_err() {
            break; // consumer dropped the stream
        }
    }
    // dropping tx closes the stream on the caller side
}
```

**Relationship to Ask:** `ask()` is request → single reply. `stream()` is
request → multiple replies. Both are modeled as separate extension traits
(`AskRef` and `StreamRef`) so that adapters can implement either, both, or
neither independently.

**Dependencies:** The core crate adds `futures-core` (for the `Stream` trait)
and `tokio-stream` (for `ReceiverStream`) as dependencies, both lightweight
and standard in the async Rust ecosystem.

### 3.6 Actor Lifecycle

**Rationale:** Erlang has `init/terminate`, Akka has `preStart/postStop`,
ractor has `pre_start/post_stop`, kameo has `on_start/on_stop`.

```rust
/// Lifecycle hooks for an actor. All methods have default no-op
/// implementations so that simple actors can ignore them.
pub trait ActorLifecycle: Send + 'static {
    /// Called after the actor is spawned, before it processes any messages.
    fn on_start(&mut self) {}

    /// Called when the actor is stopping (graceful shutdown).
    fn on_stop(&mut self) {}

    /// Called when the actor's handler panics or returns an error.
    /// Return an `ErrorAction` to control what happens next.
    fn on_error(&mut self, _error: Box<dyn std::error::Error + Send>) -> ErrorAction {
        ErrorAction::Stop
    }
}

pub enum ErrorAction {
    /// Resume processing the next message (Erlang: continue).
    Resume,
    /// Restart the actor (Erlang: restart, Akka: Restart).
    Restart,
    /// Stop the actor (Erlang: shutdown).
    Stop,
    /// Escalate to the supervisor (Akka: Escalate).
    Escalate,
}
```

### 3.7 Supervision

**Rationale:** Erlang supervisors, Akka supervision strategies, ractor
parent-child supervision, kameo `on_link_died`.

```rust
/// Notification sent to a supervisor when a child actor terminates.
pub struct ChildTerminated {
    pub child_id: ActorId,
    pub child_name: String,
    /// `None` for graceful shutdown, `Some(reason)` for failure.
    pub reason: Option<String>,
}

/// Strategy applied by a supervisor when a child fails.
pub trait SupervisionStrategy: Send + Sync + 'static {
    fn on_child_failed(&self, event: &ChildTerminated) -> SupervisionAction;
}

pub enum SupervisionAction {
    /// Restart the failed child actor.
    Restart,
    /// Stop the failed child and don't restart.
    Stop,
    /// Escalate the failure to the parent supervisor.
    Escalate,
}

/// Built-in strategies (matching Erlang/Akka conventions):
pub struct OneForOne;       // restart only the failed child
pub struct OneForAll;       // restart all children when one fails
pub struct RestForOne;      // restart the failed child and all after it
```

**DeathWatch** — any actor can watch another:

```rust
pub trait ActorRuntime: Send + Sync + 'static {
    // ... existing methods ...

    /// Watch an actor. When it terminates, the watcher receives a
    /// `ChildTerminated` notification via its message handler.
    /// Returns `Err(NotSupported)` if the adapter doesn't support death watch.
    fn watch<M: Send + 'static>(
        &self,
        watcher: &Self::Ref<M>,
        target: ActorId,
    ) -> Result<(), RuntimeError>;

    /// Stop watching an actor.
    /// Returns `Err(NotSupported)` if the adapter doesn't support death watch.
    fn unwatch<M: Send + 'static>(
        &self,
        watcher: &Self::Ref<M>,
        target: ActorId,
    ) -> Result<(), RuntimeError>;
}
```

### 3.8 Mailbox Configuration

**Rationale:** kameo defaults to bounded, ractor to unbounded. The abstraction
should let users choose.

```rust
/// Mailbox sizing strategy for an actor.
#[derive(Debug, Clone)]
pub enum MailboxConfig {
    /// Unbounded mailbox — never blocks senders. Risk of memory exhaustion.
    Unbounded,
    /// Bounded mailbox with backpressure.
    Bounded {
        capacity: usize,
        /// What to do when the mailbox is full.
        overflow: OverflowStrategy,
    },
}

pub enum OverflowStrategy {
    /// Block the sender until space is available.
    Block,
    /// Drop the newest message (the one being sent).
    DropNewest,
    /// Drop the oldest message in the mailbox.
    DropOldest,
    /// Return an error to the sender.
    RejectWithError,
}

impl Default for MailboxConfig {
    fn default() -> Self {
        MailboxConfig::Unbounded
    }
}
```

### 3.9 Spawn Configuration

Collect all per-actor settings into a config struct:

```rust
pub struct SpawnConfig {
    pub mailbox: MailboxConfig,
    pub interceptors: Vec<Box<dyn Interceptor>>,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            mailbox: MailboxConfig::default(),
            interceptors: Vec::new(),
        }
    }
}
```

Updated `ActorRuntime::spawn`:

```rust
pub trait ActorRuntime: Send + Sync + 'static {
    // Simple spawn (backward-compatible)
    fn spawn<M, H>(&self, name: &str, handler: H) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static;

    // Spawn with configuration
    fn spawn_with_config<M, H>(
        &self,
        name: &str,
        config: SpawnConfig,
        handler: H,
    ) -> Self::Ref<M>
    where
        M: Send + 'static,
        H: FnMut(M) + Send + 'static;

    // ... timers, groups, cluster events ...
}
```

### 3.10 Feature-Gated Test Support

**Rationale:** `TestClock`, `TestRuntime`, `TestClusterEvents` are test
utilities. They should not be compiled into production binaries.

```toml
# dactor/Cargo.toml
[features]
default = []
test-support = ["tokio/test-util"]
```

```rust
// dactor/src/lib.rs
#[cfg(feature = "test-support")]
pub mod test_support;
```

Downstream crates use:

```toml
[dev-dependencies]
dactor = { version = "0.2", features = ["test-support"] }
```

### 3.11 Revised `ActorRuntime` Trait (Complete)

```rust
pub trait ActorRuntime: Send + Sync + 'static {
    type Ref<M: Send + 'static>: ActorRef<M>;
    type Events: ClusterEvents;
    type Timer: TimerHandle;

    // ── Spawning ────────────────────────────────────────
    fn spawn<M, H>(&self, name: &str, handler: H) -> Self::Ref<M>
    where M: Send + 'static, H: FnMut(M) + Send + 'static;

    /// Spawn with per-actor configuration (mailbox, interceptors).
    /// Returns `Err(NotSupported)` for config options the adapter can't honor.
    fn spawn_with_config<M, H>(
        &self, name: &str, config: SpawnConfig, handler: H,
    ) -> Result<Self::Ref<M>, RuntimeError>
    where M: Send + 'static, H: FnMut(M) + Send + 'static;

    // ── Timers ──────────────────────────────────────────
    fn send_interval<M: Clone + Send + 'static>(
        &self, target: &Self::Ref<M>, interval: Duration, msg: M,
    ) -> Self::Timer;

    fn send_after<M: Send + 'static>(
        &self, target: &Self::Ref<M>, delay: Duration, msg: M,
    ) -> Self::Timer;

    // ── Processing Groups ───────────────────────────────
    fn join_group<M: Send + 'static>(
        &self, group: &str, actor: &Self::Ref<M>,
    ) -> Result<(), RuntimeError>;

    fn leave_group<M: Send + 'static>(
        &self, group: &str, actor: &Self::Ref<M>,
    ) -> Result<(), RuntimeError>;

    fn broadcast_group<M: Clone + Send + 'static>(
        &self, group: &str, msg: M,
    ) -> Result<(), RuntimeError>;

    fn get_group_members<M: Send + 'static>(
        &self, group: &str,
    ) -> Result<Vec<Self::Ref<M>>, RuntimeError>;

    // ── Supervision / DeathWatch ────────────────────────
    /// Watch an actor for termination.
    /// Returns `Err(NotSupported)` if the adapter doesn't support it.
    fn watch<M: Send + 'static>(
        &self, watcher: &Self::Ref<M>, target: ActorId,
    ) -> Result<(), RuntimeError>;

    fn unwatch<M: Send + 'static>(
        &self, watcher: &Self::Ref<M>, target: ActorId,
    ) -> Result<(), RuntimeError>;

    // ── Cluster ─────────────────────────────────────────
    fn cluster_events(&self) -> &Self::Events;

    // ── Global Interceptors ─────────────────────────────
    /// Register a global interceptor applied to all actors.
    /// Returns `Err(NotSupported)` if the adapter doesn't support interceptors.
    fn add_interceptor(&self, interceptor: Box<dyn Interceptor>) -> Result<(), RuntimeError>;
}
```

### 3.12 Default Implementations via `RuntimeError::NotSupported`

Capabilities that are universally available (tell, spawn, timers) never return
`NotSupported`. Capabilities that may not be available in every adapter
(watch, ask, certain mailbox configs) return `Result<_, RuntimeError>`.

Adapters have three strategies for each method:

| Strategy | When to use | Example |
|---|---|---|
| **Native mapping** | The underlying framework directly supports the operation | ractor `call()` → dactor `ask()` |
| **Adapter-layer shim** | The framework lacks the API but the adapter can emulate it | ractor bounded mailbox via wrapper channel |
| **`NotSupported`** | The feature genuinely can't be provided | `OverflowStrategy::DropOldest` on ractor |

```rust
// Example: adapter that doesn't support watch
fn watch<M: Send + 'static>(
    &self, _watcher: &Self::Ref<M>, _target: ActorId,
) -> Result<(), RuntimeError> {
    Err(RuntimeError::NotSupported(NotSupportedError {
        operation: "watch",
        adapter: "dactor-example",
        detail: Some("underlying framework has no death watch API".into()),
    }))
}
```

### 3.13 Mock Cluster Crate (`dactor-mock`)

**Rationale:** The existing `test_support` module in the `dactor` core crate
provides `TestRuntime` — a single-node, in-memory mock useful for unit-testing
individual actors. However, testing **cluster behavior** (node join/leave,
cross-node messaging, state replication, partition tolerance) requires
simulating multiple nodes within a single process.

Erlang/OTP achieves this with `slave` / `peer` nodes in tests. Akka has
`TestKit` with multi-actor-system setups. No Rust actor framework currently
provides a dedicated multi-node testing crate — this is a differentiating
feature for dactor.

`dactor-mock` is a **standalone workspace crate** (not hidden behind a feature
flag) that provides a fully-functional `ActorRuntime` implementation simulating
a multi-node cluster in a single process. It is the **fourth adapter** in the
workspace, purpose-built for testing.

**Design goals:**

1. **Multi-node in one process** — create N simulated nodes, each with its own
   `ActorRuntime`, connected via in-process channels
2. **Forced serialization** — all cross-node messages are serialized and
   deserialized (using `bincode`, `serde_json`, or a pluggable codec), catching
   serialization bugs that in-memory mocks would miss
3. **Simulated cluster events** — programmatically trigger `NodeJoined` /
   `NodeLeft` events on any node
4. **Network fault injection** — simulate partitions, message drops, latency,
   and reordering between any pair of nodes
5. **Deterministic control** — integrate with `TestClock` for time-controlled
   testing; no real timers unless opted in

**Core types:**

```rust
/// A simulated cluster of N nodes running in the same process.
pub struct MockCluster {
    nodes: Vec<MockNode>,
    network: MockNetwork,
}

/// A single simulated node in the cluster.
/// Implements `dactor::ActorRuntime` so it can be used anywhere
/// a real runtime is expected.
pub struct MockNode {
    node_id: NodeId,
    runtime: MockRuntime,     // ActorRuntime implementation
    cluster_events: MockClusterEvents,
    clock: TestClock,
}

/// The simulated network connecting nodes.
/// Cross-node messages pass through this, which enforces serialization
/// and can inject faults.
pub struct MockNetwork {
    links: HashMap<(NodeId, NodeId), LinkConfig>,
}

/// Configuration for a network link between two nodes.
pub struct LinkConfig {
    /// Whether the link is active (false = simulated partition).
    pub connected: bool,
    /// Simulated one-way latency.
    pub latency: Duration,
    /// Random jitter added to latency (uniform ±jitter).
    pub jitter: Duration,
    /// Probability of dropping a message silently (0.0 = reliable, 1.0 = black hole).
    pub drop_rate: f64,
    /// Probability of duplicating a message (0.0 = no dupes, 1.0 = every msg sent twice).
    pub duplicate_rate: f64,
    /// Probability of corrupting message bytes before decode (0.0 = clean).
    pub corrupt_rate: f64,
    /// Whether to deliver messages out of order (reordering).
    pub reorder: bool,
    /// If set, the link returns an error to the sender instead of silently dropping.
    /// Simulates connection-refused / timeout errors visible to the caller.
    pub error_mode: Option<LinkError>,
    /// The codec used for serialization/deserialization.
    pub codec: Box<dyn MessageCodec>,
}

/// Pre-built link configurations for common test scenarios.
impl LinkConfig {
    /// Perfectly reliable link — no faults, no latency.
    pub fn reliable() -> Self;
    /// Unreliable link with the given drop rate.
    pub fn lossy(drop_rate: f64) -> Self;
    /// Link that simulates a network partition (connected = false).
    pub fn partitioned() -> Self;
    /// Link with simulated WAN-like latency and jitter.
    pub fn slow(latency: Duration, jitter: Duration) -> Self;
}

/// Error mode for a link — what the sender sees when the link is faulty.
#[derive(Debug, Clone)]
pub enum LinkError {
    /// The send returns `Err(ActorSendError)` as if the remote actor is unreachable.
    ConnectionRefused,
    /// The send hangs until a timeout, then returns an error.
    Timeout(Duration),
    /// The send succeeds from the sender's perspective, but the message is silently lost.
    SilentDrop,
}

/// Pluggable serialization for cross-node messages.
pub trait MessageCodec: Send + Sync + 'static {
    fn encode(&self, msg: &[u8]) -> Result<Vec<u8>, CodecError>;
    fn decode(&self, bytes: &[u8]) -> Result<Vec<u8>, CodecError>;
}
```

**How cross-node messaging works:**

```
Node A (sender)                  MockNetwork                   Node B (receiver)
  │                                 │                              │
  │  actor_on_b.tell(msg)           │                              │
  │────────────────────────────────►│                              │
  │                                 │  1. codec.encode(msg)        │
  │                                 │  2. check link config:       │
  │                                 │     - connected?             │
  │                                 │     - error_mode?            │
  │                                 │     - drop_rate roll?        │
  │                                 │     - corrupt_rate roll?     │
  │                                 │     - duplicate_rate roll?   │
  │                                 │  3. if corrupt: flip bits    │
  │                                 │  4. sleep(latency ± jitter)  │
  │                                 │  5. if reorder: enqueue      │
  │                                 │  6. codec.decode(bytes)      │
  │                                 │     → may fail if corrupted  │
  │                                 │  7. deliver to Node B        │
  │                                 │─────────────────────────────►│
  │                                 │                              │  handler(msg)
  │                                 │  8. if duplicate: re-deliver │
  │                                 │─────────────────────────────►│
  │                                 │                              │  handler(msg) again
```

**Key point: forced serialization.** Even though sender and receiver are in the
same process, the message is serialized to bytes and deserialized back. This
catches bugs that only appear over the wire:
- Non-serializable fields (e.g., `Arc`, function pointers)
- Version mismatches in serialization formats
- Incorrect `Serialize` / `Deserialize` implementations

**Intra-node messaging** (actor-to-actor on the same node) uses direct
channel passing (no serialization), matching the behavior of a real runtime.

**Example usage:**

```rust
use dactor_mock::{MockCluster, LinkConfig};
use dactor::{ActorRuntime, ActorRef, ClusterEvents, ClusterEvent, NodeId};

#[tokio::test]
async fn test_cluster_state_sync() {
    // Create a 3-node cluster with reliable links
    let cluster = MockCluster::builder()
        .add_node(NodeId(1))
        .add_node(NodeId(2))
        .add_node(NodeId(3))
        .default_link(LinkConfig::reliable())
        .build();

    let node1 = cluster.node(NodeId(1));
    let node2 = cluster.node(NodeId(2));

    // Spawn actors on different nodes
    let actor_a = node1.runtime().spawn("a", |msg: MyMessage| { /* ... */ });
    let actor_b = node2.runtime().spawn("b", |msg: MyMessage| { /* ... */ });

    // Cross-node send — goes through serialization
    actor_b.tell(MyMessage { data: 42 }).unwrap();

    // Simulate a network partition
    cluster.partition(NodeId(1), NodeId(2));
    // actor_b.tell(...) would now fail or be dropped

    // Heal the partition
    cluster.heal(NodeId(1), NodeId(2));

    // Simulate node failure
    cluster.emit_event(NodeId(2), ClusterEvent::NodeLeft(NodeId(1)));
}

#[tokio::test]
async fn test_serialization_roundtrip() {
    let cluster = MockCluster::builder()
        .add_node(NodeId(1))
        .add_node(NodeId(2))
        .default_link(LinkConfig::reliable())
        .build();

    // This would panic at runtime if MyMessage doesn't correctly
    // implement Serialize/Deserialize — catching it in unit tests
    // rather than in production.
    let actor = cluster.node(NodeId(2)).runtime().spawn("echo", |msg: MyMessage| {
        assert_eq!(msg.data, 42); // deserialized correctly?
    });

    actor.tell(MyMessage { data: 42 }).unwrap();
}
```

**Fault injection API:**

```rust
impl MockCluster {
    // ── Network-level faults ────────────────────────────

    /// Simulate a network partition between two nodes (bidirectional).
    /// Messages in both directions are dropped; senders see `LinkError`
    /// if `error_mode` is set, otherwise silent drop.
    pub fn partition(&self, a: NodeId, b: NodeId);

    /// Heal a partition between two nodes (bidirectional).
    pub fn heal(&self, a: NodeId, b: NodeId);

    /// Set latency on a directional link.
    pub fn set_latency(&self, from: NodeId, to: NodeId, latency: Duration);

    /// Set latency jitter on a directional link.
    pub fn set_jitter(&self, from: NodeId, to: NodeId, jitter: Duration);

    /// Set message drop rate on a directional link (0.0–1.0).
    pub fn set_drop_rate(&self, from: NodeId, to: NodeId, rate: f64);

    /// Set message duplication rate on a directional link (0.0–1.0).
    pub fn set_duplicate_rate(&self, from: NodeId, to: NodeId, rate: f64);

    /// Set message corruption rate on a directional link (0.0–1.0).
    /// Corrupted messages cause deserialization failures on the receiver.
    pub fn set_corrupt_rate(&self, from: NodeId, to: NodeId, rate: f64);

    /// Enable/disable message reordering on a directional link.
    pub fn set_reorder(&self, from: NodeId, to: NodeId, enabled: bool);

    /// Set the error mode on a directional link — controls what the
    /// sender sees when a message can't be delivered.
    pub fn set_error_mode(&self, from: NodeId, to: NodeId, mode: Option<LinkError>);

    /// Replace the entire link config for a directional link.
    pub fn set_link_config(&self, from: NodeId, to: NodeId, config: LinkConfig);

    // ── Node-level faults ───────────────────────────────

    /// Simulate a node crash. All actors on the node are stopped, all
    /// pending messages are lost, and `NodeLeft` is emitted to all
    /// other nodes. The node cannot receive or send messages until
    /// restarted.
    pub fn crash_node(&self, node: NodeId);

    /// Restart a previously crashed node. A fresh `MockRuntime` is
    /// created (all actor state is lost — simulating a cold restart).
    /// `NodeJoined` is emitted to all other nodes.
    pub fn restart_node(&self, node: NodeId);

    /// Gracefully shut down a node. Actors receive `on_stop` lifecycle
    /// hook before termination. `NodeLeft` is emitted to peers.
    pub fn shutdown_node(&self, node: NodeId);

    /// Freeze a node — it stops processing messages but doesn't crash.
    /// Simulates a GC pause, CPU starvation, or deadlock. Messages
    /// queue up but are not delivered until `unfreeze_node()`.
    pub fn freeze_node(&self, node: NodeId);

    /// Resume a frozen node. Queued messages are delivered.
    pub fn unfreeze_node(&self, node: NodeId);

    // ── Cluster event simulation ────────────────────────

    /// Emit a cluster event on a specific node (manual control).
    pub fn emit_event(&self, on_node: NodeId, event: ClusterEvent);

    /// Emit a cluster event on all nodes simultaneously.
    pub fn emit_event_all(&self, event: ClusterEvent);

    // ── Time control ────────────────────────────────────

    /// Advance all node clocks by the given duration (deterministic).
    pub fn advance_time(&self, duration: Duration);

    /// Advance a single node's clock (simulate clock skew).
    pub fn advance_node_time(&self, node: NodeId, duration: Duration);

    // ── Inspection / assertions ─────────────────────────

    /// Get the number of messages in flight (sent but not yet delivered)
    /// across the entire cluster.
    pub fn in_flight_count(&self) -> usize;

    /// Get the number of messages dropped since the cluster was created.
    pub fn dropped_count(&self) -> usize;

    /// Get the number of messages corrupted (deserialization failures).
    pub fn corrupted_count(&self) -> usize;

    /// Drain all in-flight messages (deliver everything immediately,
    /// ignoring latency). Useful for deterministic test assertions.
    pub fn flush(&self);
}
```

**Workspace placement:** `dactor-mock` is a peer crate alongside `dactor-ractor`
and `dactor-kameo`, listed in the workspace `Cargo.toml`:

```toml
[workspace]
members = ["dactor", "dactor-ractor", "dactor-kameo", "dactor-mock"]
```

**Dependencies:**

```toml
[package]
name = "dactor-mock"

[dependencies]
dactor = { path = "../dactor", features = ["test-support"] }
tokio = { version = "1", features = ["sync", "rt", "time"] }
serde = { version = "1", features = ["derive"] }
bincode = "1"          # default codec
```

**Relationship to `test_support`:**

| | `dactor::test_support` | `dactor-mock` |
|---|---|---|
| **Scope** | Single node | Multi-node cluster |
| **Location** | Feature-gated module in core crate | Standalone workspace crate |
| **Serialization** | No (in-memory channels) | Yes (forced encode/decode on cross-node) |
| **Cluster events** | Manual `emit()` on one runtime | Coordinated across N nodes; auto-emitted on crash/restart |
| **Network faults** | N/A | Partition, latency, jitter, drop, corruption, duplication, reordering |
| **Node faults** | N/A | Crash, restart (cold), graceful shutdown, freeze/unfreeze |
| **Error visibility** | N/A | Configurable: silent drop, connection refused, timeout |
| **Clock** | `TestClock` per runtime | `TestClock` per node + coordinated `advance_time()` + clock skew |
| **Inspection** | N/A | In-flight count, dropped count, corrupted count, `flush()` |
| **Use case** | Unit testing single actors | Integration testing cluster behavior, chaos testing |

---

## 4. Module Reorganization

### Before (v0.1)

```
dactor/src/
├── lib.rs
├── traits/
│   ├── mod.rs
│   ├── runtime.rs       ← ActorRef + ActorRuntime + errors + ClusterEvents + NodeId
│   └── clock.rs         ← Clock + SystemClock + TestClock (mixed!)
├── types/
│   ├── mod.rs
│   └── node.rs          ← NodeId
└── test_support/
    ├── mod.rs
    ├── test_runtime.rs
    └── test_clock.rs
```

### After (v0.2)

```
dactor/src/
├── lib.rs               ← public API, re-exports
├── actor.rs             ← ActorRef, ActorId, ActorRuntime, SpawnConfig
├── message.rs           ← Envelope, Headers, HeaderValue, built-in headers
├── interceptor.rs       ← Interceptor trait, Disposition
├── lifecycle.rs         ← ActorLifecycle, ErrorAction
├── supervision.rs       ← SupervisionStrategy, SupervisionAction, ChildTerminated
├── stream.rs            ← StreamRef, StreamSender, BoxStream, StreamSendError
├── clock.rs             ← Clock, SystemClock (NO TestClock)
├── cluster.rs           ← ClusterEvents, ClusterEvent, NodeId, SubscriptionId
├── timer.rs             ← TimerHandle
├── mailbox.rs           ← MailboxConfig, OverflowStrategy
├── errors.rs            ← ActorSendError, GroupError, ClusterError, RuntimeError
└── test_support/        ← #[cfg(feature = "test-support")]
    ├── mod.rs
    ├── test_runtime.rs  ← TestRuntime, TestActorRef, TestClusterEvents
    └── test_clock.rs    ← TestClock
```

---

## 5. Breaking Changes & Migration

| v0.1 | v0.2 | Migration |
|------|------|-----------|
| `ActorRef::send()` | `ActorRef::tell()` | Rename. Provide deprecated `send()` shim for one release. |
| `TestClock` in `traits::clock` | `test_support::TestClock` behind feature | Add `features = ["test-support"]` to dev-deps. |
| `test_support` always compiled | Feature-gated | Same as above. |
| No `ActorId` | `ActorRef::id()` required | Adapters must implement. |
| `NodeId` in `types::node` | `cluster::NodeId` | Module moved, re-exported from root. |
| `GroupError` return type | `RuntimeError` return type | Wrap existing errors in `RuntimeError::Group(...)`. |
| — | `NotSupportedError` / `RuntimeError` | New error types. Unsupported ops return `Err(NotSupported)`. |
| — | `Envelope<M>`, `Headers` | New types, `tell()` accepts both `M` and `Envelope<M>`. |
| — | `Interceptor` pipeline | New opt-in feature, no breakage. |
| — | `SpawnConfig` / `MailboxConfig` | New, with defaults matching v0.1 behavior. |
| — | `ActorLifecycle` | New opt-in trait. |
| — | `StreamRef<M, R>` / `BoxStream<R>` | New streaming trait. Adapters implement via channel shim. |

---

## 6. Adapter Impact

### Strategy Key

For each feature and each adapter, there are exactly three possibilities:

- ✅ **Library Native** — the underlying actor library directly supports this; the adapter maps to the library's API
- ⚙️ **Adapter Implemented** — the library does *not* support this; the adapter crate implements it with custom logic
- ❌ **Not Supported** — returns `RuntimeError::NotSupported` at runtime

### dactor-ractor

| Feature | Strategy | Implementation Detail |
|---------|:---:|---|
| `tell()` | ✅ Library | `ractor::ActorRef::cast()` |
| `tell_envelope()` | ⚙️ Adapter | ractor has no envelope concept; adapter runs interceptor chain on headers, forwards `body` to `cast()` |
| `ask()` | ✅ Library | `ractor::ActorRef::call()` |
| `stream()` | ⚙️ Adapter | ractor has no streaming; adapter creates `mpsc` channel, passes `StreamSender` to actor, returns `ReceiverStream` |
| `ActorRef::id()` | ✅ Library | `ractor::ActorRef::get_id()` → `ActorId` |
| `ActorRef::is_alive()` | ✅ Library | Check ractor actor cell liveness |
| Lifecycle hooks | ✅ Library | ractor `pre_start` / `post_stop` callbacks |
| Supervision | ✅ Library | ractor's parent-child supervision model |
| `watch()` / `unwatch()` | ✅ Library | ractor's supervisor notification system |
| `MailboxConfig::Unbounded` | ✅ Library | Default ractor behavior |
| `MailboxConfig::Bounded` | ⚙️ Adapter | ractor only has unbounded mailboxes; adapter wraps with bounded `tokio::sync::mpsc` channel |
| `OverflowStrategy::Block` | ⚙️ Adapter | ractor has no overflow control; adapter uses bounded channel which naturally blocks sender |
| `OverflowStrategy::RejectWithError` | ⚙️ Adapter | ractor has no overflow control; adapter uses `try_send()` on bounded channel |
| `OverflowStrategy::DropNewest` | ⚙️ Adapter | ractor has no overflow control; adapter uses `try_send()`, silently discards on error |
| `OverflowStrategy::DropOldest` | ❌ Not Supported | ractor has no queue eviction; no efficient way to implement in adapter |
| Interceptors (global) | ⚙️ Adapter | ractor has no interceptors; adapter stores in `Arc<Mutex<Vec>>`, runs chain before `cast()` |
| Interceptors (per-actor) | ⚙️ Adapter | ractor has no interceptors; adapter stores in actor wrapper, runs per message |
| Processing groups | ⚙️ Adapter | ractor has no processing groups; adapter maintains type-erased registry (implemented in v0.1) |
| Cluster events | ⚙️ Adapter | ractor has no unified cluster events; adapter provides `RactorClusterEvents` callback system (implemented in v0.1) |

### dactor-kameo

| Feature | Strategy | Implementation Detail |
|---------|:---:|---|
| `tell()` | ✅ Library | `kameo::ActorRef::tell().try_send()` |
| `tell_envelope()` | ⚙️ Adapter | kameo has no envelope concept; adapter runs interceptor chain on headers, forwards `body` to `tell()` |
| `ask()` | ✅ Library | `kameo::ActorRef::ask()` |
| `stream()` | ⚙️ Adapter | kameo has no streaming; adapter creates `mpsc` channel, passes `StreamSender` to actor, returns `ReceiverStream` |
| `ActorRef::id()` | ✅ Library | `kameo::actor::ActorId` → `ActorId` |
| `ActorRef::is_alive()` | ✅ Library | Check kameo actor ref validity |
| Lifecycle hooks | ✅ Library | kameo `on_start` / `on_stop` hooks |
| Supervision | ✅ Library | kameo `on_link_died` actor linking model |
| `watch()` / `unwatch()` | ✅ Library | kameo actor linking API |
| `MailboxConfig::Unbounded` | ✅ Library | `kameo::actor::Spawn::spawn()` |
| `MailboxConfig::Bounded` | ✅ Library | `kameo::actor::Spawn::spawn_bounded(capacity)` |
| `OverflowStrategy::Block` | ✅ Library | kameo bounded mailbox default behavior (blocks sender) |
| `OverflowStrategy::RejectWithError` | ✅ Library | kameo `try_send()` returns error when mailbox full |
| `OverflowStrategy::DropNewest` | ⚙️ Adapter | kameo has no drop-newest policy; adapter uses `try_send()`, silently discards on error |
| `OverflowStrategy::DropOldest` | ❌ Not Supported | kameo doesn't expose queue eviction; no efficient way to implement in adapter |
| Interceptors (global) | ⚙️ Adapter | kameo has no interceptors; adapter stores in `Arc<Mutex<Vec>>`, runs chain before `tell()` |
| Interceptors (per-actor) | ⚙️ Adapter | kameo has no interceptors; adapter stores in actor wrapper, runs per message |
| Processing groups | ⚙️ Adapter | kameo has no processing groups; adapter maintains type-erased registry (implemented in v0.1) |
| Cluster events | ⚙️ Adapter | kameo has no unified cluster events; adapter provides `KameoClusterEvents` callback system (implemented in v0.1) |

---

## 7. Dependency Cleanup

### v0.1 dactor/Cargo.toml deps:
```toml
serde = { version = "1", features = ["derive"] }   # for NodeId
tokio = { version = "1", features = ["time", "sync", "rt", "macros"] }
tracing = "0.1"
```

### v0.2 proposed:
```toml
[dependencies]
tokio = { version = "1", features = ["time", "sync", "rt"] }  # drop macros
tracing = "0.1"
futures-core = "0.3"      # Stream trait (used by StreamRef)
tokio-stream = "0.1"      # ReceiverStream wrapper

[dependencies.serde]
version = "1"
features = ["derive"]
optional = true  # only if user needs NodeId serialization

[features]
default = []
serde = ["dep:serde"]
test-support = ["tokio/test-util"]
```

---

## 8. Implementation Priority

### Phase 1 — Foundation (v0.2.0)
1. Module reorganization (flat structure, one concept per file)
2. Feature-gate `test_support` behind `test-support`
3. Move `TestClock` out of `traits/clock.rs`
4. Add `ActorId` to `ActorRef` trait
5. Rename `send()` → `tell()` (with deprecated alias)
6. Add `Envelope<M>` and `Headers`
7. Add `Interceptor` trait and pipeline
8. Clean up `serde` dependency (make optional)

### Phase 2 — Lifecycle & Config (v0.2.1)
1. Add `ActorLifecycle` trait (`on_start`, `on_stop`, `on_error`)
2. Add `MailboxConfig` and `OverflowStrategy`
3. Add `SpawnConfig` for per-actor configuration
4. Add `spawn_with_config()` to `ActorRuntime`
5. Update adapter crates

### Phase 3 — Supervision (v0.3.0)
1. Add `SupervisionStrategy` trait
2. Add `ChildTerminated` event
3. Add `watch()` / `unwatch()` to `ActorRuntime`
4. Built-in strategies: `OneForOne`, `OneForAll`, `RestForOne`
5. Add `ErrorAction::Escalate` flow

### Phase 4 — Ask Pattern & Streaming (v0.3.1)
1. Add `AskRef<M, R>` trait
2. Add `StreamRef<M, R>` trait, `StreamSender<R>`, `BoxStream<R>`
3. Implement for adapters (channel-based shim)
4. Add timeout support for ask
5. Add `futures-core` and `tokio-stream` dependencies

### Phase 5 — Mock Cluster Crate (v0.4.0)
1. Create `dactor-mock` workspace crate
2. `MockCluster` builder with multi-node setup
3. `MockRuntime` implementing `ActorRuntime` per node
4. `MockNetwork` with forced serialization on cross-node messages
5. `LinkConfig` with latency, jitter, drop, corruption, duplication, reordering
6. Network fault injection: `partition()`, `heal()`, `set_drop_rate()`, etc.
7. Node fault injection: `crash_node()`, `restart_node()`, `shutdown_node()`, `freeze_node()`
8. `LinkError` modes: `ConnectionRefused`, `Timeout`, `SilentDrop`
9. Inspection API: `in_flight_count()`, `dropped_count()`, `flush()`
10. `MessageCodec` trait with default `bincode` codec

---

## 9. Open Questions

1. **Should `Envelope<M>` be the only way to send messages?** Or should `tell(M)` auto-wrap in an envelope with empty headers? → **Proposed: both.** `tell(msg)` wraps automatically; `tell_envelope(env)` gives full control.

2. **Should interceptors be per-runtime or per-actor?** → **Proposed: both.** Global interceptors via `runtime.add_interceptor()`, per-actor via `SpawnConfig`.

3. **Should `ActorLifecycle` be a separate trait or methods on the handler?** → **Proposed: separate trait.** Simple actors use closures (no lifecycle); stateful actors implement `ActorLifecycle`.

4. **Should dactor provide a `Registry` (named actor lookup)?** → **Deferred to v0.4.** 5 of 6 frameworks support it (qualifies under superset rule), but adapters already have their own registry mechanisms. Design to be informed by adapter experience.

5. **How to handle `serde` for `NodeId`?** → **Make it a feature.** `NodeId` gets `Serialize/Deserialize` only with `features = ["serde"]`.

6. **Should `NotSupported` be a compile-time or runtime error?** → **Runtime.** Rust's trait system with GATs can't express "this adapter supports method X" at the type level without fragmenting the trait hierarchy. A single `ActorRuntime` trait with `Result<_, RuntimeError>` is simpler and more ergonomic. Adapters document their support matrix.

7. **What's the threshold for adapter-level shims vs NotSupported?** → **Effort and correctness.** If the adapter can implement the feature correctly with reasonable overhead (e.g., bounded channel wrapper for ractor), use a shim. If the emulation would be incorrect, surprising, or prohibitively expensive (e.g., DropOldest requires draining a queue), return `NotSupported`.

8. **Should `stream()` support bidirectional streaming?** → **Deferred.** Start with request-stream (one request, many responses). Bidirectional streaming (many-to-many) can be added later as a separate `BidiStreamRef` trait if there is demand. The channel-based approach naturally extends to this.
