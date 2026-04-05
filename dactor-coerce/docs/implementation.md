# dactor-coerce — Implementation Details

## Overview

The coerce adapter provides `CoerceRuntime`, which bridges dactor's API
with the [coerce-rs](https://github.com/LeonHartley/Coerce-rs) actor
framework (v0.8). Actors are spawned as real coerce actors with type-erased
message dispatch.

## Architecture

```
┌──────────────────────────────────────────┐
│  CoerceRuntime                           │
│  ┌────────────────────────────────────┐  │
│  │ system: ActorSystem               │  │ ← coerce actor system
│  │ node_id: NodeId("coerce-node")    │  │
│  │ next_local: Arc<AtomicU64>        │  │ ← unified ID counter
│  │ cluster_events: CoerceClusterEvents│  │
│  │ watchers: WatcherMap              │  │ ← shared watch registry
│  │ stop_receivers: Arc<Mutex<...>>   │  │ ← lifecycle handles
│  │                                    │  │
│  │ spawn_manager: SpawnManager        │  │ ← struct-based system actors
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
│  │ inner: LocalActorRef<             │  │
│  │          CoerceDactorActor<A>>     │  │ ← real coerce actor ref
│  │ outbound_interceptors: Arc<Vec>   │  │
│  │ drop_observer, dead_letter_handler│  │
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
         │
         ▼
┌──────────────────────────────────────────┐
│  CoerceDactorActor<A>  (coerce::Actor)   │
│  ┌────────────────────────────────────┐  │
│  │ actor: A                          │  │ ← dactor Actor instance
│  │ ctx: dactor::ActorContext         │  │
│  │ interceptors: Vec<InboundIntcptr> │  │
│  │ watchers: WatcherMap              │  │
│  │ stop_notifier: Mutex<Option<...>> │  │
│  └────────────────────────────────────┘  │
│                                          │
│  Handler<DactorMsg<A>>                   │
│  → inbound interceptor pipeline          │
│  → cancellation check                    │
│  → panic-catching dispatch               │
│  → on_complete interceptor callback      │
└──────────────────────────────────────────┘
```

## Type-Erased Dispatch

Messages flow through a `DactorMsg<A>(Box<dyn Dispatch<A>>)` envelope,
the same pattern used by ractor and kameo adapters. `DactorMsg` implements
`coerce::actor::message::Message` (with `type Result = ()`), and
`CoerceDactorActor<A>` implements `coerce::actor::message::Handler<DactorMsg<A>>`.

The handler runs the full inbound interceptor pipeline, panic catching,
cancellation racing, and post-dispatch interceptor callbacks.

## Spawn Mechanism

Spawning uses coerce's synchronous `start_actor()` function, which creates
an unbounded mpsc channel and spawns a tokio task for the actor loop:

```rust
let coerce_ref = coerce::actor::scheduler::start_actor(
    wrapper,              // CoerceDactorActor<A>
    new_actor_id(),       // coerce ActorId (UUID)
    ActorType::Anonymous, // not registered in scheduler
    None,                 // no on_start notification
    Some(system),         // ActorSystem reference
    None,                 // no parent
    name.into(),          // ActorPath
);
```

This is non-blocking — the actor's `started()` callback (which calls
dactor's `on_start`) runs asynchronously on the tokio runtime.

## Lifecycle

- **started()** → calls `dactor::Actor::on_start()`
- **Handler<DactorMsg>** → runs interceptor pipeline, dispatches to inner actor
- **stopped()** → calls `dactor::Actor::on_stop()`, notifies watchers,
  sends stop_notifier for `await_stop()`

## Feature Parity with Ractor/Kameo

| Feature | Ractor | Kameo | Coerce |
|---------|--------|-------|--------|
| Real provider runtime | ✅ | ✅ | ✅ |
| Type-erased dispatch | ✅ DactorMsg | ✅ DactorMsg | ✅ DactorMsg |
| ClusterEvents impl | ✅ | ✅ | ✅ CoerceClusterEvents |
| ClusterEvent emission | ✅ | ✅ | ✅ |
| Lifecycle handles | ✅ JoinHandle | ✅ oneshot | ✅ oneshot |
| Outbound interceptors | ✅ Full pipeline | ✅ Full pipeline | ✅ Full pipeline |
| Inbound interceptors | ✅ Full pipeline | ✅ Full pipeline | ✅ Full pipeline |
| Watch/unwatch | ✅ | ✅ | ✅ WatcherMap |
| Native system actors | ✅ | ✅ | ❌ Struct-based |
| Bounded mailbox | ⚠️ Warning | ⚠️ Warning | ⚠️ Warning |

## Limitations

| Limitation | Description | Impact |
|-----------|-------------|--------|
| **Actors must be Send + Sync** | coerce::Actor requires Sync | Most actors satisfy this |
| **No native system actors** | System actors are plain structs | No mailbox isolation |
| **Unbounded mailbox only** | coerce uses unbounded mpsc internally | Cannot limit backpressure |
| **Requires tokio context** | ActorSystem::new() spawns internal actors | Must construct within tokio runtime |
