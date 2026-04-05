# dactor-coerce — Implementation Details

## Overview

The coerce adapter provides `CoerceRuntime`, intended to bridge dactor's
API with the [coerce-rs](https://github.com/LeonHartley/Coerce-rs) actor
framework.

**⚠️ Current Status: STUB**

The adapter currently wraps `TestRuntime` as a placeholder. The real
`coerce-rt` integration has not been done due to potential dependency or
maintenance issues with the coerce-rs crate. The stub locks down the
public API surface and passes conformance tests, so the interface will
remain stable when the real engine is swapped in.

## Architecture (Current — Stub)

```
┌──────────────────────────────────────────┐
│  CoerceRuntime                           │
│  ┌────────────────────────────────────┐  │
│  │ inner: TestRuntime                 │  │ ← All spawns/messages delegate here
│  │ node_id: NodeId("coerce-node")     │  │
│  │ next_remote_local: AtomicU64       │  │ ← Offset counter for remote spawn
│  │                                    │  │
│  │ spawn_manager: SpawnManager        │  │ ← Struct-based system actors
│  │ watch_manager: WatchManager        │  │
│  │ cancel_manager: CancelManager      │  │
│  │ node_directory: NodeDirectory      │  │
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
         │
         ▼
┌──────────────────────────────────────────┐
│  CoerceActorRef<A>                       │
│  ┌────────────────────────────────────┐  │
│  │ inner: TestActorRef<A>             │  │ ← All ActorRef methods delegate
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
```

## Why a Stub?

The coerce-rs crate (`coerce-rt`) may have dependency or maintenance issues.
This stub lets us:

1. **Lock down the API surface** — `CoerceRuntime` and `CoerceActorRef` define
   the public interface that will be preserved when the real engine arrives.
2. **Pass conformance tests** — all 16 conformance tests pass via TestRuntime.
3. **Wire system actors** — SpawnManager, WatchManager, CancelManager,
   NodeDirectory are wired with the same API as ractor/kameo.

## Spawn Mechanism

All spawn methods delegate directly to `TestRuntime`:

```rust
pub fn spawn<A>(&self, name: &str, args: A::Args) -> CoerceActorRef<A> {
    CoerceActorRef { inner: self.inner.spawn(name, args) }
}
```

No type-erasure wrapper (`DactorMsg`) is needed since TestRuntime already
handles dispatch internally.

## Dual ID Counter

The stub has two independent ID counters:

| Counter | Source | Range | Purpose |
|---------|--------|-------|---------|
| TestRuntime's `next_local` | `AtomicU64(1)` | 1, 2, 3, ... | Local spawns via `spawn()` |
| CoerceRuntime's `next_remote_local` | `AtomicU64(1_000_000)` | 1M, 1M+1, ... | Remote spawns via `handle_spawn_request()` |

This offset prevents collisions between local and remote ActorIds.
When the real coerce engine replaces TestRuntime, a single unified counter
will be used.

## System Actors

System actors are **struct-based only** (no native coerce actors):

- `SpawnManager` — `register_factory()`, `handle_spawn_request()`
- `WatchManager` — `remote_watch()`, `remote_unwatch()`, `notify_terminated()`
- `CancelManager` — `register_cancel()`, `cancel_request()`, `complete_request()`
- `NodeDirectory` — `connect_peer()`, `disconnect_peer()`, `is_peer_connected()`

All use `&mut self` methods (no mailbox, no message passing).

## ActorRef Delegation

`CoerceActorRef<A>` implements `ActorRef<A>` by delegating every method
to the inner `TestActorRef<A>`:

```rust
impl<A: Actor + 'static> ActorRef<A> for CoerceActorRef<A> {
    fn id(&self) -> ActorId      { self.inner.id() }
    fn name(&self) -> String     { self.inner.name() }
    fn is_alive(&self) -> bool   { self.inner.is_alive() }
    fn stop(&self)               { self.inner.stop() }
    fn tell<M>(&self, msg: M)    { self.inner.tell(msg) }
    fn ask<M>(&self, msg, cancel){ self.inner.ask(msg, cancel) }
    fn stream<M>(...)            { self.inner.stream(...) }
    fn feed<Item, Reply>(...)    { self.inner.feed(...) }
}
```

## Feature Gaps vs Ractor/Kameo

| Feature | Ractor | Kameo | Coerce (Stub) |
|---------|--------|-------|---------------|
| Real provider runtime | ✅ | ✅ | ❌ Wraps TestRuntime |
| Type-erased dispatch | ✅ DactorMsg | ✅ DactorMsg | ❌ Delegates to TestRuntime |
| Native system actors | ✅ ractor::Actor | ✅ kameo::Actor | ❌ Struct-based only |
| Runtime auto-start | ✅ start_system_actors() | ✅ start_system_actors() | ❌ |
| ClusterEvents impl | ✅ RactorClusterEvents | ✅ KameoClusterEvents | ❌ |
| ClusterEvent emission | ✅ connect/disconnect | ✅ connect/disconnect | ❌ |
| Lifecycle handles | ✅ JoinHandle | ✅ oneshot | ❌ |
| Outbound interceptors | ✅ Full pipeline | ✅ Full pipeline | ⚠️ Via TestRuntime |
| Watch/unwatch | ✅ Native wiring | ✅ Native wiring | ⚠️ Via TestRuntime |
| Bounded mailbox | ⚠️ Warning only | ⚠️ Warning only | ⚠️ Via TestRuntime |

## Planned Work (Phase 12: Coerce Adapter Parity)

See `docs/progress.md` Phase 12 (CP1-CP10) for the full plan to bring
coerce to feature parity with ractor and kameo. Key milestones:

1. **CP1**: Replace TestRuntime with real `coerce-rt` actors
2. **CP2-CP3**: ClusterEvents and emission
3. **CP4**: Lifecycle handles (await_stop/await_all)
4. **CP5-CP6**: Native system actors + runtime auto-start
5. **CP7-CP9**: Interceptors, watch, mailbox on real engine
6. **CP10**: Comprehensive test suite

CP1 (coerce-rt integration) is the gate — all other items depend on it.

## Limitations

| Limitation | Description | Impact |
|-----------|-------------|--------|
| **Entire runtime is a stub** | All functionality delegated to TestRuntime | Not production-ready |
| **No real coerce actors** | No `coerce::Actor` trait impl | Missing supervision, sharding |
| **Dual ID counters** | Local and remote spawns use different counters | Offset prevents collisions |
| **No ClusterEvents** | No subscription system for membership changes | Cannot react to topology changes |
| **No lifecycle handles** | No await_stop/await_all | Cannot do graceful shutdown |
| **No native system actors** | System actors are plain structs | No mailbox isolation |
