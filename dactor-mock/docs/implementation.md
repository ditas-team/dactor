# dactor-mock — Implementation Details

## Overview

The mock adapter provides `MockCluster`, `MockNode`, and `MockNetwork` for
testing dactor actor systems in a single process. It simulates a multi-node
cluster without real networking, enabling unit testing of cross-node
messaging, fault injection, and cluster behavior.

## Architecture

```
┌──────────────────────────────────────────────────────┐
│  MockCluster                                         │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐    │
│  │ MockNode     │ │ MockNode     │ │ MockNode     │    │
│  │ "node-1"     │ │ "node-2"     │ │ "node-3"     │    │
│  │              │ │              │ │              │    │
│  │ TestRuntime  │ │ TestRuntime  │ │ TestRuntime  │    │
│  │ SpawnManager │ │ SpawnManager │ │ SpawnManager │    │
│  │ WatchManager │ │ WatchManager │ │ WatchManager │    │
│  │ CancelManager│ │ CancelManager│ │ CancelManager│   │
│  │ NodeDirectory│ │ NodeDirectory│ │ NodeDirectory│   │
│  └─────────────┘ └─────────────┘ └─────────────┘    │
│                                                      │
│  network: Arc<MockNetwork>  ← fault injection        │
└──────────────────────────────────────────────────────┘
```

## MockNode

Each `MockNode` contains:

- **`node_id: NodeId`** — unique identity
- **`runtime: TestRuntime`** — dactor's built-in test runtime for local
  actor spawning and messaging
- **`spawn_manager: SpawnManager`** — handles remote actor spawn requests
- **`watch_manager: WatchManager`** — tracks remote watch subscriptions
- **`cancel_manager: CancelManager`** — manages remote cancellation tokens
- **`node_directory: NodeDirectory`** — tracks peer connection state

### Node Creation

```rust
pub fn new(node_id: NodeId) -> Self {
    let runtime = TestRuntime::with_node_id(node_id.clone());
    Self {
        node_id,
        runtime,
        spawn_manager: SpawnManager::new(TypeRegistry::new()),
        watch_manager: WatchManager::new(),
        cancel_manager: CancelManager::new(),
        node_directory: NodeDirectory::new(),
    }
}
```

### Factory Registration

Nodes can register actor factories for remote spawning:

```rust
node.register_factory("myapp::Counter", |bytes| {
    let args: CounterArgs = serde_json::from_slice(bytes)?;
    Ok(Box::new(args))
});
```

### Peer Management

- `connect_peer(peer_id)` — marks peer as `Connected` in NodeDirectory
- `disconnect_peer(peer_id)` — marks peer as `Disconnected`

## MockCluster

### Creation

Creates N nodes, all connected to each other:

```rust
let cluster = MockCluster::new(&["node-1", "node-2", "node-3"]);
// Each node's NodeDirectory has the other 2 as Connected peers
```

### Node Access

```rust
let node = cluster.node("node-1");        // immutable
let node = cluster.node_mut("node-1");     // mutable
```

### Fault Injection

```rust
// Crash a node — removes it, marks as Disconnected on survivors
cluster.crash_node("node-2");

// Restart a node — fresh node, reconnects to all peers
cluster.restart_node("node-2");

// Freeze/unfreeze — temporary removal, retains runtime state
let frozen = cluster.freeze_node("node-2");
cluster.unfreeze_node(frozen.unwrap());
```

### Watch Operations

```rust
// Local watch (same node)
cluster.watch("node-1", &watcher_ref, target_id);
cluster.unwatch("node-1", &watcher_id, &target_id);

// Remote watch (cross-node)
cluster.remote_watch("node-1", target_id, watcher_id);
cluster.remote_unwatch("node-1", &target_id, &watcher_id);

// Trigger termination notifications
let notifications = cluster.notify_terminated("node-1", &terminated_id);
```

### Cancellation

```rust
cluster.register_cancel("node-1", "req-1".into(), token);
let response = cluster.cancel_request("node-1", "req-1");
```

### Cluster State

```rust
let state = cluster.state(); // Option<ClusterState>
// ClusterState { local_node, nodes, is_leader }
```

## MockNetwork

The `MockNetwork` provides fault injection for simulating network
conditions. It is shared across all nodes via `Arc<MockNetwork>`.

Currently supports:
- Node isolation (via crash_node/freeze_node)
- Delivery tracking via the cluster's node directory

## System Actors

All four system actors are **struct-based** (same as coerce adapter):

- Direct method calls, no mailboxes
- State is per-node, managed in `MockNode`
- The `MockCluster` provides convenience methods that delegate to the
  appropriate node's system actor

## Differences from Production Adapters

| Feature | Ractor/Kameo | Mock |
|---------|-------------|------|
| **Actor runtime** | Real provider actors | TestRuntime (in-process tasks) |
| **Cross-node messaging** | Transport layer (WireEnvelope) | Direct method calls on MockCluster |
| **Fault injection** | Not built-in | crash_node, freeze/unfreeze |
| **System actors** | Native provider actors | Struct-based (direct methods) |
| **Network simulation** | Real TCP/gRPC | MockNetwork (configurable) |
| **ClusterEvents** | Event emission on connect/disconnect | Not wired (direct node management) |

## Use Cases

1. **Unit testing cross-node behavior** — test remote watch, spawn, cancel
   without real networking
2. **Fault injection testing** — crash nodes, verify watch notifications,
   test recovery paths
3. **Conformance verification** — validate that system actor APIs work
   correctly before wiring to a real transport
4. **Integration test scaffolding** — build test scenarios that mirror
   production cluster topology

## Limitations

| Limitation | Description |
|-----------|-------------|
| **No real networking** | All communication is in-process method calls |
| **No message serialization** | Messages are passed by reference, not serialized |
| **No transport layer** | No WireEnvelope, no message routing |
| **No ClusterEvents** | connect/disconnect doesn't emit events |
| **Single-process only** | Cannot simulate real multi-process clusters |
| **No supervision** | Actor failures don't propagate across nodes |
