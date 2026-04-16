# dactor Usage Guide

> **A provider-agnostic distributed actor framework for Rust.**
>
> Write your actor logic once. Swap the runtime underneath.

---

## Table of Contents

- [1. What is dactor?](#1-what-is-dactor)
  - [The Actor Model](#the-actor-model)
  - [Core Abstractions](#core-abstractions)
- [2. Why dactor?](#2-why-dactor)
  - [The Vendor Lock-in Problem](#the-vendor-lock-in-problem)
  - [dactor's Solution](#dactors-solution)
  - [The Testing Story](#the-testing-story)
  - [Feature Completeness](#feature-completeness)
  - [Comparison Table](#comparison-table)
- [3. Getting Started](#3-getting-started)
  - [Installation](#installation)
  - [Define an Actor](#define-an-actor)
  - [Define Messages](#define-messages)
  - [Implement Handlers](#implement-handlers)
  - [Spawn and Interact](#spawn-and-interact)
  - [Full Runnable Example](#full-runnable-example)
- [4. Communication Patterns](#4-communication-patterns)
  - [Tell (Fire-and-Forget)](#tell-fire-and-forget)
  - [Ask (Request-Reply)](#ask-request-reply)
  - [Expand (Server-Streaming)](#expand-server-streaming)
  - [Reduce (Client-Streaming)](#reduce-client-streaming)
  - [Transform (Bidirectional Streaming)](#transform-bidirectional-streaming)
  - [Broadcast (Fan-Out)](#broadcast-fan-out)
  - [Pattern Summary](#pattern-summary)
- [5. Interceptors](#5-interceptors)
  - [Inbound Interceptors](#inbound-interceptors)
  - [Outbound Interceptors](#outbound-interceptors)
  - [Interceptor Disposition](#interceptor-disposition)
  - [DropObserver](#dropobserver)
  - [Built-in Interceptors](#built-in-interceptors)
- [6. Actor Lifecycle](#6-actor-lifecycle)
  - [Lifecycle Hooks](#lifecycle-hooks)
  - [Error Handling](#error-handling)
  - [Supervision Strategies](#supervision-strategies)
  - [DeathWatch](#deathwatch)
  - [Lifecycle Handles](#lifecycle-handles)
- [7. Actor Pools](#7-actor-pools)
  - [Creating a Pool](#creating-a-pool)
  - [Routing Strategies](#routing-strategies)
  - [Key-Based Routing](#key-based-routing)
  - [Distributed Pools with WorkerRef](#distributed-pools-with-workerref)
  - [VirtualPoolRef](#virtualpoolref)
- [8. Persistence](#8-persistence)
  - [Event Sourcing](#event-sourcing)
  - [Durable State](#durable-state)
  - [InMemoryStorage for Testing](#inmemorystorage-for-testing)
  - [Recovery](#recovery)
  - [Snapshot Configuration](#snapshot-configuration)
- [9. Remote Actors](#9-remote-actors)
  - [WireEnvelope Wire Format](#wireenvelope-wire-format)
  - [MessageSerializer](#messageserializer)
  - [RemoteActorRef](#remoteactorref)
  - [Transport Trait](#transport-trait)
  - [Protobuf System Messages](#protobuf-system-messages)
- [10. Cluster Management](#10-cluster-management)
  - [Cluster Events](#cluster-events)
  - [Cluster Discovery](#cluster-discovery)
  - [System Actors](#system-actors)
- [11. Observability](#11-observability)
  - [Metrics](#metrics)
  - [Dead Letter Handling](#dead-letter-handling)
  - [Circuit Breaker](#circuit-breaker)
  - [Rate Limiting](#rate-limiting)
- [12. Mailbox Configuration](#12-mailbox-configuration)
  - [Bounded Mailboxes](#bounded-mailboxes)
  - [Overflow Strategies](#overflow-strategies)
  - [Priority Mailboxes](#priority-mailboxes)
- [13. Cancellation](#13-cancellation)
  - [Cancellation Tokens](#cancellation-tokens)
  - [Cooperative Cancellation in Handlers](#cooperative-cancellation-in-handlers)
  - [Timed Cancellation](#timed-cancellation)
- [14. Testing](#14-testing)
  - [TestRuntime for Unit Tests](#testruntime-for-unit-tests)
  - [Conformance Suite](#conformance-suite)
  - [MockCluster for Integration Tests](#mockcluster-for-integration-tests)
  - [gRPC Test Harness for E2E Tests](#grpc-test-harness-for-e2e-tests)
- [15. Choosing an Adapter](#15-choosing-an-adapter)
  - [Ractor](#ractor)
  - [Kameo](#kameo)
  - [Coerce](#coerce)
  - [Switching Adapters](#switching-adapters)
- [16. Architecture](#16-architecture)
  - [Crate Layout](#crate-layout)
  - [Trait Hierarchy](#trait-hierarchy)
- [17. Examples](#17-examples)

---

## 1. What is dactor?

**dactor** is a framework-agnostic actor abstraction for Rust. It defines a
unified set of traits for actor spawning, message delivery, supervision,
streaming, persistence, and cluster membership — without coupling your code
to any specific actor runtime.

Concrete runtimes are plugged in via **adapter crates**:

| Adapter | Runtime |
|---------|---------|
| `dactor-ractor` | [ractor](https://crates.io/crates/ractor) |
| `dactor-kameo` | [kameo](https://crates.io/crates/kameo) |
| `dactor-coerce` | [coerce](https://crates.io/crates/coerce) |

Your application code depends on `dactor` (the core crate) and one adapter.
Switching runtimes means changing a single dependency line — your actors,
messages, and handlers stay exactly the same.

### The Actor Model

The [actor model](https://en.wikipedia.org/wiki/Actor_model) is a
concurrency paradigm where **actors** are the fundamental unit of
computation. Each actor:

- **Encapsulates state** — no shared mutable memory between actors
- **Communicates via messages** — actors interact exclusively by sending
  asynchronous messages to each other
- **Processes one message at a time** — sequential execution within an actor
  eliminates data races by construction
- **Can create child actors** — actors form supervision hierarchies for
  fault tolerance

This model naturally maps to distributed systems: actors don't care whether
messages arrive from the same process, another thread, or across the
network. That location transparency is what makes actor systems scalable.

### Core Abstractions

dactor's API is built on a small set of traits:

| Trait | Purpose |
|-------|---------|
| `Actor` | Core actor trait with `create()`, `on_start()`, `on_stop()`, `on_error()` |
| `Handler<M>` | Per-message handler — `async fn handle(&mut self, msg, ctx) -> M::Reply` |
| `ExpandHandler<M, Out>` | Server-streaming — sends items via `StreamSender` |
| `ReduceHandler<In, Reply>` | Client-streaming — receives `StreamReceiver<In>`, returns `Reply` |
| `TransformHandler<In, Out>` | Bidirectional streaming — N inputs → M outputs |
| `ActorRef<A>` | Typed handle — `tell`, `ask`, `expand`, `reduce`, `transform`, `stop` |
| `Message` | Message trait with associated `Reply` type |
| `InboundInterceptor` | Runs on actor task before/after handler execution |
| `OutboundInterceptor` | Runs on caller task before message send |

---

## 2. Why dactor?

### The Vendor Lock-in Problem

The Rust ecosystem has several excellent actor frameworks — ractor, kameo,
coerce, actix — but they each have completely different APIs. If you build
your application on ractor and later discover that kameo's feature set is a
better fit, you're looking at a **full rewrite** of every actor, message,
and handler.

This problem gets worse in large teams where different services may want
different runtimes, or where you need to evaluate multiple frameworks before
committing.

### dactor's Solution

dactor provides a **single, unified API** that compiles against multiple
runtimes. Your actor code targets dactor's traits:

```text
┌──────────────────────────────────────────────────┐
│                Application Code                  │
├──────────────────────────────────────────────────┤
│           dactor (core traits + types)           │
│  Actor · Handler · ActorRef · Message            │
│  Interceptors · Lifecycle · Persistence          │
├──────────┬──────────┬──────────┬─────────────────┤
│  dactor  │  dactor  │  dactor  │  dactor-mock    │
│  -ractor │  -kameo  │  -coerce │  (test cluster) │
└──────────┴──────────┴──────────┴─────────────────┘
```

**Switching adapters is a 2-line change** in your `Cargo.toml` — swap the
adapter crate and its feature flag. No actor code changes required.

### The Testing Story

dactor was designed with testing as a first-class concern:

| Level | Tool | What it tests |
|-------|------|---------------|
| **Unit** | `TestRuntime` | Individual actor logic with in-memory mailboxes |
| **Conformance** | Conformance suite | 25+ tests verifying adapter correctness |
| **Integration** | `MockCluster` | Multi-node simulation with fault injection |
| **E2E** | gRPC test harness | 60 multi-process tests across 3 adapters |

Your unit tests run with `TestRuntime` (no real runtime needed), your
integration tests use `MockCluster` to simulate network partitions, and
your E2E tests run against real adapter binaries over gRPC.

### Feature Completeness

dactor doesn't just abstract the lowest common denominator — it abstracts
the **superset** of capabilities supported by 2 or more surveyed
frameworks. If a capability is common to at least two of Erlang/OTP, Akka,
ractor, kameo, Actix, and Coerce, dactor models it as a first-class trait.

Key features:
- 5 communication patterns (tell, ask, expand, reduce, transform)
- Broadcast messaging and processing groups
- Interceptor pipelines (inbound + outbound)
- Actor pools with 4 routing strategies
- Supervision (OneForOne, AllForOne, RestForOne)
- DeathWatch with ChildTerminated notifications
- Event sourcing and durable state persistence
- Bounded mailboxes with overflow strategies
- Cooperative cancellation
- Metrics, circuit breakers, and rate limiting
- Remote actor references and pluggable transport
- Protobuf-based system message serialization

### Comparison Table

| Feature | dactor | Raw ractor | Raw kameo | Raw coerce |
|---------|:------:|:----------:|:---------:|:----------:|
| Tell (fire-and-forget) | ✅ | ✅ | ✅ | ✅ |
| Ask (request-reply) | ✅ | ✅ | ✅ | ✅ |
| Server-streaming (expand) | ✅ | ❌ | ❌ | ❌ |
| Client-streaming (reduce) | ✅ | ❌ | ❌ | ❌ |
| Bidirectional streaming | ✅ | ❌ | ❌ | ❌ |
| Broadcast groups | ✅ | ❌ | ❌ | ❌ |
| Interceptor pipelines | ✅ | ❌ | ❌ | partial |
| Actor pools | ✅ | ✅ | ✅ | partial |
| Supervision strategies | ✅ | ✅ | ✅ | ✅ |
| Bounded mailboxes | ✅ | ❌ | ✅ | ❌ |
| Event sourcing / persistence | ✅ | ❌ | ❌ | ✅ |
| Metrics interceptor | ✅ | ❌ | ❌ | ❌ |
| Circuit breaker | ✅ | ❌ | ❌ | ❌ |
| Rate limiting | ✅ | ❌ | ❌ | ❌ |
| Cooperative cancellation | ✅ | ❌ | ❌ | ❌ |
| Pluggable runtime | ✅ | N/A | N/A | N/A |
| Built-in test runtime | ✅ | ❌ | ❌ | ❌ |
| Conformance suite | ✅ | N/A | N/A | N/A |

---

## 3. Getting Started

### Installation

Add the core crate and an adapter to your `Cargo.toml`:

```toml
[dependencies]
dactor = { version = "0.2", features = ["test-support"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

For development and testing, the `test-support` feature provides
`TestRuntime` — an in-memory actor runtime that requires no external
dependencies. When you're ready to run against a real runtime, add an
adapter:

```toml
[dependencies]
dactor = "0.2"
dactor-ractor = "0.2"   # or dactor-kameo, dactor-coerce
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

> **Note:** Building dactor requires the `protoc` protobuf compiler. Install
> via `brew install protobuf` (macOS), `apt install protobuf-compiler`
> (Linux), or `choco install protoc` (Windows).

### Define an Actor

An actor is a struct that implements the `Actor` trait. The trait has two
associated types and one required method:

```rust
use dactor::prelude::*;

struct Counter {
    count: u64,
}

impl Actor for Counter {
    /// Serializable arguments passed to `spawn()`.
    type Args = Self;

    /// Non-serializable local dependencies (use `()` if none).
    type Deps = ();

    /// Construct the actor from args and deps. Called by the runtime.
    fn create(args: Self, _deps: ()) -> Self {
        args
    }
}
```

- **`Args`** — the data needed to create an actor. Must be `Send + 'static`.
  For remote spawning, this is what gets serialized and sent to the target
  node.
- **`Deps`** — local, non-serializable dependencies (database connections,
  shared state, etc.). Resolved at the target node. Use `()` if none.
- **`create()`** — synchronous constructor. For async initialization, use
  the `on_start()` lifecycle hook.

### Define Messages

Messages are structs (or enums) that implement the `Message` trait. The
trait requires one associated type: `Reply`.

```rust
use dactor::message::Message;

/// Fire-and-forget message — reply is `()`.
struct Increment(u64);
impl Message for Increment {
    type Reply = ();
}

/// Request-reply message — reply is `u64`.
struct GetCount;
impl Message for GetCount {
    type Reply = u64;
}
```

When `Reply = ()`, the message can be used with both `tell()` and `ask()`.
When `Reply` is a concrete type, it should be used with `ask()`.

### Implement Handlers

Each `(Actor, Message)` pair gets its own `Handler` implementation.
Handlers are async and have exclusive access to `&mut self` — no
`Arc<Mutex<…>>` needed.

```rust
use dactor::prelude::*;
# use dactor::message::Message;
# struct Counter { count: u64 }
# impl Actor for Counter {
#     type Args = Self; type Deps = ();
#     fn create(args: Self, _deps: ()) -> Self { args }
# }
# struct Increment(u64);
# impl Message for Increment { type Reply = (); }
# struct GetCount;
# impl Message for GetCount { type Reply = u64; }

#[dactor::async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

#[dactor::async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}
```

- Each handler receives `&mut ActorContext` which provides access to the
  actor's identity, message headers, and cancellation tokens.
- Handlers execute sequentially — the runtime guarantees that only one
  handler runs at a time per actor.

### Spawn and Interact

Use a runtime to spawn actors and get back an `ActorRef`:

```rust,ignore
use dactor::test_support::test_runtime::TestRuntime;

let runtime = TestRuntime::new();
let counter = runtime
    .spawn::<Counter>("counter", Counter { count: 0 })
    .await
    .unwrap();

// Fire-and-forget
counter.tell(Increment(5)).unwrap();
counter.tell(Increment(3)).unwrap();

// Wait for messages to be processed
tokio::time::sleep(std::time::Duration::from_millis(50)).await;

// Request-reply
let count = counter.ask(GetCount, None).unwrap().await.unwrap();
assert_eq!(count, 8);

// Graceful shutdown
counter.stop();
```

### Full Runnable Example

```rust
use dactor::prelude::*;
use dactor::message::Message;
use dactor::test_support::test_runtime::TestRuntime;

struct Counter { count: u64 }

impl Actor for Counter {
    type Args = Self;
    type Deps = ();
    fn create(args: Self, _deps: ()) -> Self { args }
}

struct Increment(u64);
impl Message for Increment { type Reply = (); }

struct GetCount;
impl Message for GetCount { type Reply = u64; }

#[dactor::async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

#[dactor::async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

#[tokio::main]
async fn main() {
    let runtime = TestRuntime::new();
    let counter = runtime
        .spawn::<Counter>("counter", Counter { count: 0 })
        .await
        .unwrap();

    counter.tell(Increment(5)).unwrap();
    counter.tell(Increment(3)).unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let count = counter.ask(GetCount, None).unwrap().await.unwrap();
    println!("Count: {count}"); // Count: 8
}
```

Run it:

```bash
cargo run --example readme_quickstart -p dactor --features test-support
```

---

## 4. Communication Patterns

dactor supports five communication patterns that cover the full spectrum of
actor interactions — from simple fire-and-forget to complex bidirectional
streaming.

### Tell (Fire-and-Forget)

Send a message with no reply expected. Returns immediately after the message
is enqueued in the actor's mailbox.

```rust,ignore
// The message must have Reply = ()
counter.tell(Increment(1)).unwrap();
```

**When to use:** Commands, event notifications, side effects where you don't
need confirmation.

### Ask (Request-Reply)

Send a message and await the reply. Returns an `AskReply<R>` future.

```rust,ignore
// .ask() returns Result<AskReply<R>, ActorSendError>
// AskReply is a future — .await it for the reply
let count = counter.ask(GetCount, None).unwrap().await.unwrap();

// With a cancellation token
use dactor::CancellationToken;
let token = CancellationToken::new();
let count = counter.ask(GetCount, Some(token)).unwrap().await.unwrap();
```

**When to use:** Queries, operations where the caller needs the result,
request-response patterns.

### Expand (Server-Streaming)

Send a single request and receive a stream of responses. The handler pushes
items into a `StreamSender`.

```rust,ignore
use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ExpandHandler};
use dactor::stream::StreamSender;

struct GetLogs;

#[async_trait]
impl ExpandHandler<GetLogs, String> for LogServer {
    async fn handle_expand(
        &mut self,
        _msg: GetLogs,
        sender: StreamSender<String>,
        _ctx: &mut ActorContext,
    ) {
        for log in &self.logs {
            if sender.send(log.clone()).await.is_err() {
                break; // consumer disconnected
            }
        }
    }
}

// Caller side: buffer=16, no batching, no cancellation
let mut stream = log_server.expand(GetLogs, 16, None, None).unwrap();
while let Some(entry) = stream.next().await {
    println!("Log: {entry}");
}
```

**When to use:** Log streaming, database cursors, paginated results,
real-time event feeds.

### Reduce (Client-Streaming)

Stream items to an actor and get a single reply when the stream ends.

```rust,ignore
use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ReduceHandler};
use dactor::stream::StreamReceiver;

#[async_trait]
impl ReduceHandler<u64, u64> for Aggregator {
    async fn handle_reduce(
        &mut self,
        mut receiver: StreamReceiver<u64>,
        _ctx: &mut ActorContext,
    ) -> u64 {
        let mut total = 0u64;
        while let Some(n) = receiver.recv().await {
            total += n;
        }
        total
    }
}

// Caller side
let input = futures::stream::iter(vec![10u64, 20, 30, 40, 50]);
let sum = aggregator
    .reduce::<u64, u64>(Box::pin(input), 8, None, None)
    .unwrap()
    .await
    .unwrap();
assert_eq!(sum, 150);
```

**When to use:** Aggregation, bulk uploads, batch processing, ETL pipelines.

### Transform (Bidirectional Streaming)

Stream items in and receive a stream of outputs. Each input item can produce
zero or more output items.

```rust,ignore
use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, TransformHandler};
use dactor::stream::StreamSender;

#[async_trait]
impl TransformHandler<i32, String> for Transformer {
    async fn handle_transform(
        &mut self,
        item: i32,
        sender: &StreamSender<String>,
        _ctx: &mut ActorContext,
    ) {
        // Each input produces one output
        let _ = sender.send(format!("processed: {item}")).await;
    }

    async fn on_transform_complete(
        &mut self,
        sender: &StreamSender<String>,
        _ctx: &mut ActorContext,
    ) {
        // Emit a final summary when the input stream ends
        let _ = sender.send("done".to_string()).await;
    }
}

// Caller side
let input = futures::stream::iter(vec![1, 2, 3]);
let mut output = transformer
    .transform::<i32, String>(Box::pin(input), 8, None, None)
    .unwrap();
while let Some(item) = output.next().await {
    println!("{item}");
}
```

**When to use:** Data transformation pipelines, protocol translation,
real-time filtering and enrichment.

### Broadcast (Fan-Out)

`BroadcastRef` fans out messages to all members of a group concurrently.

```rust,ignore
use dactor::BroadcastRef;

let group = BroadcastRef::new(vec![worker1, worker2, worker3]);

// Fire-and-forget to all members (returns BroadcastTellResult, not Result)
let result = group.tell(DoWork);
// result contains per-actor outcomes (Ok, SendError)

// Ask all members with a per-actor timeout
use std::time::Duration;
let receipts = group.ask(GetStatus, Duration::from_secs(1)).await;
for receipt in &receipts {
    match receipt {
        BroadcastReceipt::Ok { actor_id, reply } => {
            println!("{actor_id}: {reply}");
        }
        BroadcastReceipt::Timeout { actor_id } => {
            println!("{actor_id}: timed out");
        }
        _ => {}
    }
}
```

**When to use:** Fan-out/fan-in, consensus, health checks across a group,
scatter-gather queries.

### Pattern Summary

| Pattern | Method | Direction | Reply |
|---------|--------|-----------|-------|
| **Tell** | `actor.tell(msg)` | 1→1 | None |
| **Ask** | `actor.ask(msg, cancel)` | 1→1 | Single reply |
| **Expand** | `actor.expand(msg, buf, batch, cancel)` | 1→N | Stream of items |
| **Reduce** | `actor.reduce(input, buf, batch, cancel)` | N→1 | Single reply |
| **Transform** | `actor.transform(input, buf, batch, cancel)` | N→M | Stream of items |
| **Broadcast** | `group.tell(msg)` / `group.ask(msg, timeout)` | 1→all | Per-actor receipts |

All streaming patterns support optional **`BatchConfig`** for transparent
batching (amortize per-item overhead) and **`CancellationToken`** for
cooperative cancellation.

---

## 5. Interceptors

Interceptors are hooks that run **before** and **after** message handling,
allowing you to add cross-cutting concerns (logging, auth, metrics, rate
limiting) without modifying actor code.

### Inbound Interceptors

Inbound interceptors run on the **actor's task**, surrounding the handler
invocation. They are attached per-actor at spawn time.

```rust,ignore
use std::any::Any;
use dactor::interceptor::{
    Disposition, InboundContext, InboundInterceptor, Outcome,
};
use dactor::message::{Headers, RuntimeHeaders};

struct LoggingInterceptor;

impl InboundInterceptor for LoggingInterceptor {
    fn name(&self) -> &'static str { "logging" }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        println!("[{}] received {}", ctx.actor_name, ctx.message_type);
        Disposition::Continue // or Disposition::Drop to reject
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        println!("[{}] completed {:?}", ctx.actor_name, outcome);
    }
}
```

Attach interceptors via `SpawnOptions`:

```rust,ignore
use dactor::{SpawnOptions, TestRuntime};
use dactor::mailbox::MailboxConfig;

let runtime = TestRuntime::new();
let actor = runtime.spawn_with_options::<MyActor>(
    "my-actor",
    args,
    SpawnOptions {
        interceptors: vec![
            Box::new(LoggingInterceptor),
            Box::new(TimingInterceptor::new()),
        ],
        mailbox: MailboxConfig::Unbounded,
    },
).await.unwrap();
```

### Outbound Interceptors

Outbound interceptors run on the **caller's task**, before the message
reaches the actor's mailbox. They are registered globally on the runtime.

```rust,ignore
use dactor::interceptor::{
    Disposition, OutboundContext, OutboundInterceptor,
};
use dactor::message::{Headers, Priority, RuntimeHeaders};

struct HeaderStampInterceptor;

impl OutboundInterceptor for HeaderStampInterceptor {
    fn name(&self) -> &'static str { "header-stamp" }

    fn on_send(
        &self,
        ctx: &OutboundContext<'_>,
        _rh: &RuntimeHeaders,
        headers: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        headers.insert(Priority::HIGH);
        Disposition::Continue
    }
}

// Register before spawning actors
let mut runtime = TestRuntime::new();
runtime.add_outbound_interceptor(Box::new(HeaderStampInterceptor));
```

### Interceptor Disposition

The `Disposition` enum controls what happens after an interceptor runs:

| Variant | Effect |
|---------|--------|
| `Disposition::Continue` | Proceed to the next interceptor or handler |
| `Disposition::Delay(duration)` | Delay the message by the specified duration before proceeding |
| `Disposition::Drop` | Silently discard the message |
| `Disposition::Reject(reason)` | Reject the message with a reason string |
| `Disposition::Retry(duration)` | Return immediately with a retry hint; the caller decides when to resend |

When a message is dropped, the `DropObserver` (if registered) is notified.

### DropObserver

Register a global observer to be notified whenever an interceptor drops a
message:

```rust,ignore
use dactor::interceptor::{DropNotice, DropObserver};

struct MetricsDropObserver;

impl DropObserver for MetricsDropObserver {
    fn on_drop(&self, notice: DropNotice) {
        println!(
            "Message dropped by '{}': {} → {}",
            notice.interceptor_name, notice.message_type, notice.target_name
        );
    }
}
```

### Built-in Interceptors

dactor ships with several production-ready interceptors:

| Interceptor | Type | Purpose |
|-------------|------|---------|
| `MetricsInterceptor` | Inbound | Message counts, latency percentiles, error rates |
| `CircuitBreakerInterceptor` | Inbound | Fault isolation (Closed → Open → HalfOpen) |
| `ActorRateLimiter` | Outbound | Tumbling-window rate limiting |
| `MaxBodySizeInterceptor` | Wire | Reject oversized remote messages |
| `RateLimitWireInterceptor` | Wire | Rate limit incoming wire envelopes |

---

## 6. Actor Lifecycle

### Lifecycle Hooks

Every actor has three lifecycle hooks. `on_start` and `on_stop` default
to no-ops; `on_error` defaults to returning `ErrorAction::Stop`:

```rust,ignore
#[async_trait]
impl Actor for MyActor {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self { MyActor }

    /// Called after spawn, before any messages. Use for async init.
    async fn on_start(&mut self, ctx: &mut ActorContext) {
        println!("Actor {} started", ctx.actor_name);
    }

    /// Called when the actor is stopping. Use for cleanup.
    async fn on_stop(&mut self) {
        println!("Actor stopping — releasing resources");
    }

    /// Called on handler error/panic. Returns what to do next.
    fn on_error(&mut self, error: &ActorError) -> ErrorAction {
        eprintln!("Error: {error}");
        ErrorAction::Resume // keep running
    }
}
```

### Error Handling

When a handler returns an error or panics, `on_error()` is called. It
returns an `ErrorAction` that tells the runtime what to do:

| Action | Behavior |
|--------|----------|
| `ErrorAction::Resume` | Skip the failed message and continue processing |
| `ErrorAction::Restart` | Restart the actor (call `create()` + `on_start()` again) |
| `ErrorAction::Stop` | Stop the actor permanently |
| `ErrorAction::Escalate` | Propagate the failure to the supervisor |

`ActorError` carries structured error information inspired by gRPC status
codes:

```rust,ignore
use dactor::actor::ActorError;
use dactor::errors::ErrorCode;

let err = ActorError::new(ErrorCode::InvalidArgument, "age must be positive")
    .with_details(r#"{"field": "age", "value": -1}"#)
    .with_cause(ActorError::internal("validation failed"));
```

### Supervision Strategies

Supervisors control how the system reacts when child actors fail. dactor
provides three built-in strategies:

| Strategy | Behavior |
|----------|----------|
| `OneForOne` | Only the failed child is restarted |
| `AllForOne` | All children are restarted when any child fails |
| `RestForOne` | The failed child and all children started after it are restarted |

All strategies support configurable restart limits to prevent restart storms:

```rust,ignore
use dactor::supervision::{OneForOne, AllForOne, RestForOne};
use std::time::Duration;

// Allow at most 3 restarts within 60 seconds
let strategy = OneForOne::new(3, Duration::from_secs(60));
```

If the restart limit is exceeded, the strategy stops the actor (returns
`SupervisionAction::Stop`).

### DeathWatch

A **watcher** actor can monitor another actor and receive a
`ChildTerminated` notification when it stops:

```rust,ignore
use dactor::supervision::ChildTerminated;

// Register the watch
runtime.watch(&supervisor, worker.id());

// The supervisor must implement Handler<ChildTerminated>
#[async_trait]
impl Handler<ChildTerminated> for Supervisor {
    async fn handle(&mut self, msg: ChildTerminated, _ctx: &mut ActorContext) {
        let reason = msg.reason.as_deref().unwrap_or("graceful shutdown");
        println!(
            "Child '{}' terminated: {}",
            msg.child_name, reason
        );
    }
}
```

The `ChildTerminated` message includes:
- `child_id` — the terminated actor's ID
- `child_name` — the name it was spawned with
- `reason` — `None` for graceful shutdown, `Some(reason)` for failures

### Lifecycle Handles

`ActorRef` provides `is_alive()` and `stop()` for basic lifecycle control:

```rust,ignore
assert!(actor.is_alive());

actor.stop(); // triggers on_stop, closes mailbox
tokio::time::sleep(Duration::from_millis(50)).await;

assert!(!actor.is_alive());
```

---

## 7. Actor Pools

Actor pools distribute work across multiple worker instances using
configurable routing strategies. A `PoolRef` implements `ActorRef<A>`, so it
can be used as a drop-in replacement for a single actor reference.

### Creating a Pool

```rust,ignore
use dactor::pool::{PoolRef, PoolRouting};

let workers = vec![
    runtime.spawn::<Worker>("w-0", args0).await.unwrap(),
    runtime.spawn::<Worker>("w-1", args1).await.unwrap(),
    runtime.spawn::<Worker>("w-2", args2).await.unwrap(),
    runtime.spawn::<Worker>("w-3", args3).await.unwrap(),
];

let pool = PoolRef::new(workers, PoolRouting::RoundRobin);

// Use the pool exactly like a single ActorRef
pool.tell(Task).unwrap();
let result = pool.ask(Query, None).unwrap().await.unwrap();
```

### Routing Strategies

| Strategy | Algorithm | Best For |
|----------|-----------|----------|
| `RoundRobin` | Cycle through workers in order | Uniform workloads |
| `Random` | Pick a random worker | Simple load distribution |
| `KeyBased` | Hash the message's routing key | Session affinity, partitioned state |
| `LeastLoaded` | Pick the worker with fewest pending messages | Variable processing times |

### Key-Based Routing

For sticky routing, implement the `Keyed` trait on your message:

```rust,ignore
use dactor::pool::Keyed;

struct OrderRequest {
    customer_id: u64,
}

impl Keyed for OrderRequest {
    fn routing_key(&self) -> u64 {
        self.customer_id
    }
}

impl Message for OrderRequest {
    type Reply = u64;
}

// Messages with the same key always go to the same worker
let result = pool.ask_keyed(OrderRequest { customer_id: 42 }, None)
    .unwrap()
    .await
    .unwrap();
```

### Distributed Pools with WorkerRef

`WorkerRef` wraps either a local `ActorRef` or a `RemoteActorRef`, enabling
pools that span multiple nodes:

```rust,ignore
use dactor::worker_ref::WorkerRef;
use dactor::pool::{PoolRef, PoolRouting};

let workers = vec![
    WorkerRef::Local(local_ref),
    WorkerRef::Remote(remote_ref),
];

let pool = PoolRef::new(workers, PoolRouting::RoundRobin);
pool.tell(Task).unwrap(); // routes to local or remote transparently
```

### VirtualPoolRef

`VirtualPoolRef` routes all decisions through a **single tokio task**,
providing zero-contention metrics and deterministic routing. The architecture
is:

```text
Caller → [mpsc channel] → RouterTask → Worker-N → reply direct to caller
```

This is useful when you need strict ordering guarantees on routing decisions
or want contention-free metrics collection.

---

## 8. Persistence

dactor provides two persistence patterns for actors that need durable state:
**Event Sourcing** and **Durable State**.

### Event Sourcing

With event sourcing, state changes are persisted as a sequence of events.
On recovery, the actor replays events to rebuild its state.

The storage layer consists of two traits:

- **`JournalStorage`** — append events, read events, query sequences
- **`SnapshotStorage`** — save/load point-in-time snapshots

```rust,ignore
use dactor::persistence::*;

let storage = InMemoryStorage::new();
let pid = PersistenceId::new("BankAccount", "acct-42");

// Write events to the journal
storage.write_event(&pid, SequenceId(1), "Deposited", b"100").await?;
storage.write_event(&pid, SequenceId(2), "Withdrawn", b"30").await?;
storage.write_event(&pid, SequenceId(3), "Deposited", b"50").await?;

// Save a snapshot at sequence 3
storage.save_snapshot(&pid, SequenceId(3), b"balance=120").await?;

// Read events from a specific sequence (for replay after snapshot)
let events = storage.read_events(&pid, SequenceId(4)).await?;
```

### Durable State

For simpler cases, the **Durable State** pattern saves and loads the
actor's entire state as a blob:

```rust,ignore
use dactor::persistence::*;

let storage = InMemoryStorage::new();
let pid = PersistenceId::new("Config", "app-settings");

// Save state
storage.save_state(&pid, b"{\"theme\": \"dark\"}").await?;

// Load state
let state = storage.load_state(&pid).await?;

// Delete state
storage.delete_state(&pid).await?;
```

### InMemoryStorage for Testing

`InMemoryStorage` implements `JournalStorage`, `SnapshotStorage`, and
`StateStorage` using in-memory `HashMap`s. It's designed for testing and
development — no external dependencies required.

```rust,ignore
use dactor::persistence::InMemoryStorage;

let storage = InMemoryStorage::new();
// Use with any persistence operation — everything stays in memory
```

### Recovery

dactor provides two recovery functions that orchestrate the full recovery
pipeline:

```rust,ignore
use dactor::persistence::{recover_event_sourced, recover_durable_state};

// Event-sourced recovery: load snapshot → replay events → ready
recover_event_sourced(&mut actor, &journal, &snapshots).await?;

// Durable-state recovery: load state → apply → ready
recover_durable_state(&mut actor, &state_storage).await?;
```

Recovery behavior is controlled by `RecoveryFailurePolicy`:

| Policy | Behavior |
|--------|----------|
| `Stop` (default) | Stop the actor if recovery fails |
| `Retry { max_attempts, initial_delay }` | Retry with exponential backoff |
| `SkipAndStart` | Skip recovery and start with default state |

### Snapshot Configuration

`SnapshotConfig` controls automatic snapshotting for event-sourced actors:

```rust,ignore
use dactor::persistence::SnapshotConfig;

let config = SnapshotConfig {
    every_n_events: Some(100),           // Snapshot every 100 events
    interval: None,                       // No time-based snapshots
    retention_count: Some(3),             // Keep at most 3 snapshots
    delete_events_on_snapshot: true,       // Clean up old events
};
```

---

## 9. Remote Actors

dactor provides the building blocks for distributed actor systems: a wire
format, serialization traits, remote references, and a pluggable transport
layer.

### WireEnvelope Wire Format

All remote messages travel as `WireEnvelope` structs over the network:

```rust,ignore
use dactor::remote::WireEnvelope;

// A WireEnvelope carries:
// - target: ActorId of the destination actor
// - target_name: human-readable name
// - message_type: Rust type name for deserialization dispatch
// - send_mode: Tell, Ask, Expand, Reduce, or Transform
// - headers: serialized headers (WireHeaders)
// - body: serialized message payload
// - request_id: UUID for correlating ask replies (None for tell)
// - version: optional message version for schema evolution
```

### MessageSerializer

The `MessageSerializer` trait abstracts serialization for wire transport.
Implement it to use any serialization format (JSON, MessagePack, bincode,
etc.):

```rust,ignore
use dactor::remote::MessageSerializer;

struct MySerializer;

impl MessageSerializer for MySerializer {
    fn name(&self) -> &'static str { "json" }

    fn serialize(&self, value: &dyn std::any::Any) -> Result<Vec<u8>, SerializationError> {
        // Your serialization logic here
        todo!()
    }

    fn deserialize(
        &self,
        bytes: &[u8],
        type_name: &str,
    ) -> Result<Box<dyn std::any::Any + Send>, SerializationError> {
        // Your deserialization logic here
        todo!()
    }
}
```

dactor includes a `JsonSerializer` (behind the `serde` feature flag) for
JSON-based serialization.

### RemoteActorRef

`RemoteActorRef<A>` implements `ActorRef<A>` for actors on remote nodes.
Messages are serialized into `WireEnvelope`s and sent through a `Transport`.

> **Note:** The `register_tell` / `register_ask` convenience methods require
> the `serde` feature: `dactor = { version = "0.2", features = ["serde"] }`

```rust,ignore
use dactor::remote_ref::RemoteActorRefBuilder;

let remote = RemoteActorRefBuilder::<MyActor>::new(actor_id, "counter", transport)
    .register_tell::<Increment>()
    .register_ask::<GetCount>()
    .build();

// Use exactly like a local ActorRef
remote.tell(Increment(1))?;
let count = remote.ask(GetCount, None)?.await?;
```

Message types must be **pre-registered** with the builder so the ref knows
how to serialize each type at send time.

### Transport Trait

The `Transport` trait defines how nodes send and receive `WireEnvelope`s.
It is protocol-agnostic — implementations can be backed by gRPC, TCP, QUIC,
or any other protocol.

```rust,ignore
use dactor::transport::Transport;

#[async_trait]
impl Transport for MyTransport {
    async fn send(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<(), TransportError> {
        // Send the envelope over the network
        todo!()
    }

    async fn send_request(
        &self,
        target_node: &NodeId,
        envelope: WireEnvelope,
    ) -> Result<WireEnvelope, TransportError> {
        // Send and wait for a reply envelope
        todo!()
    }

    async fn is_reachable(&self, node: &NodeId) -> bool {
        // Check if a node is reachable
        todo!()
    }

    async fn connect(&self, node: &NodeId, address: Option<&str>) -> Result<(), TransportError> {
        // Establish a connection to a remote node at the given address
        todo!()
    }

    async fn disconnect(&self, node: &NodeId) -> Result<(), TransportError> {
        // Disconnect from a remote node
        todo!()
    }
}
```

For testing, `InMemoryTransport` provides a fully functional in-process
transport without real networking.

### Protobuf System Messages

System messages (spawn, watch, cancel, peer management) use a **fixed
protobuf format** via `prost` for stability and interoperability. Application
messages remain pluggable — use whatever serialization format you prefer.

The `SystemMessageRouter` routes incoming `WireEnvelope` system messages to
the correct system actor mailbox based on the `message_type` field.

---

## 10. Cluster Management

### Cluster Events

Subscribe to cluster membership changes via the `ClusterEvents` trait:

```rust,ignore
use dactor::cluster::{ClusterEvent, ClusterEvents};

let sub_id = cluster.subscribe(Box::new(|event| {
    match event {
        ClusterEvent::NodeJoined(node_id) => {
            println!("Node {node_id} joined the cluster");
        }
        ClusterEvent::NodeLeft(node_id) => {
            println!("Node {node_id} left the cluster");
        }
    }
}))?;

// Later: unsubscribe
cluster.unsubscribe(sub_id)?;
```

### Cluster Discovery

`ClusterDiscovery` defines how nodes find each other. It's an async trait
returning `Result<Vec<String>, DiscoveryError>`:

```rust,ignore
use dactor::remote::{ClusterDiscovery, StaticSeeds};

let seeds = StaticSeeds::new(vec![
    "node-1.example.com:9001".to_string(),
    "node-2.example.com:9001".to_string(),
]);

// Async discovery — returns Result
let peers = seeds.discover().await?;
```

For cloud environments, use platform-specific crates:
- `dactor-discover-k8s` — Kubernetes (AKS, EKS, GKE)
- `dactor-discover-aws` — AWS Auto Scaling / EC2 tags

### System Actors

dactor automatically manages four system actors per node for distributed
operations:

| System Actor | Responsibility |
|--------------|----------------|
| `SpawnManager` | Handles remote actor spawn requests |
| `WatchManager` | Manages remote watch/unwatch subscriptions |
| `CancelManager` | Handles remote cancellation requests |
| `NodeDirectory` | Maps `NodeId` → connection metadata, tracks peer status |

System actors communicate using well-known wire protocol message type
constants (e.g., `SYSTEM_MSG_TYPE_SPAWN`, `SYSTEM_MSG_TYPE_WATCH`). These
constants are **frozen wire protocol values** — they never change, ensuring
backward compatibility between nodes running different versions.

---

## 11. Observability

### Metrics

Enable per-actor metrics collection with `MetricsInterceptor`:

```rust,ignore
let mut runtime = TestRuntime::new();
runtime.enable_metrics();

let counter = runtime.spawn::<Counter>("counter", ()).await.unwrap();

// Send some messages...
counter.tell(Increment(1)).unwrap();
let _ = counter.ask(GetCount, None).unwrap().await.unwrap();

// Query the metrics registry
let registry = runtime.metrics().unwrap();
println!("Total messages: {}", registry.total_messages());
println!("Total errors: {}", registry.total_errors());
println!("Actor count: {}", registry.actor_count());

// Per-actor breakdowns
for (actor_id, snapshot) in registry.all() {
    println!("Actor {:?}:", actor_id);
    println!("  message_count: {}", snapshot.message_count);
    println!("  error_count: {}", snapshot.error_count);
    println!("  message_rate: {:.2}/s", snapshot.message_rate);
    if let Some(avg) = snapshot.avg_latency {
        println!("  avg latency: {:?}", avg);
    }
}

// Runtime-level windowed metrics
let runtime_metrics = registry.runtime_metrics();
println!("Message rate: {:.1}/s", runtime_metrics.message_rate);
println!("Error rate: {:.1}/s", runtime_metrics.error_rate);
```

### Dead Letter Handling

Undeliverable messages can be routed to a `DeadLetterHandler`:

```rust,ignore
use dactor::dead_letter::{
    DeadLetterHandler, DeadLetterEvent,
    LoggingDeadLetterHandler, CollectingDeadLetterHandler,
};

// Built-in: log dead letters
let handler = LoggingDeadLetterHandler;

// Built-in: collect dead letters for inspection
let handler = CollectingDeadLetterHandler::new();
// Later: handler.events() to inspect collected dead letters
```

### Circuit Breaker

The `CircuitBreakerInterceptor` provides fault isolation with three states:

```text
Closed → (failures exceed threshold) → Open → (timeout expires) → HalfOpen
                                                                       ↓
                                                          success → Closed
                                                          failure → Open
```

```rust,ignore
use dactor::circuit_breaker::{CircuitBreakerInterceptor, CircuitState};

// Transitions to Open after 5 errors within 60 seconds,
// tries HalfOpen after 30 seconds
let breaker = CircuitBreakerInterceptor::new(
    5,                          // trip after 5 errors
    Duration::from_secs(60),    // within a 60-second window
    Duration::from_secs(30),    // stay open for 30 seconds
);
```

### Rate Limiting

`ActorRateLimiter` is an outbound interceptor that throttles message sending
using a tumbling-window algorithm:

```rust,ignore
use dactor::throttle::ActorRateLimiter;

// Allow at most 100 messages per second
let limiter = ActorRateLimiter::new(100, Duration::from_secs(1));
runtime.add_outbound_interceptor(Box::new(limiter));
```

---

## 12. Mailbox Configuration

### Bounded Mailboxes

By default, actors have unbounded mailboxes. For backpressure, configure a
bounded mailbox:

```rust,ignore
use dactor::mailbox::{MailboxConfig, OverflowStrategy};
use dactor::SpawnOptions;

let actor = runtime.spawn_with_options::<MyActor>(
    "bounded-actor",
    args,
    SpawnOptions {
        interceptors: vec![],
        mailbox: MailboxConfig::Bounded {
            capacity: 100,
            overflow: OverflowStrategy::RejectWithError,
        },
    },
).await.unwrap();
```

### Overflow Strategies

| Strategy | Behavior |
|----------|----------|
| `Block` | Block the sender until space is available |
| `RejectWithError` | Return `Err(ActorSendError)` immediately |
| `DropNewest` | Silently drop the new message |

### Priority Mailboxes

Messages can carry a `Priority` header. With a `StrictPriorityComparer`,
higher-priority messages are dequeued first:

```rust,ignore
use dactor::message::Priority;

// In an outbound interceptor or manually:
headers.insert(Priority::HIGH);
headers.insert(Priority(42)); // custom priority level
```

---

## 13. Cancellation

### Cancellation Tokens

All request-reply and streaming patterns accept an optional
`CancellationToken` for cooperative cancellation:

```rust,ignore
use dactor::CancellationToken;

let token = CancellationToken::new();
let reply = actor.ask(Query, Some(token.clone())).unwrap();

// Cancel from another task
token.cancel();

// The ask will resolve with an error
```

### Cooperative Cancellation in Handlers

Handlers can check for cancellation using `ctx.cancelled()` in a
`tokio::select!`:

```rust,ignore
#[async_trait]
impl Handler<LongRunningTask> for MyActor {
    async fn handle(
        &mut self,
        msg: LongRunningTask,
        ctx: &mut ActorContext,
    ) -> String {
        tokio::select! {
            result = do_expensive_work(&msg) => result,
            _ = ctx.cancelled() => {
                "cancelled".to_string()
            }
        }
    }
}
```

If no cancellation token is set, `ctx.cancelled()` returns a permanently
pending future (the cancellation branch never triggers).

### Timed Cancellation

Create a token that automatically cancels after a duration:

```rust,ignore
use dactor::actor::cancel_after;

let token = cancel_after(Duration::from_secs(5));
let reply = actor.ask(SlowQuery, Some(token)).unwrap().await;
```

---

## 14. Testing

dactor provides a comprehensive, multi-tier testing strategy.

### TestRuntime for Unit Tests

`TestRuntime` is an in-memory actor runtime included in the core crate
(behind the `test-support` feature). It uses channel-based mailboxes and
requires no external runtime dependencies.

```rust,ignore
use dactor::test_support::test_runtime::TestRuntime;

#[tokio::test]
async fn test_counter() {
    let runtime = TestRuntime::new();
    let counter = runtime
        .spawn::<Counter>("counter", Counter { count: 0 })
        .await
        .unwrap();

    counter.tell(Increment(5)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let count = counter.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 5);
}
```

`TestRuntime` also provides:
- **`TestClock`** — deterministic clock with manual `advance()` for
  timer-dependent tests
- **`SpawnOptions`** — configure interceptors and mailbox per actor
- **`enable_metrics()`** — enable metrics collection for testing
- **`watch()`** — register DeathWatch between test actors

### Conformance Suite

dactor includes a **conformance suite** of 25+ standardized tests that
verify runtime correctness. Every adapter (ractor, kameo, coerce) runs the
same suite, ensuring behavioral consistency:

Tests cover:
- Tell/ask message delivery
- Lifecycle hooks (on_start, on_stop, on_error)
- Streaming (expand, reduce, transform)
- Transparent batching
- Cancellation
- Concurrent asks
- Message ordering
- Slow consumers
- Multiple handlers per actor

### MockCluster for Integration Tests

`MockCluster` (in the `dactor-mock` crate) provides multi-node simulation
with fault injection for integration testing:

```rust,ignore
use dactor_mock::MockCluster;

let mut cluster = MockCluster::new(&["node-1", "node-2", "node-3"]);

// Access a specific node's runtime to spawn actors
let node = cluster.node("node-1");
let actor = node.runtime.spawn::<MyActor>("counter", args, deps).await;

// Simulate node crash (removes node and updates peers)
cluster.crash_node("node-2");

// Restart a crashed node (fresh state, reconnects to peers)
cluster.restart_node("node-2");

// Use the MockNetwork for partition simulation
let network = cluster.network();
network.partition(
    &NodeId("node-1".into()),
    &NodeId("node-2".into()),
);
network.remove_partition(
    &NodeId("node-1".into()),
    &NodeId("node-2".into()),
);
```

### gRPC Test Harness for E2E Tests

The `dactor-test-harness` crate provides a gRPC-based test harness for
running end-to-end tests against real adapter binaries. The harness runs
**60 multi-process integration tests** across all 3 adapters, covering:

- Spawn/tell/ask
- Stop notifications
- Network partition and heal
- Error handling
- Watch termination
- Node crash detection
- Concurrent operations
- Large payloads
- Multi-actor interaction
- Rapid lifecycle
- Slow handler isolation
- Cancellation and timeout
- Inter-actor forwarding
- State snapshots

Run E2E tests:

```bash
# Build the test-node binary for an adapter
cargo build -p dactor-ractor --features test-harness --bin test-node-ractor

# Run the E2E test suite
cargo test -p dactor-ractor --test e2e_tests --features test-harness
```

---

## 15. Choosing an Adapter

All three adapters implement the full dactor v0.2 API. Choose based on
your project's requirements.

### Ractor

**Best for:** Applications that need ractor's Erlang-style supervision trees
and process linking semantics.

```toml
[dependencies]
dactor = "0.2"
dactor-ractor = "0.2"
```

### Kameo

**Best for:** Applications that prefer kameo's lightweight, tokio-native
approach with built-in request coalescing and bounded mailboxes.

```toml
[dependencies]
dactor = "0.2"
dactor-kameo = "0.2"
```

### Coerce

**Best for:** Applications that need Coerce's built-in persistence
(event sourcing), remoting, and cluster sharding capabilities.

```toml
[dependencies]
dactor = "0.2"
dactor-coerce = "0.2"
```

### Switching Adapters

Switching from one adapter to another is a **2-line change** in your
`Cargo.toml`. Your actor code, messages, and handlers remain identical.

```diff
 [dependencies]
 dactor = "0.2"
-dactor-ractor = "0.2"
+dactor-kameo = "0.2"
```

Then update your runtime initialization:

```diff
-use dactor_ractor::RactorRuntime;
-let runtime = RactorRuntime::new();
+use dactor_kameo::KameoRuntime;
+let runtime = KameoRuntime::new();
```

Everything else — actors, messages, handlers, interceptors, pools — stays
the same.

---

## 16. Architecture

### Crate Layout

```
dactor/                  Workspace root
├── dactor/              Core library — traits, types, test support
│   ├── src/
│   │   ├── actor.rs         Actor, Handler, ExpandHandler, ReduceHandler, ActorRef
│   │   ├── message.rs       Message, Headers, Priority
│   │   ├── interceptor.rs   InboundInterceptor, OutboundInterceptor, Disposition
│   │   ├── mailbox.rs       MailboxConfig, OverflowStrategy
│   │   ├── supervision.rs   ChildTerminated, OneForOne, AllForOne, RestForOne
│   │   ├── persistence.rs   PersistentActor, EventSourced, DurableState, storage traits
│   │   ├── pool.rs          PoolRef, PoolRouting, PoolConfig, Keyed
│   │   ├── stream.rs        BoxStream, StreamSender, StreamReceiver, BatchConfig
│   │   ├── remote.rs        WireEnvelope, MessageSerializer, ClusterDiscovery
│   │   ├── remote_ref.rs    RemoteActorRef, RemoteActorRefBuilder
│   │   ├── transport.rs     Transport trait, InMemoryTransport
│   │   ├── system_actors.rs SpawnManager, WatchManager, CancelManager, NodeDirectory
│   │   └── test_support/    TestRuntime, TestClock, conformance suite
│   ├── examples/            16 runnable examples
│   └── tests/               Core integration tests
├── dactor-ractor/       Ractor adapter (full v0.2 API)
├── dactor-kameo/        Kameo adapter (full v0.2 API)
├── dactor-coerce/       Coerce adapter (full v0.2 API)
├── dactor-mock/         Mock cluster for testing
├── dactor-test-harness/ gRPC integration test harness
└── docs/                Design documents and guides
```

### Trait Hierarchy

```text
Actor                    (lifecycle: create, on_start, on_stop, on_error)
├── Handler<M>           (per-message handling)
├── ExpandHandler<M,O>   (server-streaming)
├── ReduceHandler<I,R>   (client-streaming)
├── TransformHandler<I,O>(bidirectional streaming)
├── PersistentActor      (persistence identity and recovery)
│   ├── EventSourced     (event journal + snapshots)
│   └── DurableState     (full state save/load)
└── SupervisionStrategy  (child failure response)

ActorRef<A>              (typed handle to a running actor)
├── tell, ask, expand, reduce, transform, stop
├── PoolRef<A,R>         (routed pool of workers)
├── VirtualPoolRef<A,R>  (single-threaded routed pool)
├── RemoteActorRef<A>    (serializing remote proxy)
├── WorkerRef<A,L>       (local or remote worker)
└── BroadcastRef<A,R>    (fan-out to all members)

InboundInterceptor       (on_receive, on_complete, on_expand_item)
OutboundInterceptor      (on_send, on_reply)
DropObserver             (on_drop notification)

Transport                (send, send_with_reply for WireEnvelope)
MessageSerializer        (serialize/deserialize for wire transport)
ClusterEvents            (subscribe/unsubscribe to NodeJoined/NodeLeft)
```

---

## 17. Examples

The [`dactor/examples/`](../dactor/examples/) directory contains 16 runnable
examples. Each can be run with:

```bash
cargo run --example <name> -p dactor --features test-support
```

| Example | Description |
|---------|-------------|
| `readme_quickstart` | Quick start — counter with tell + ask |
| `basic_counter` | Tell + ask patterns |
| `streaming` | Server-streaming (expand) and client-streaming (reduce) |
| `batch_streaming` | Transparent batching with `BatchConfig` |
| `cancellation` | Cancellation tokens and `cancel_after()` |
| `supervision` | Supervision strategies (OneForOne, AllForOne, RestForOne) |
| `interceptors` | Inbound/outbound interceptor pipelines |
| `dead_letters` | DropObserver for monitoring message drops |
| `persistence` | Event sourcing with journal + snapshots |
| `bounded_mailbox` | Bounded mailbox with overflow strategies |
| `metrics` | MetricsInterceptor and MetricsStore |
| `rate_limiting` | ActorRateLimiter outbound throttling |
| `error_handling` | ActorError with ErrorCode and error chains |
| `event_sourcing` | Event sourcing with CQRS patterns |
| `actor_pool` | Actor pools with routing strategies |
| `showcase` | Comprehensive feature showcase |

---

## License

MIT — see [LICENSE](../LICENSE) for details.
