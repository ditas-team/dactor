# dactor-kameo — Implementation Details

## Overview

The kameo adapter bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API
with [kameo](https://crates.io/crates/kameo)'s actor framework using
type-erased dispatch envelopes.

Each dactor actor is spawned as a real kameo actor via `Spawn::spawn_with_mailbox`.
Kameo uses a per-message-type `kameo::message::Message<M>` trait, but dactor
wraps all messages into a single `DactorMsg<A>(Box<dyn Dispatch<A>>)` type.

## Architecture

```
┌──────────────────────────────────────────────────────┐
│  KameoRuntime                                        │
│  ┌──────────────┐  ┌──────────────────────────────┐  │
│  │ next_local   │  │ System Actors (optional)      │  │
│  │ (Arc<AtomicU64>)│ SpawnManagerActor             │  │
│  │              │  │ WatchManagerActor             │  │
│  │ stop_receivers│ │ CancelManagerActor            │  │
│  │ (Arc<Mutex>) │  │ NodeDirectoryActor            │  │
│  └──────────────┘  └──────────────────────────────┘  │
└───────────────┬──────────────────────────────────────┘
                │ spawn_internal()
                ▼
┌────────────────────────────────────────────┐
│  KameoDactorActor<A>  (kameo::Actor)       │
│  ┌───────────────────────────────────────┐ │
│  │   actor: A (dactor Actor instance)    │ │
│  │   ctx: ActorContext                   │ │
│  │   interceptors: Vec<InboundInterceptor>│ │
│  │   watchers: WatcherMap (shared)       │ │
│  │   stop_notifier: Option<oneshot::Sender>│ │
│  └───────────────────────────────────────┘ │
│                                            │
│  Message<DactorMsg<A>> → type-erased dispatch│
│                                            │
│  on_start → A::create() + on_start()       │
│  handle   → interceptors → dispatch        │
│  on_stop  → on_stop() + watchers + notify  │
└────────────────────────────────────────────┘
```

## Spawn Mechanism

Kameo's `Spawn::spawn_with_mailbox()` is **synchronous** (internally calls
`tokio::spawn`), so no sync→async bridge is needed:

```
spawn_internal(&self, name, args, deps, interceptors)
    │
    ├─ Generate ActorId (next_local.fetch_add)
    ├─ Create (stop_tx, stop_rx) oneshot channel
    ├─ Build KameoSpawnArgs (includes stop_notifier: Some(stop_tx))
    │
    ├─ KameoDactorActor::<A>::spawn_with_mailbox(args, unbounded())
    │     (synchronous — returns immediately)
    │
    ├─ Store stop_rx in stop_receivers map
    └─ Return KameoActorRef
```

**Spawn is synchronous** — kameo's `spawn_with_mailbox()` internally calls
`tokio::spawn` and returns immediately (no async bridge needed).

## Message Dispatch

Same type-erased envelope as ractor:

```rust
struct DactorMsg<A: Actor>(Box<dyn Dispatch<A>>);
```

Kameo requires implementing `kameo::message::Message<DactorMsg<A>>`:

```rust
impl<A: Actor + 'static> kameo::message::Message<DactorMsg<A>> for KameoDactorActor<A> {
    type Reply = ();
    async fn handle(&mut self, msg: DactorMsg<A>, _ctx: &mut Context<Self, Self::Reply>) {
        // Same dispatch pipeline as ractor
    }
}
```

The `Reply` type is `()` because replies are sent via internal channels
(oneshot for ask, mpsc for stream), not through kameo's reply mechanism.

## Interceptor Pipeline

Identical to the ractor adapter. Both inbound and outbound pipelines use
the same `InboundInterceptor`/`OutboundInterceptor` traits and
`OutboundPipeline` struct from `dactor::runtime_support`.

## Cancellation

Same `tokio::select!` biased pattern as ractor.

## Watch / DeathWatch

Same `WatcherMap` pattern as ractor, but notification delivery uses kameo's
`.tell().try_send()` instead of ractor's `.cast()`.

## Error Handling — Key Difference from Ractor

When a handler panics and `on_error()` returns `Stop` or `Escalate`:

| Adapter | Behavior |
|---------|----------|
| **Ractor** | Calls `myself.stop(None)` to trigger graceful shutdown |
| **Kameo** | **Re-panics** via `std::panic::resume_unwind` to trigger kameo's built-in panic handler |

This difference exists because kameo's default `on_panic` behavior handles
actor supervision — if we catch the panic and stop gracefully, kameo
wouldn't know the actor panicked.

## Lifecycle Handles — Stop Notifier

Kameo doesn't return a JoinHandle from spawn. Instead, the adapter wires
a `tokio::sync::oneshot` channel into the actor's lifecycle:

```
spawn_internal()
    ├─ let (stop_tx, stop_rx) = oneshot::channel()
    ├─ stop_tx passed to KameoDactorActor via KameoSpawnArgs
    ├─ stop_rx stored in stop_receivers map
    │
    │  ... actor runs ...
    │
    ▼  on_stop() called when actor terminates
    ├─ Notifies watchers
    ├─ stop_notifier.take().send(())  ← wakes await_stop()
    └─ Returns Ok(())
```

- `await_stop(id)` — removes and awaits the oneshot receiver
- `await_all()` — drains all receivers, awaits all (collects first error)
- `cleanup_finished()` — removes receivers where `try_recv()` != `Empty`

## Native System Actors

Kameo uses a **per-message-type** `Message<M>` pattern instead of ractor's
single message enum. Each system actor operation is a separate struct:

| Actor | Messages |
|-------|----------|
| `SpawnManagerActor` | `HandleSpawnRequest`, `RegisterFactory`, `GetSpawnedActors` |
| `WatchManagerActor` | `RemoteWatch`, `RemoteUnwatch`, `OnTerminated`, `GetWatchedCount` |
| `CancelManagerActor` | `RegisterCancel`, `CancelById`, `CompleteRequest`, `GetActiveCount` |
| `NodeDirectoryActor` | `ConnectPeer`, `DisconnectPeer`, `IsConnected`, `GetPeerCount`, `GetConnectedCount`, `GetPeerInfo` |

Reply types use `kameo::Reply` derive macro. For types that can't derive
`Reply` (like `CancelResponse`), wrapper newtypes are used:

```rust
#[derive(kameo::Reply)]
pub struct CancelOutcome(pub CancelResponse);

#[derive(kameo::Reply)]
pub enum SpawnOutcome {
    Success { actor_id: ActorId, actor: Box<dyn Any + Send> },
    Failure(SpawnResponse),
}
```

`SpawnOutcome` is used instead of `Result` to prevent kameo from promoting
domain failures (unknown actor type) to handler errors.

## Limitations

| Limitation | Description | Planned Fix |
|-----------|-------------|-------------|
| **Bounded mailbox** | ✅ Front-buffer via bounded `mpsc` channel (PR #106) | — |
| **Restart** | `ErrorAction::Restart` treated as `Resume` | Requires kameo supervision integration |
| **Remote watch auto-wiring** | `notify_terminated()` must be called manually | NA10 (transport routing) |
| **Reply type wrapping** | Some reply types need newtype wrappers for `kameo::Reply` trait | Inherent kameo design |

## ClusterEvent Emission

Same behavior as ractor — `connect_peer()` emits `NodeJoined`,
`disconnect_peer()` emits `NodeLeft`. Subscriber panics are isolated
via `catch_unwind`.
