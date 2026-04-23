# dactor

**Write your actor logic once. Swap the runtime underneath.**

dactor is a provider-agnostic actor framework for Rust. You write actors against
dactor's API and choose a runtime adapter —
[ractor](https://crates.io/crates/ractor),
[kameo](https://crates.io/crates/kameo), or
[coerce](https://crates.io/crates/coerce) — without changing your business
logic.

## Quick Start

Add dactor to your project:

```toml
[dependencies]
dactor = { version = "0.2", features = ["test-support"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

Build a counter actor in four steps — **define**, **message**, **handle**, **run**:

```rust
use dactor::prelude::*;
use dactor::message::Message;
use dactor::test_support::test_runtime::TestRuntime;

// 1. Define your actor
struct Counter { count: u64 }

impl Actor for Counter {
    type Args = Self;
    type Deps = ();
    fn create(args: Self, _deps: ()) -> Self { args }
}

// 2. Define messages
struct Increment(u64);
impl Message for Increment { type Reply = (); }

struct GetCount;
impl Message for GetCount { type Reply = u64; }

// 3. Implement handlers
#[async_trait::async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

#[async_trait::async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

// 4. Spawn and interact
#[tokio::main]
async fn main() {
    let runtime = TestRuntime::new();
    let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 })
        .await.unwrap();

    counter.tell(Increment(5)).unwrap();   // fire-and-forget
    counter.tell(Increment(3)).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let count = counter.ask(GetCount, None).unwrap().await.unwrap();
    println!("Count: {count}"); // → 8
}
```

Run this example: `cargo run --example readme_quickstart -p dactor --features test-support`

## Using a Runtime Adapter

The quick start above uses `TestRuntime` (great for unit tests). For
production, pick a runtime adapter:

```toml
[dependencies]
dactor = "0.2"
dactor-ractor = "0.2"   # or dactor-kameo / dactor-coerce
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

Your actor code stays **exactly the same** — only the runtime setup changes.
All adapters expose the same `ActorRef<A>` interface.

| Adapter | Runtime | Best for |
|---------|---------|----------|
| [`dactor-ractor`](dactor-ractor/) | [ractor](https://crates.io/crates/ractor) | Erlang-style supervision, mature ecosystem |
| [`dactor-kameo`](dactor-kameo/) | [kameo](https://crates.io/crates/kameo) | Lightweight, Tokio-native |
| [`dactor-coerce`](dactor-coerce/) | [coerce](https://crates.io/crates/coerce) | Built-in clustering support |

## What You Can Do

dactor supports five communication patterns:

| Pattern | Code | When to use |
|---------|------|-------------|
| **Tell** | `actor.tell(msg)` | Fire-and-forget commands |
| **Ask** | `actor.ask(msg, cancel)` | Request-reply queries |
| **Expand** | `actor.expand(msg, ...)` | One request → stream of responses |
| **Reduce** | `actor.reduce(stream, ...)` | Stream of inputs → one response |
| **Transform** | `actor.transform(stream, ...)` | Stream in → stream out |

Plus **broadcast** messaging, **actor pools**, **interceptors**, **supervision**,
**persistence**, **bounded mailboxes**, **metrics**, and **remote actor** support.

## Learn More

📖 **[Usage Guide](docs/usage-guide.md)** — step-by-step tutorial covering all
features, from your first actor to distributed clusters.

🏃 **[Examples](dactor/examples/)** — 17 runnable examples:

| Example | What it teaches | Run it |
|---------|-----------------|--------|
| [`basic_counter`](dactor/examples/basic_counter.rs) | Tell + ask basics | `cargo run --example basic_counter -p dactor --features test-support` |
| [`streaming`](dactor/examples/streaming.rs) | Expand + reduce patterns | `cargo run --example streaming -p dactor --features test-support` |
| [`supervision`](dactor/examples/supervision.rs) | Supervision strategies | `cargo run --example supervision -p dactor --features test-support` |
| [`interceptors`](dactor/examples/interceptors.rs) | Logging, auth, rate limiting | `cargo run --example interceptors -p dactor --features test-support` |
| [`actor_pool`](dactor/examples/actor_pool.rs) | Pool routing strategies | `cargo run --example actor_pool -p dactor --features test-support` |
| [`persistence`](dactor/examples/persistence.rs) | Event sourcing + snapshots | `cargo run --example persistence -p dactor --features test-support` |
| [`bounded_mailbox`](dactor/examples/bounded_mailbox.rs) | Backpressure with overflow | `cargo run --example bounded_mailbox -p dactor --features test-support` |
| [`task_queue`](dactor/examples/task_queue.rs) | Worker pool + retries | `cargo run --example task_queue -p dactor --features test-support,metrics` |
| [`showcase`](dactor/examples/showcase.rs) | All features combined | `cargo run --example showcase -p dactor --features test-support` |

<details>
<summary>All 17 examples</summary>

| Example | Description |
|---------|-------------|
| `readme_quickstart` | Minimal counter (this README) |
| `basic_counter` | Tell + ask patterns |
| `streaming` | Server + client streaming |
| `batch_streaming` | Transparent batching |
| `cancellation` | Cancellation tokens |
| `supervision` | OneForOne, AllForOne, RestForOne |
| `interceptors` | Inbound/outbound pipelines |
| `dead_letters` | DropObserver monitoring |
| `persistence` | Journal + snapshots |
| `bounded_mailbox` | Overflow strategies |
| `metrics` | MetricsInterceptor |
| `rate_limiting` | Outbound throttling |
| `error_handling` | ErrorCode + error chains |
| `task_queue` | Distributed task queue |
| `event_sourcing` | CQRS patterns |
| `actor_pool` | Pool routing |
| `showcase` | Comprehensive demo |

</details>

## Building & Testing

**Prerequisite:** `protoc` compiler — `brew install protobuf` (macOS),
`apt install protobuf-compiler` (Linux), or `choco install protoc` (Windows).

```bash
# Run all tests
cargo test --workspace --exclude dactor-test-harness --features test-support

# Run E2E tests (requires building test-node first)
cargo build -p dactor-ractor --features test-harness --bin test-node-ractor
cargo test -p dactor-ractor --test e2e_tests --features test-harness
```

## Architecture

```
┌──────────────────────────────────────────────────┐
│              Your Application Code               │
├──────────────────────────────────────────────────┤
│            dactor (core traits + API)            │
├──────────┬──────────┬──────────┬─────────────────┤
│  dactor  │  dactor  │  dactor  │  dactor-mock    │
│  -ractor │  -kameo  │  -coerce │  (testing)      │
└──────────┴──────────┴──────────┴─────────────────┘
```

## Documentation

- 📖 [Usage Guide](docs/usage-guide.md) — comprehensive tutorial
- 📐 [Design Document](docs/design-v0.2.md) — API design and rationale
- 📋 [API Reference](https://docs.rs/dactor) — generated Rust docs

## License

MIT — see [LICENSE](LICENSE) for details.
