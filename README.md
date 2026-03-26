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
use dactor::{ActorRuntime, ActorRef};
use dactor_ractor::RactorRuntime;  // or dactor_kameo::KameoRuntime

#[tokio::main]
async fn main() {
    let runtime = RactorRuntime::new();

    // Spawn an actor that handles u64 messages
    let actor = runtime.spawn("counter", |msg: u64| {
        println!("Received: {msg}");
    });

    // Fire-and-forget send
    actor.send(42).unwrap();
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

- **ActorRef\<M\>** — Handle to a running actor for fire-and-forget messaging
- **ActorRuntime** — Actor spawning, timer scheduling, processing groups
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

Both adapters expose identical `ActorRuntime` semantics. Choose based on your
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
