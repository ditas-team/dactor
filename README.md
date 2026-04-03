# dactor

An abstract framework for distributed actors in Rust.

dactor provides a **provider-agnostic** actor API that works across multiple
actor runtimes ([ractor](https://crates.io/crates/ractor),
[kameo](https://crates.io/crates/kameo), with a
[coerce](https://crates.io/crates/coerce) adapter in progress).
Write your actor logic once, swap the runtime underneath.

## Key Features

- **4 communication patterns** — `tell` (fire-and-forget), `ask`
  (request-reply), `stream` (server-streaming), `feed` (client-streaming)
- **Transparent batching** — `BatchConfig` on `stream()` / `feed()` groups
  items into batches (max_items + max_delay) to reduce per-item overhead
- **Actor pools** — `PoolRef` with `RoundRobin`, `Random`, and `KeyBased`
  routing strategies for distributing work across workers
- **Interceptor pipelines** — inbound and outbound hooks for logging, auth,
  header stamping, rate limiting, and more; per-item `on_stream_item`
  interception returning `Disposition`; `on_reply` for outbound ask replies
- **DropObserver** — global observer for interceptor-driven message drops
  (metrics, alerting, dead-letter routing)
- **Lifecycle management** — `on_start`, `on_stop`,
  `on_error` → `ErrorAction` (Resume / Restart / Stop / Escalate)
- **Supervision strategies** — `OneForOne`, `AllForOne`, `RestForOne` with
  configurable restart limits and time windows
- **DeathWatch** — `ChildTerminated` notifications for watched actors
- **Timers** — `send_after()` and `send_interval()` with cancellation via
  `CancellationToken`
- **Bounded & unbounded mailboxes** — configurable `OverflowStrategy`
  (Block, RejectWithError, DropNewest)
- **Cooperative cancellation** — `CancellationToken` on ask / stream / feed,
  `ctx.cancelled()` for select!-based cancellation
- **Persistence** — `PersistentActor`, `EventSourced`, `DurableState` traits
  with recovery pipeline (`recover_event_sourced`, `recover_durable_state`),
  `JournalStorage`, `SnapshotStorage`, `StateStorage` with in-memory default
- **Observability** — `MetricsInterceptor` tracking message counts, latency
  percentiles (p99, avg, max) per actor
- **Dead letter handling** — `DeadLetterHandler` trait with logging and
  collecting implementations
- **Rate limiting** — `ActorRateLimiter` outbound interceptor with
  tumbling-window throttle
- **Mock cluster for testing** — `MockCluster` with multi-node simulation
  and fault injection

## Quick Start

Add the core crate to your `Cargo.toml`:

```toml
[dependencies]
dactor = { version = "0.2", features = ["test-support"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

Define an actor, its messages, and handlers:

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
    let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

    counter.tell(Increment(5)).unwrap();
    counter.tell(Increment(3)).unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let count = counter.ask(GetCount, None).unwrap().await.unwrap();
    println!("Count: {count}"); // Count: 8
}
```

## Communication Patterns

| Pattern | Method | Description |
|---------|--------|-------------|
| **Tell** | `actor.tell(msg)` | Fire-and-forget — no reply, returns immediately |
| **Ask** | `actor.ask(msg, cancel)` | Request-reply — returns `AskReply<T>` future |
| **Stream** | `actor.stream(msg, buf, batch, cancel)` | Server-streaming — handler sends multiple items via `StreamSender`. Pass `Some(BatchConfig)` or `None`. |
| **Feed** | `actor.feed::<Item, Reply>(input, buf, batch, cancel)` | Client-streaming — sends a `BoxStream` of items, gets one reply. Pass `Some(BatchConfig)` or `None`. |

## Architecture

```
┌──────────────────────────────────────────────────┐
│                Application Code                  │
├──────────────────────────────────────────────────┤
│           dactor (core traits + types)           │
│  Actor · Handler · ActorRef · Message            │
│  Interceptors · Lifecycle · Mailbox              │
│  Persistence · Metrics · DeathWatch              │
├──────────┬──────────┬──────────┬─────────────────┤
│  dactor  │  dactor  │  dactor  │  dactor-mock    │
│  -ractor │  -kameo  │  -coerce │  (test cluster) │
└──────────┴──────────┴──────────┴─────────────────┘
```

### Core Traits

| Trait | Purpose |
|-------|---------|
| `Actor` | Core trait with `create()`, `on_start()`, `on_stop()`, `on_error()` |
| `Handler<M>` | Per-message handler — `async fn handle(&mut self, msg, ctx) -> M::Reply` |
| `StreamHandler<M>` | Server-streaming handler — sends items via `StreamSender` |
| `FeedHandler<Item, Reply>` | Client-streaming handler — receives `StreamReceiver<Item>`, returns `Reply` |
| `PersistentActor` | Base persistence trait with `persistence_id()` and recovery hooks |
| `EventSourced` | Event-sourcing — `apply()`, `persist()`, `snapshot()`, `restore_snapshot()` |
| `DurableState` | Durable-state — `save_state()`, `restore_state()` |
| `SupervisionStrategy` | Determines supervisor response to child failure — `on_child_failed()` |
| `ActorRef<A>` | Typed handle — `tell`, `ask`, `stream`, `feed`, `stop`, `is_alive` |
| `Message` | Message trait with associated `Reply` type |
| `InboundInterceptor` | Runs on actor task before handler (logging, auth, metrics, `on_stream_item`) |
| `OutboundInterceptor` | Runs on caller task before send (rate limiting, tracing, `on_reply`) |
| `DropObserver` | Global observer notified when interceptors drop messages |

## Adapter Crates

| Crate | Provider | Status |
|-------|----------|--------|
| [`dactor-ractor`](dactor-ractor/) | [ractor](https://crates.io/crates/ractor) | ✅ Full v0.2 |
| [`dactor-kameo`](dactor-kameo/) | [kameo](https://crates.io/crates/kameo) | ✅ Full v0.2 |
| [`dactor-coerce`](dactor-coerce/) | [coerce](https://crates.io/crates/coerce) | 🔲 Stub |
| [`dactor-mock`](dactor-mock/) | Mock cluster | ✅ Testing |
| [`dactor-test-harness`](dactor-test-harness/) | gRPC harness | ✅ E2E testing |

Use an adapter to run your actors on a real runtime:

```toml
[dependencies]
dactor = { version = "0.2", features = ["test-support"] }
dactor-ractor = "0.2"   # or dactor-kameo = "0.2"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

All adapters expose the same `ActorRef<A>` semantics. Choose based on your
preferred runtime.

## Examples

The [`dactor/examples/`](dactor/examples/) directory contains runnable
examples:

| Example | Description | Command |
|---------|-------------|---------|
| [`readme_quickstart`](dactor/examples/readme_quickstart.rs) | Quick start — basic counter with tell + ask | `cargo run --example readme_quickstart -p dactor --features test-support` |
| [`basic_counter`](dactor/examples/basic_counter.rs) | Tell + ask patterns with a counter actor | `cargo run --example basic_counter -p dactor --features test-support` |
| [`streaming`](dactor/examples/streaming.rs) | Server-streaming and client-streaming (feed) | `cargo run --example streaming -p dactor --features test-support` |
| [`batch_streaming`](dactor/examples/batch_streaming.rs) | Transparent batching for stream/feed with `BatchConfig` | `cargo run --example batch_streaming -p dactor --features test-support` |
| [`cancellation`](dactor/examples/cancellation.rs) | Cancellation tokens and `cancel_after()` for streams | `cargo run --example cancellation -p dactor --features test-support` |
| [`supervision`](dactor/examples/supervision.rs) | Supervision strategies (OneForOne, AllForOne, RestForOne) | `cargo run --example supervision -p dactor --features test-support` |
| [`interceptors`](dactor/examples/interceptors.rs) | Inbound/outbound interceptor pipelines | `cargo run --example interceptors -p dactor --features test-support` |
| [`dead_letters`](dactor/examples/dead_letters.rs) | DropObserver for monitoring interceptor-driven drops | `cargo run --example dead_letters -p dactor --features test-support` |
| [`persistence`](dactor/examples/persistence.rs) | Event sourcing with journal + snapshots | `cargo run --example persistence -p dactor --features test-support` |
| [`bounded_mailbox`](dactor/examples/bounded_mailbox.rs) | Bounded mailbox with overflow strategies | `cargo run --example bounded_mailbox -p dactor --features test-support` |
| [`metrics`](dactor/examples/metrics.rs) | MetricsInterceptor and MetricsStore for observability | `cargo run --example metrics -p dactor --features test-support` |
| [`rate_limiting`](dactor/examples/rate_limiting.rs) | ActorRateLimiter outbound throttling | `cargo run --example rate_limiting -p dactor --features test-support` |
| [`error_handling`](dactor/examples/error_handling.rs) | ActorError with ErrorCode and error chains | `cargo run --example error_handling -p dactor --features test-support` |

## Project Structure

```
dactor/                  Workspace root
├── dactor/              Core library — traits, types, test support
│   ├── src/
│   │   ├── actor.rs         Actor, Handler, StreamHandler, FeedHandler, ActorRef
│   │   ├── message.rs       Message, Headers, Priority
│   │   ├── interceptor.rs   InboundInterceptor, OutboundInterceptor, Disposition,
│   │   │                    DropObserver, on_stream_item, on_reply
│   │   ├── mailbox.rs       MailboxConfig, OverflowStrategy
│   │   ├── supervision.rs   ChildTerminated, OneForOne, AllForOne, RestForOne
│   │   ├── persistence.rs   PersistentActor, EventSourced, DurableState,
│   │   │                    JournalStorage, SnapshotStorage, StateStorage
│   │   ├── pool.rs          PoolRef, PoolRouting, PoolConfig, Keyed
│   │   ├── timer.rs         send_after, send_interval, TimerHandle
│   │   ├── dead_letter.rs   DeadLetterHandler, DeadLetterEvent
│   │   ├── metrics.rs       MetricsInterceptor, MetricsStore
│   │   ├── throttle.rs      ActorRateLimiter
│   │   ├── errors.rs        ErrorCode, ErrorAction, ActorError
│   │   ├── stream.rs        BoxStream, StreamSender, StreamReceiver, BatchConfig
│   │   ├── remote.rs        WireEnvelope, MessageSerializer, ClusterDiscovery
│   │   ├── dispatch.rs      Type-erased message dispatch
│   │   └── test_support/    TestRuntime, TestClock, conformance suite
│   ├── examples/            13 runnable examples
│   └── tests/               Core integration tests
├── dactor-ractor/       Ractor adapter (full v0.2 API)
├── dactor-kameo/        Kameo adapter (full v0.2 API)
├── dactor-coerce/       Coerce adapter (stub)
├── dactor-mock/         Mock cluster for testing (multi-node, fault injection)
├── dactor-test-harness/ gRPC integration test harness
└── docs/                Design docs, adapter plan, progress tracking
```

## Testing

Run the full workspace test suite:

```bash
cargo test --workspace
```

The core crate includes `test_support` with mock implementations:

- **`TestRuntime`** — in-memory actor runtime with channel-based mailboxes
- **`TestClock`** — deterministic clock with manual `advance()`
- **Conformance suite** — 6 standardized tests verifying runtime correctness

## Documentation

- [Design document (v0.2)](docs/design-v0.2.md) — full API design and rationale
- [Adapter plan](docs/adapter-plan.md) — implementation plan for each adapter
- [Progress tracker](docs/progress.md) — PR-level status for all milestones

## License

MIT — see [LICENSE](LICENSE) for details.
