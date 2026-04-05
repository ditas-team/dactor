# dactor-ractor — Implementation Details

## Overview

The ractor adapter bridges dactor's `Actor`/`Handler<M>`/`ActorRef<A>` API
with [ractor](https://crates.io/crates/ractor)'s single-message-type
`ractor::Actor` trait using type-erased dispatch envelopes.

Each dactor actor is spawned as a real ractor actor via `ractor::Actor::spawn`.
Multiple `Handler<M>` impls per actor are supported through a single
`DactorMsg<A>(Box<dyn Dispatch<A>>)` message type.

## Architecture

```
┌─────────────────────────────────────────────────────┐
│  RactorRuntime                                      │
│  ┌──────────────┐  ┌─────────────────────────────┐  │
│  │ next_local   │  │ System Actors (optional)     │  │
│  │ (Arc<AtomicU64>)│ SpawnManagerActor            │  │
│  │              │  │ WatchManagerActor            │  │
│  │ watchers     │  │ CancelManagerActor           │  │
│  │ (WatcherMap) │  │ NodeDirectoryActor           │  │
│  └──────────────┘  └─────────────────────────────┘  │
└───────────────┬─────────────────────────────────────┘
                │ spawn_internal()
                ▼
┌───────────────────────────────────────────┐
│  RactorDactorActor<A>  (ractor::Actor)    │
│  ┌──────────────────────────────────────┐ │
│  │ RactorActorState<A>                  │ │
│  │   actor: A (dactor Actor instance)   │ │
│  │   ctx: ActorContext                  │ │
│  │   interceptors: Vec<InboundInterceptor>│
│  │   watchers: WatcherMap (shared)      │ │
│  │   dead_letter_handler                │ │
│  └──────────────────────────────────────┘ │
│                                           │
│  Msg = DactorMsg<A>(Box<dyn Dispatch<A>>) │
│                                           │
│  pre_start → A::create() + on_start()     │
│  handle    → interceptors → dispatch      │
│  post_stop → on_stop() + notify watchers  │
└───────────────────────────────────────────┘
```

## Spawn Mechanism

Ractor's `Actor::spawn()` is **async**, but dactor's spawn API is **sync**.
The adapter bridges this with a dedicated OS thread:

```
spawn_internal(&self, name, args, deps, interceptors)
    │
    ├─ Generate ActorId (next_local.fetch_add)
    ├─ Build RactorSpawnArgs
    │
    ├─ std::thread::spawn ──────────────────────┐
    │     handle.block_on(async {               │
    │         ractor::Actor::spawn(...).await    │
    │         tx.send(Ok((actor_ref, join_handle)))
    │     })                                    │
    │                                           │
    ├─ rx.recv() ◄──────────────────────────────┘
    │     (blocks calling thread until spawn completes)
    │
    ├─ Store JoinHandle in join_handles map
    └─ Return RactorActorRef
```

**Cost**: ~100μs per spawn (OS thread creation + channel round-trip).
This will be eliminated when spawn becomes async (Phase 11: AS1).

## Message Dispatch

All message types flow through a single type-erased envelope:

```rust
struct DactorMsg<A: Actor>(Box<dyn Dispatch<A>>);
```

The `Dispatch<A>` trait has four concrete implementations:

| Dispatch Type | Used By | Reply Mechanism |
|--------------|---------|-----------------|
| `TypedDispatch { msg }` | `tell()` | None (fire-and-forget) |
| `AskDispatch { msg, reply_tx, cancel }` | `ask()` | `oneshot::Sender` |
| `StreamDispatch { msg, sender, cancel }` | `stream()` | `mpsc::Sender` |
| `FeedDispatch { receiver, reply_tx, cancel }` | `feed()` | `oneshot::Sender` |

## Inbound Interceptor Pipeline

```
Message arrives in ractor mailbox
    │
    ▼
for interceptor in &state.interceptors:
    match interceptor.on_receive(&ctx, &headers, &msg_any):
        Continue  → next interceptor
        Delay(d)  → accumulate delay
        Drop      → route to dead letter handler, reject dispatch
        Reject(r) → reject dispatch with reason
        Retry(d)  → reject dispatch with retry-after
    │
    ▼ (if all Continue)
    sleep(total_delay) if any
    │
    ▼
    dispatch.dispatch(&mut actor, &mut ctx)
    │
    ▼
for interceptor in &state.interceptors:
    interceptor.on_complete(&ctx, &headers, &outcome)
    │
    ▼
    dispatch_result.send_reply()  // reply sent AFTER interceptors
```

## Outbound Interceptor Pipeline

Applied in `RactorActorRef` before sending any message:

```rust
let pipeline = self.outbound_pipeline();
let result = pipeline.run_on_send(SendMode::Tell, &msg);
match result.disposition {
    Continue => { /* proceed with send */ }
    Drop     => { return Ok(()); }  // silently dropped
    Reject   => { return Err(RuntimeError::Rejected { ... }); }
    Retry    => { return Err(RuntimeError::RetryAfter { ... }); }
}
```

## Cancellation

Cancellation uses `tokio::select!` with biased preference for dispatch
completion (to avoid unnecessary cancellation when the handler is about
to finish):

```rust
tokio::select! {
    biased;
    r = dispatch_fut => r,           // prefer completion
    _ = token.cancelled() => {       // cancel if token fires
        // dispatch_fut dropped → reply_tx dropped → caller sees channel closed
        return Ok(());
    }
}
```

Pre-dispatch check: if token is already cancelled before dispatch starts,
`dispatch.cancel()` is called which sends `RuntimeError::Cancelled`.

## Watch / DeathWatch

Watch entries are stored in a shared `WatcherMap`:

```rust
type WatcherMap = Arc<Mutex<HashMap<ActorId, Vec<WatchEntry>>>>;

struct WatchEntry {
    watcher_id: ActorId,
    notify: Box<dyn Fn(ChildTerminated) + Send + Sync>,
}
```

- **Register**: `runtime.watch(watcher_ref, target_id)` creates a type-erased
  closure that sends `ChildTerminated` to the watcher via ractor's mailbox.
- **On stop**: `post_stop()` drains the watch map bidirectionally — removing
  entries where the stopped actor is either a target or a watcher.
- **Remote watches**: Managed by `WatchManager` (struct-based). Not yet
  auto-wired to `post_stop`.

## Lifecycle Handles

JoinHandles from `ractor::Actor::spawn` are stored in a map:

```rust
join_handles: Arc<Mutex<HashMap<ActorId, JoinHandle<()>>>>
```

- `await_stop(id)` — removes and awaits the JoinHandle
- `await_all()` — drains all handles, awaits all (collects first error)
- `cleanup_finished()` — removes handles for actors that already stopped
- `active_handle_count()` — number of stored handles (may include finished)

## Native System Actors

When `start_system_actors()` is called, the runtime spawns 4 native ractor
actors using a shared `Arc<AtomicU64>` counter:

| Actor | Message Enum | State |
|-------|-------------|-------|
| `SpawnManagerActor` | `SpawnManagerMsg` | `SpawnManagerState` (SpawnManager + NodeId + counter) |
| `WatchManagerActor` | `WatchManagerMsg` | `WatchManager` |
| `CancelManagerActor` | `CancelManagerMsg` | `CancelManager` |
| `NodeDirectoryActor` | `NodeDirectoryMsg` | `NodeDirectory` |

System actors use ractor's `cast()` for fire-and-forget messages and
`oneshot` channels for request-response (ask semantics).

## Limitations

| Limitation | Description | Planned Fix |
|-----------|-------------|-------------|
| **Bounded mailbox** | Only unbounded supported; bounded logs a warning | Phase 11 or provider-specific work |
| **Restart** | `ErrorAction::Restart` treated as `Resume` | Requires ractor restart mechanism |
| **Sync spawn** | OS thread per spawn (~100μs overhead) | Phase 11: AS1 (async spawn) |
| **Remote watch auto-wiring** | `notify_terminated()` must be called manually | NA10 (transport routing) |
| **Global actor registry** | Named ractor actors use a global registry; name collisions across runtimes possible | Use `None` names or unique prefixes |
| **Panic handling** | Handler panics caught via `catch_unwind`; may not catch all FFI panics | Inherent Rust limitation |

## ClusterEvent Emission

`connect_peer()` and `disconnect_peer()` automatically emit cluster events:

- `NodeJoined` emitted when a peer transitions to `Connected` (not already connected)
- `NodeLeft` emitted when a previously-connected peer is disconnected
- Subscriber panics are isolated via `catch_unwind` — one bad callback won't
  affect other subscribers or the peer status update
