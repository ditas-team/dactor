# dactor

An abstract framework for distributed actors in Rust.

`dactor` provides framework-agnostic traits for building actor-based systems.
It defines the core abstractions for actor spawning, message delivery, timer
scheduling, processing groups, and cluster membership events — without coupling
to any specific actor framework.

## Workspace Crates

| Crate | Description |
|-------|-------------|
| [`dactor`](dactor/) | Core library — traits, types, test support |
| [`dactor-ractor`](dactor-ractor/) | Adapter for the [ractor](https://crates.io/crates/ractor) actor framework |
| [`dactor-kameo`](dactor-kameo/) | Adapter for the [kameo](https://crates.io/crates/kameo) actor framework |

## Quick Start

Add the core crate and an adapter to your `Cargo.toml`:

```toml
[dependencies]
dactor = "0.1"
dactor-ractor = "0.1"   # or dactor-kameo = "0.1"
```

### Use an Actor Runtime

```rust
use dactor::{Actor, ActorRef, Handler, TestRuntime};
use dactor::message::Message;

// Define your actor, messages, and handlers, then:
#[tokio::main]
async fn main() {
    let runtime = TestRuntime::new();
    let actor = runtime.spawn::<Counter>("counter", Counter { count: 0 });

    // Fire-and-forget
    actor.tell(Increment(1)).unwrap();

    // Request-reply
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
}
```

## Architecture

```
┌─────────────────────────────────────────────┐
│              Application Code               │
├─────────────────────────────────────────────┤
│        dactor (core traits + types)         │
│  ┌─────────┐  ┌──────────┐  ┌───────────┐  │
│  │ Actor   │  │  Timer   │  │  Cluster  │  │
│  │ Traits  │  │ Handling │  │  Events   │  │
│  └─────────┘  └──────────┘  └───────────┘  │
├──────────────────┬──────────────────────────┤
│  dactor-ractor   │     dactor-kameo         │
│  (ractor adapter)│     (kameo adapter)      │
└──────────────────┴──────────────────────────┘
```

### Core Traits

- **ActorRef\<A\>** — Typed handle to a running actor (tell, ask, stream, feed)
- **Actor** — Core actor trait with lifecycle hooks
- **Handler\<M\>** — Per-message-type handler trait
- **ClusterEvents** — Subscribe to node join/leave notifications
- **TimerHandle** — Cancellable scheduled timer
- **Clock** — Time abstraction for deterministic testing

### Choosing an Adapter

| Feature | `dactor-ractor` | `dactor-kameo` |
|---------|----------------|----------------|
| Framework | [ractor](https://crates.io/crates/ractor) | [kameo](https://crates.io/crates/kameo) |
| Mailbox | Unbounded | Bounded (default) |
| Spawn | Async (bridge thread) | Sync (cheaper) |
| Send | `cast()` fire-and-forget | `tell().try_send()` fire-and-forget |
| Timers | tokio tasks | tokio tasks |

Both adapters expose identical `ActorRef` semantics. Choose based on your
preferred actor framework.

## Testing

The core crate includes `test_support` with mock implementations:

- `TestRuntime` — In-memory actor runtime with channel-based mailboxes
- `TestClock` — Deterministic clock with manual `advance()`
- `TestClusterEvents` — Manually triggered cluster events

```rust
use dactor::test_support::{test_runtime::TestRuntime, test_clock::TestClock};
use dactor::Clock;
```

Run all tests:

```bash
cargo test --workspace
```

## License

MIT
