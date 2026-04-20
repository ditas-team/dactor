# Version Compatibility & Rolling Upgrades

> How dactor handles version differences between nodes in a cluster —
> wire protocol versioning, application-level schema evolution, and
> deployment strategies for both infrastructure and application changes.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Category 1: Infrastructure-Level Change](#2-category-1-infrastructure-level-change)
3. [Category 2: Application-Level Change](#3-category-2-application-level-change)
4. [Version Detection Protocol](#4-version-detection-protocol)
5. [Implementation Plan](#5-implementation-plan)
6. [Compatibility Matrix](#6-compatibility-matrix)
7. [Deployment Guides](#7-deployment-guides)
8. [Error Handling](#8-error-handling)
9. [Testing Strategy](#9-testing-strategy)
10. [FAQ](#10-faq)

---

## 1. Overview

### Why Version Compatibility Matters

In a distributed actor system, nodes communicate by sending serialized
messages over the network. Every message travels as a
[`WireEnvelope`](../dactor/src/remote.rs) containing:

- **Target** — the actor to receive the message
- **Message type** — the Rust type name used for deserialization dispatch
- **Body** — the serialized payload (JSON or custom codec)
- **Version** — optional per-message version for schema evolution
- **Headers** — key-value metadata propagated across nodes

When any node in the cluster is upgraded — whether the dactor framework
itself or just the application code — the set of messages it can send and
receive may change.  If two nodes disagree on the shape of a message, one
of three things happens:

1. **Silent data loss** — a field is dropped without warning.
2. **Deserialization error** — the receiving node cannot parse the payload.
3. **Semantic mismatch** — the payload parses, but means something different.

All three are bad.  The dactor version compatibility design eliminates (1)
and (3) through explicit version tracking and migration, and turns (2) into
a clear, actionable error.

### Two Fundamental Upgrade Categories

Every upgrade falls into one of two categories:

| | Category 1 | Category 2 |
|---|---|---|
| **What changed** | dactor framework, adapter crate, or wire protocol | Application code only (dactor unchanged) |
| **System actors** | May be incompatible | Always compatible |
| **Cluster formation** | Nodes **cannot** join the same cluster | Nodes **can** join the same cluster |
| **Upgrade strategy** | Cluster split (blue/green) | Rolling restart |
| **Rollback** | Switch traffic back to old cluster | Roll back individual nodes |
| **Detection** | Wire protocol version in handshake | Application version in handshake |

The sections below describe each category in detail.

---

## 2. Category 1: Infrastructure-Level Change

### When It Applies

Category 1 applies when **any** of the following change between nodes:

- **dactor core version** (major version bump)
- **Adapter crate version** (e.g. `dactor-ractor` 0.2 → 0.3)
- **Wire protocol format** (protobuf schema, WireEnvelope framing)
- **System message semantics** (new required fields in SpawnRequest, etc.)

### Why Nodes Cannot Form a Single Cluster

System actors — `SpawnManager`, `WatchManager`, `CancelManager`,
`NodeDirectory` — are the **backbone** of inter-node communication.
Every remote spawn, remote watch, and peer connection handshake flows
through them.

These system messages use **protobuf** with frozen field numbers
(see [`proto.rs`](../dactor/src/proto.rs)):

```
// From system_actors.rs — these constants are frozen wire protocol values.
// A unit test enforces they never change.
pub const SYSTEM_MSG_TYPE_SPAWN:           &str = "dactor::system_actors::SpawnRequest";
pub const SYSTEM_MSG_TYPE_WATCH:           &str = "dactor::system_actors::WatchRequest";
pub const SYSTEM_MSG_TYPE_UNWATCH:         &str = "dactor::system_actors::UnwatchRequest";
pub const SYSTEM_MSG_TYPE_CANCEL:          &str = "dactor::system_actors::CancelRequest";
pub const SYSTEM_MSG_TYPE_CONNECT_PEER:    &str = "dactor::system_actors::ConnectPeer";
pub const SYSTEM_MSG_TYPE_DISCONNECT_PEER: &str = "dactor::system_actors::DisconnectPeer";
```

While the field numbers are frozen and the type-name strings are stable
across minor releases, a **major** dactor version change may:

- Add new **required** fields to system messages (e.g. a `capabilities`
  field in `ConnectPeer`)
- Change the **semantics** of existing fields (e.g. `SpawnResponse` error
  codes)
- Alter the **WireEnvelope framing** itself (e.g. new mandatory envelope
  fields)

When this happens, a node running dactor 0.2 cannot deserialize a system
message from a node running dactor 0.3.  The cluster **cannot function** as
a mixed-version deployment.

### Wire Protocol Version Detection

The very first message exchanged when two nodes connect is the **handshake**
(see [§4 — Version Detection Protocol](#4-version-detection-protocol)).
The handshake includes the `dactor_wire_version`.  If the versions are
incompatible, the connection is **rejected immediately** with a
`NodeRejected` cluster event — not silently dropped.

### Upgrade Strategy: Cluster Split (Blue/Green)

Because nodes with different wire protocol versions cannot coexist in one
cluster, the upgrade strategy is a **cluster-level blue/green deployment**.

```
                        ┌─────────────────────────────────────────┐
                        │           Load Balancer / DNS           │
                        │         (or Service Mesh Router)        │
                        └──────────────┬──────────────────────────┘
                                       │
                    ┌──────────────────┐│┌──────────────────┐
                    │                  │││                  │
          ┌─────────▼──────────┐   ┌───▼▼──────────────────▼─┐
          │   OLD CLUSTER      │   │   NEW CLUSTER            │
          │   (dactor v0.2)    │   │   (dactor v0.3)          │
          │                    │   │                          │
          │  ┌──────┐ ┌──────┐│   │  ┌──────┐ ┌──────┐      │
          │  │Node-A│ │Node-B││   │  │Node-X│ │Node-Y│      │
          │  └──────┘ └──────┘│   │  └──────┘ └──────┘      │
          │  ┌──────┐         │   │  ┌──────┐ ┌──────┐      │
          │  │Node-C│         │   │  │Node-Z│ │Node-W│      │
          │  └──────┘         │   │  └──────┘ └──────┘      │
          └────────────────────┘   └──────────────────────────┘
                ▲                              ▲
           traffic: 100% → 0%            traffic: 0% → 100%
```

**Steps:**

1. **Deploy new cluster** — spin up a fresh set of nodes running the new
   dactor version.  They form their own cluster.  No traffic is routed to
   them yet.

2. **Validate new cluster** — run health checks, smoke tests, and canary
   probes against the new cluster.

3. **Shift traffic** — gradually move traffic from old cluster to new
   cluster using your load balancer, DNS, or service mesh.

4. **Drain old cluster** — once 100% of traffic is on the new cluster,
   allow the old cluster to drain (finish in-flight work, stop accepting
   new requests).

5. **Decommission old cluster** — tear down old nodes once drain is
   complete.

**Rollback:**  If the new cluster shows problems, switch traffic back to
the old cluster.  The old cluster is still running and healthy until you
explicitly decommission it.

---

## 3. Category 2: Application-Level Change

### When It Applies

Category 2 applies when the dactor framework version **does not change**
(same major + minor wire protocol version), but:

- Application actor code changes
- Application message struct definitions change
- New actor types are added or removed
- Message serialization format changes within user types

### System Actors Work Fine

Because the dactor wire protocol is unchanged, all system actors
communicate normally:

- **SpawnManager** — remote spawns work (as long as the type is registered)
- **WatchManager** — remote watch/unwatch works
- **CancelManager** — remote cancellation works
- **NodeDirectory** — peer connect/disconnect works
- **Cluster formation** — nodes join and leave the cluster normally

### Remote Actor Calls May Fail

While system actors are fine, **application-level** remote actor calls
(`tell`, `ask`) may fail depending on the nature of the schema change.

#### Backward-Compatible Changes (Safe for Rolling Restart)

These changes are safe — old and new nodes can coexist:

| Change | Why It's Safe |
|--------|---------------|
| **Added optional fields** | Old serde schemas use `#[serde(default)]` — missing fields get default values. New nodes send the field; old nodes ignore it. |
| **New message types** | Old nodes receive an unknown `message_type` string and return a deserialization error. The sender gets a clear error — no silent corruption. |
| **New actor types** | Old nodes can't spawn them (type not in registry), but the `SpawnResponse::Error` path handles this cleanly. |
| **Changed default values** | As long as the field type stays the same, serde defaults absorb the change. |

**Example — adding an optional field:**

```rust
// v1: original message
#[derive(Serialize, Deserialize)]
struct PlaceOrder {
    item_id: String,
    quantity: u32,
}

// v2: added optional field with serde default
#[derive(Serialize, Deserialize)]
struct PlaceOrder {
    item_id: String,
    quantity: u32,
    #[serde(default)]                 // old nodes don't send this field
    priority: Option<Priority>,       // new nodes see None from old senders
}
```

Old nodes (v1) send `{ "item_id": "abc", "quantity": 3 }`.
New nodes (v2) receive it and `priority` defaults to `None`.
New nodes send `{ "item_id": "abc", "quantity": 3, "priority": "High" }`.
Old nodes (v1) ignore the unknown `priority` field (serde default behavior
with `#[serde(deny_unknown_fields)]` **not** set).

#### Breaking Changes (Require Careful Handling)

These changes are **not** safe for a simple rolling restart:

| Change | Why It Breaks |
|--------|---------------|
| **Removed fields** | Old nodes send the field; new nodes may not expect it (harmless with serde, but data is lost). New nodes don't send it; old nodes expecting it get a deserialization error. |
| **Renamed fields** | Equivalent to removing one field and adding another. |
| **Changed field types** | `u32` → `String` — deserialization fails. |
| **Changed message semantics** | Payload parses, but means something different. Hardest to detect. |
| **Removed message types** | New nodes don't register the old type — deserialization fails. |

For breaking changes, use the **`MessageVersionHandler`** migration system
or fall back to Category 1 (cluster split) if the changes are too
extensive.

### MessageVersionHandler — Schema Migration

The [`MessageVersionHandler`](../dactor/src/remote.rs) trait enables
per-message-type version migration at the byte level:

```rust
pub trait MessageVersionHandler: Send + Sync + 'static {
    /// The message type this handler manages.
    fn message_type(&self) -> &'static str;

    /// Migrate a message payload from an older version to the current version.
    /// Returns the migrated bytes, or None if migration is not possible.
    fn migrate(&self, payload: &[u8], from_version: u32, to_version: u32) -> Option<Vec<u8>>;
}
```

The runtime calls `receive_envelope_body_versioned()` on the receiving side.
If the incoming `WireEnvelope.version` differs from the node's expected
version for that message type, the registered handler gets a chance to
transform the bytes before deserialization.

#### When to Use MessageVersionHandler vs. Serde Defaults

| Scenario | Approach |
|----------|----------|
| Added optional field | Serde `#[serde(default)]` — no handler needed |
| Renamed field | Handler: rewrite JSON keys at byte level, or use `#[serde(alias)]` |
| Changed field type | Handler: parse old type, convert, re-serialize as new type |
| Removed field | Handler: inject a placeholder value in the byte stream |
| Complex structural change | Handler: full v1→v2 transformation |

#### Code Example: Migrating v1 → v2

```rust
use dactor::remote::MessageVersionHandler;

/// Migrates PlaceOrder messages from v1 to v2.
///
/// v1: { "item_id": "abc", "quantity": 3 }
/// v2: { "item_id": "abc", "quantity": 3, "priority": null }
struct PlaceOrderMigrator;

impl MessageVersionHandler for PlaceOrderMigrator {
    fn message_type(&self) -> &'static str {
        "myapp::orders::PlaceOrder"
    }

    fn migrate(&self, payload: &[u8], from_version: u32, to_version: u32) -> Option<Vec<u8>> {
        match (from_version, to_version) {
            (1, 2) => {
                // Parse v1 JSON, add missing field, re-serialize
                let mut v: serde_json::Value = serde_json::from_slice(payload).ok()?;
                let obj = v.as_object_mut()?;
                obj.entry("priority").or_insert(serde_json::Value::Null);
                serde_json::to_vec(&v).ok()
            }
            (2, 3) => {
                // v2→v3: rename "quantity" → "qty"
                let mut v: serde_json::Value = serde_json::from_slice(payload).ok()?;
                let obj = v.as_object_mut()?;
                if let Some(qty) = obj.remove("quantity") {
                    obj.insert("qty".into(), qty);
                }
                serde_json::to_vec(&v).ok()
            }
            (1, 3) => {
                // Chain: v1→v2→v3
                let v2 = self.migrate(payload, 1, 2)?;
                self.migrate(&v2, 2, 3)
            }
            _ => None, // Unknown version pair — cannot migrate
        }
    }
}
```

**Registering the handler:**

```rust
// During runtime setup, register version handlers
let mut version_handlers: HashMap<String, Box<dyn MessageVersionHandler>> =
    HashMap::new();
version_handlers.insert(
    "myapp::orders::PlaceOrder".into(),
    Box::new(PlaceOrderMigrator),
);
```

**Setting the version on outgoing messages:**

```rust
use dactor::remote::build_wire_envelope;

let envelope = build_wire_envelope(
    target_id,
    "order-processor",
    &place_order_msg,
    SendMode::Tell,
    headers,
    None,           // request_id
    Some(2),        // message version — v2
)?;
```

### Upgrade Strategy: Rolling Restart

Because nodes share the same wire protocol, they can coexist in one cluster
while being upgraded one at a time.

```
     Time ──────────────────────────────────────────────►

     Node-A:  [  v1  ]---------[ upgrading ]--[  v2  ]----------
     Node-B:  [  v1  ]----------------------[ upgrading ]--[ v2 ]
     Node-C:  [  v1  ]---[ upgrading ]--[  v2  ]----------------
                                        ▲
                              mixed-version window
                       (MessageVersionHandler active)
```

**Steps:**

1. **Register `MessageVersionHandler`s** for any messages with breaking
   schema changes.  The handlers must support migration from the old
   version to the new version **and** optionally vice versa for rollback.

2. **Set message versions** on all outgoing envelopes using
   `WireEnvelope.version`.

3. **Upgrade one node** — drain it (stop sending new work), wait for
   in-flight messages to complete, deploy the new version, rejoin the
   cluster.

4. **Observe** — monitor deserialization errors, migration fallbacks, and
   cluster events.  If the node is healthy, proceed.

5. **Repeat** for each node until all nodes are on the new version.

6. **Clean up** — after all nodes are upgraded, remove migration handlers
   for the old version in a subsequent release.

**Rollback:**  Roll back individual nodes by re-deploying the old version.
The `MessageVersionHandler`s still handle the version difference.  No
cluster-level cutover needed.

---

## 4. Version Detection Protocol

### Connect Handshake

When Node-A establishes a connection to Node-B, the first exchange is a
**version handshake** that determines whether the nodes are compatible:

```
  Node-A                                           Node-B
    │                                                 │
    │  ──── HandshakeRequest ──────────────────────►  │
    │  {                                              │
    │    dactor_wire_version: "0.2.0",                │
    │    app_version: "1.5.3",                        │
    │    node_id: "node-a-abc123",                    │
    │    adapter: "dactor-ractor"                     │
    │  }                                              │
    │                                                 │
    │  ◄──── HandshakeResponse ────────────────────   │
    │  {                                              │
    │    accepted: true,                              │
    │    dactor_wire_version: "0.2.0",                │
    │    app_version: "1.6.0",                        │
    │    node_id: "node-b-def456"                     │
    │  }                                              │
    │                                                 │
    │  ═══════ Normal WireEnvelope traffic ══════════ │
    │                                                 │
```

**Decision logic on Node-B:**

```
if Node-A.dactor_wire_version.MAJOR != Node-B.dactor_wire_version.MAJOR:
    → REJECT connection
    → emit ClusterEvent::NodeRejected {
          node_id: "node-a-abc123",
          reason: IncompatibleProtocol,
          detail: "wire version 0.2.0 vs 1.0.0"
      }

if Node-A.dactor_wire_version.MINOR > Node-B.dactor_wire_version.MINOR:
    → ACCEPT connection (backward-compatible)
    → log warning: "peer uses newer wire version 0.3.0 (local: 0.2.0)"

if Node-A.app_version != Node-B.app_version:
    → ACCEPT connection
    → log info: "peer app version 1.5.3 differs from local 1.6.0"
    → expose in ClusterState for operational visibility

else:
    → ACCEPT connection (fully compatible)
```

### Wire Protocol Version

The wire protocol version follows **semantic versioning** applied to the
on-the-wire format:

| Component | When to Bump | Example |
|-----------|-------------|---------|
| **MAJOR** | Breaking wire change — Category 1 required | `0.2.x` → `1.0.0`: new required fields in `WireEnvelope` |
| **MINOR** | Backward-compatible additions to wire format | `0.2.0` → `0.3.0`: optional field added to `ConnectPeer` |
| **PATCH** | No wire changes at all (bug fixes, perf) | `0.2.0` → `0.2.1`: internal deserialization fix |

**Rules:**

- Nodes with the **same MAJOR** version can always connect.
- Nodes with **different MAJOR** versions can **never** connect.
- A node with a **lower MINOR** version can connect to a node with a
  **higher MINOR** version, because the higher-minor node only added
  optional features that the lower-minor node can ignore.
- PATCH differences have **no effect** on compatibility.

The wire protocol version is a **compile-time constant** in `dactor`:

```rust
/// Current wire protocol version for inter-node communication.
///
/// Bumped according to semantic versioning of the on-the-wire format:
/// - MAJOR: breaking wire change (Category 1 — cluster split required)
/// - MINOR: backward-compatible wire additions
/// - PATCH: no wire changes
pub const DACTOR_WIRE_VERSION: &str = "0.2.0";
```

### Application Version

The application version is **user-configured** and included in the
handshake purely for **operational visibility**.  It does not affect
connection acceptance/rejection.

```rust
let runtime = DactorRuntime::builder()
    .app_version("1.5.3")       // included in handshake
    .build();
```

The application version appears in:

- Handshake messages (both request and response)
- `ClusterState` (queryable at runtime for dashboards)
- Log messages when app versions differ between peers
- Metrics labels for monitoring mixed-version windows

### Message Version

Per-message versioning is **already implemented** in the `WireEnvelope`:

```rust
pub struct WireEnvelope {
    // ... other fields ...
    /// Message version for schema evolution (None = current version).
    pub version: Option<u32>,
}
```

This version is:

- Set by the **sender** when building the envelope
- Checked by the **receiver** against the expected version
- Used to invoke `MessageVersionHandler.migrate()` if there's a mismatch
- Independent of the wire protocol version and application version

**Three-layer version model summary:**

```
┌─────────────────────────────────────────────────────────┐
│  Wire Protocol Version (DACTOR_WIRE_VERSION)            │
│  Scope: entire inter-node communication                 │
│  Changes: only when dactor framework changes            │
│  Effect: determines cluster compatibility               │
├─────────────────────────────────────────────────────────┤
│  Application Version (user-configured)                  │
│  Scope: deployment-level metadata                       │
│  Changes: every app release                             │
│  Effect: informational — logging, dashboards            │
├─────────────────────────────────────────────────────────┤
│  Message Version (WireEnvelope.version)                 │
│  Scope: individual message type                         │
│  Changes: when a specific message schema changes        │
│  Effect: triggers MessageVersionHandler migration       │
└─────────────────────────────────────────────────────────┘
```

---

## 5. Implementation Plan

### Phase 1: Wire Protocol Version Constant

**Add to `dactor` core:**

- `DACTOR_WIRE_VERSION` constant in `dactor::version` module
- Parse function for comparing wire versions (major/minor/patch)
- Unit tests for version comparison logic

**Files:** `dactor/src/version.rs`, `dactor/src/lib.rs`

### Phase 2: Handshake Protocol via System Actors

**Design principle:** Transport is NOT implemented by dactor core or adapters.
Handshake is performed via the three-tier strategy described in Section 2
(Architectural Principle).

**Tier 1 — Provider Preamble Capability:**

If the underlying provider supports a stable preamble mechanism (e.g., gRPC
metadata headers, HTTP request headers), the adapter models this as a provider
capability. The preamble carries a minimal, frozen version token that never
changes across dactor versions. This gives the best error messages because
rejection happens before any actor message is exchanged.

**Tier 2 — System Actor Handshake (fallback):**

When the provider does not support a preamble, dactor falls back to a
lightweight system actor that exchanges a frozen protobuf preamble message via
`Transport::send_request()`. The handshake types are already defined in
`dactor::system_actors`:

```rust
// Already implemented — no changes to these types
pub struct HandshakeRequest {
    pub dactor_wire_version: String,
    pub app_version: String,
    pub node_id: NodeId,
    pub adapter: String,
}

pub struct HandshakeResponse {
    pub accepted: bool,
    pub dactor_wire_version: String,
    pub app_version: String,
    pub node_id: NodeId,
    pub rejection_reason: Option<String>,
}
```

Validation is done by the adapter using the existing helpers:

```rust
// Adapter code (e.g., in a system actor handler):
let request = runtime.handshake_request();
let response = transport.send_request(&peer_id, serialize(request)).await?;
let hs_response: HandshakeResponse = deserialize(response);
let result = dactor::validate_handshake(&hs_response);
dactor::verify_peer_identity(&hs_response, &expected_peer_id)?;
runtime.connect_peer(peer_id, address);
```

**Tier 3 — Timeout Detection (ultimate fallback):**

If the wire format is completely incompatible (different MAJOR version), even
the preamble message cannot be parsed. The `send_request()` call will time out.
The adapter detects this and marks the remote node as unreachable — effectively
the same as rejected.

**Protobuf framing** for handshake messages in `proto/dactor_system.proto`.

### Phase 3: NodeRejected ClusterEvent

**Extend `ClusterEvent` enum:**

```rust
pub enum ClusterEvent {
    NodeJoined(NodeId),
    NodeLeft(NodeId),
    NodeRejected {
        node_id: NodeId,
        reason: RejectionReason,
        detail: String,
    },
}

pub enum RejectionReason {
    IncompatibleProtocol,   // Category 1 — different wire MAJOR version
    IncompatibleAdapter,    // Different adapter (ractor vs kameo)
    ConnectionFailed,       // Transport-level failure during handshake
}
```

**Emit** `NodeRejected` from `NodeDirectory` when handshake fails.

### Phase 4: Application Version in Runtime Configuration

The `app_version` is **your application's release version** — not a dactor
framework version. You set it when building the runtime so that dactor can
include it in the connect handshake for operational visibility.

**Why it exists:** During rolling upgrades (Category 2), a cluster will
temporarily contain nodes running different application versions. The
`app_version` lets operators see exactly which versions are active in the
cluster — how many nodes are on v2.3.0 vs v2.3.1, whether the rollout is
complete, or whether a rollback is needed.

**Important:** The `app_version` is purely informational. It does **not**
affect connection acceptance or message routing. Two nodes with different
`app_version` values can always cluster together (assuming the wire protocol
version matches). Message compatibility is determined separately by
`WireEnvelope.version` and `MessageVersionHandler`.

**Extend runtime builder:**

```rust
impl DactorRuntimeBuilder {
    /// Set the application version for this node.
    ///
    /// This is YOUR application's release version (e.g., "2.3.1"),
    /// not the dactor framework version. It is included in the connect
    /// handshake and exposed in ClusterState for operational visibility
    /// during rolling upgrades.
    ///
    /// Typical usage: set to the same value as your Cargo.toml version
    /// or your CI/CD build tag.
    pub fn app_version(mut self, version: &str) -> Self {
        self.app_version = Some(version.to_string());
        self
    }
}

// Example:
let runtime = DactorRuntime::builder()
    .app_version("2.3.1")    // your app's release version
    .build();
```

**Where `app_version` appears at runtime:**

- **Connect handshake** — exchanged between peers on connection
- **`ClusterState`** — queryable for dashboards and monitoring
- **Log messages** — logged when peers with different app versions connect
- **Metrics labels** — enables version-aware monitoring during rollouts

**Include in `ClusterState`:**

```rust
pub struct ClusterState {
    pub local_node: NodeId,
    pub nodes: Vec<NodeId>,
    pub is_leader: bool,
    pub app_version: Option<String>,     // this node's app version
    pub wire_version: String,             // DACTOR_PROTOCOL_VERSION
    pub peer_versions: HashMap<NodeId, PeerVersionInfo>,
}

pub struct PeerVersionInfo {
    pub wire_version: String,
    pub app_version: Option<String>,     // peer's app version
    pub adapter: String,
}
```

**Operational example during rolling restart:**

```
ClusterState:
  local_node: node-3 (app_version: "2.3.1")
  peers:
    node-1: app_version "2.3.0"  ← not yet upgraded
    node-2: app_version "2.3.1"  ← upgraded
    node-4: app_version "2.3.0"  ← not yet upgraded
  
  Rollout progress: 2/4 nodes on v2.3.1 (50%)
```

### Phase 5: MessageVersionHandler Playbook

**Document and test:**

- Step-by-step guide for adding a `MessageVersionHandler`
- Examples: v1→v2 field addition, v2→v3 field rename, v1→v3 chained
- Integration test with mixed-version `InMemoryTransport` nodes
- Best practices for versioning policy

---

## 6. Compatibility Matrix

### dactor Wire Protocol Versions

| Wire Version | dactor Crate | Breaking Changes | Compatible With |
|:---:|:---:|---|:---:|
| `0.2.0` | 0.2.0-alpha.1 → 0.2.x | Initial wire format | `0.2.*` |
| `0.3.0` | 0.3.0 (future) | *(TBD — new optional envelope fields)* | `0.3.*`, `0.2.*` (minor bump) |
| `1.0.0` | 1.0.0 (future) | *(TBD — breaking envelope change)* | `1.0.*` only |

### Compatibility Rules

```
Can Node-A (wire v0.2.0) talk to Node-B (wire v0.2.1)?  → YES (patch only)
Can Node-A (wire v0.2.0) talk to Node-B (wire v0.3.0)?  → YES (minor bump — backward compatible)
Can Node-A (wire v0.2.0) talk to Node-B (wire v1.0.0)?  → NO  (major bump — Category 1)
Can Node-A (wire v0.3.0) talk to Node-B (wire v0.2.0)?  → YES (higher minor can talk to lower)
```

### When to Bump Wire Version

| Scenario | Bump |
|----------|------|
| Bug fix in deserialization logic (no wire change) | PATCH |
| New optional field in WireEnvelope (old nodes ignore it) | MINOR |
| New optional system message type (old nodes ignore it) | MINOR |
| New required field in WireEnvelope | MAJOR |
| Changed semantics of existing system message | MAJOR |
| Removed a system message type | MAJOR |
| Changed protobuf field numbers in system messages | MAJOR |
| Changed WireEnvelope framing (protobuf schema) | MAJOR |

---

## 7. Deployment Guides

### Category 1: Cluster Split (Blue/Green)

Use this guide when upgrading across a wire protocol **MAJOR** version
boundary.

#### Kubernetes

```yaml
# old-cluster.yaml — keep running during migration
apiVersion: apps/v1
kind: Deployment
metadata:
  name: myapp-old
  labels:
    app: myapp
    cluster-version: old
spec:
  replicas: 3
  selector:
    matchLabels:
      app: myapp
      cluster-version: old
  template:
    metadata:
      labels:
        app: myapp
        cluster-version: old
    spec:
      containers:
      - name: myapp
        image: myapp:1.0.0    # old dactor version
        env:
        - name: CLUSTER_NAME
          value: "myapp-old"  # separate cluster namespace
---
# new-cluster.yaml — deploy alongside old cluster
apiVersion: apps/v1
kind: Deployment
metadata:
  name: myapp-new
  labels:
    app: myapp
    cluster-version: new
spec:
  replicas: 3
  selector:
    matchLabels:
      app: myapp
      cluster-version: new
  template:
    metadata:
      labels:
        app: myapp
        cluster-version: new
    spec:
      containers:
      - name: myapp
        image: myapp:2.0.0    # new dactor version
        env:
        - name: CLUSTER_NAME
          value: "myapp-new"  # separate cluster namespace
---
# Service — switch selector to cut over
apiVersion: v1
kind: Service
metadata:
  name: myapp
spec:
  selector:
    app: myapp
    cluster-version: old     # ← change to "new" to cut over
  ports:
  - port: 8080
```

**Cutover steps:**

1. `kubectl apply -f new-cluster.yaml` — deploy new cluster
2. Validate: `kubectl exec -it myapp-new-xxx -- curl localhost:8080/health`
3. Cut over: `kubectl patch svc myapp -p '{"spec":{"selector":{"cluster-version":"new"}}}'`
4. Drain old: `kubectl scale deployment myapp-old --replicas=0`
5. Clean up: `kubectl delete -f old-cluster.yaml`

**Rollback:** `kubectl patch svc myapp -p '{"spec":{"selector":{"cluster-version":"old"}}}'`

#### AWS (ALB + ASG)

1. Create a new Auto Scaling Group (`myapp-v2-asg`) with the new AMI/container.
2. Register `myapp-v2-asg` as a target group on the ALB.
3. Use weighted target groups to shift traffic: 90/10 → 50/50 → 0/100.
4. Deregister and terminate the old ASG.

**Rollback:** Shift ALB weights back to old target group.

#### Azure (VMSS + Traffic Manager)

1. Create a new Virtual Machine Scale Set (`myapp-v2-vmss`) with the new image.
2. Add as an endpoint to Traffic Manager with weight 0.
3. Gradually increase weight: 0 → 10 → 50 → 100.
4. Remove old VMSS endpoint and deallocate.

**Rollback:** Shift Traffic Manager weights back to old endpoint.

---

### Category 2: Rolling Restart

Use this guide when upgrading within the same wire protocol version
(application code changes only).

#### Kubernetes (Deployment Rolling Update)

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: myapp
spec:
  replicas: 5
  strategy:
    type: RollingUpdate
    rollingUpdate:
      maxUnavailable: 1      # upgrade one node at a time
      maxSurge: 1
  template:
    spec:
      containers:
      - name: myapp
        image: myapp:1.1.0   # ← bump version
```

```bash
# Rolling update
kubectl set image deployment/myapp myapp=myapp:1.1.0

# Monitor
kubectl rollout status deployment/myapp

# Rollback if needed
kubectl rollout undo deployment/myapp
```

#### AWS (ASG Rolling Update)

```bash
# Update launch template with new image
aws ec2 create-launch-template-version \
  --launch-template-id lt-xxxx \
  --source-version 1 \
  --launch-template-data '{"ImageId":"ami-newversion"}'

# Start rolling update
aws autoscaling start-instance-refresh \
  --auto-scaling-group-name myapp-asg \
  --preferences '{"MinHealthyPercentage":80}'
```

#### MessageVersionHandler Registration

Register version handlers **before** starting the rolling update:

```rust
// In your application setup (BOTH old and new versions must have this):
fn register_version_handlers(
    handlers: &mut HashMap<String, Box<dyn MessageVersionHandler>>
) {
    handlers.insert(
        "myapp::orders::PlaceOrder".into(),
        Box::new(PlaceOrderMigrator),   // handles v1 ↔ v2
    );
    handlers.insert(
        "myapp::users::UpdateProfile".into(),
        Box::new(UpdateProfileMigrator), // handles v3 ↔ v4
    );
}
```

> **Important:** The `MessageVersionHandler` must be deployed to **all**
> nodes before starting the rolling update.  Old nodes need the handler to
> understand messages from new nodes, and new nodes need it to understand
> messages from old nodes.

---

## 8. Error Handling

### Scenario: Incompatible Wire Versions

**What happens:** Node-A (wire v0.2) tries to connect to Node-B (wire v1.0).

```
Node-A → Node-B: HandshakeRequest { dactor_wire_version: "0.2.0", ... }
Node-B → Node-A: HandshakeResponse { accepted: false, rejection_reason: "incompatible wire version" }
```

**Behavior:**
- Connection is **rejected** — no `WireEnvelope` traffic flows.
- Node-B emits `ClusterEvent::NodeRejected { reason: IncompatibleProtocol }`.
- Node-A logs an error: `"Connection to node-b rejected: incompatible wire version 0.2.0 vs 1.0.0"`.
- Node-A does **not** retry (version won't change without redeployment).

### Scenario: Unknown Message Version

**What happens:** Node-A sends a `WireEnvelope` with `version: Some(5)` for
message type `myapp::Order`.  Node-B expects version 3 and has a handler
registered for `myapp::Order`.

```
1. Node-B receives envelope with version=5, expected=3
2. Calls OrderMigrator.migrate(payload, 5, 3)
3. OrderMigrator returns None (cannot downgrade 5→3)
4. Node-B returns SerializationError:
   "myapp::Order: cannot migrate from v5 to v3"
```

**For `tell` (fire-and-forget):**
- The message is dropped.
- An error is logged on the receiving node.
- The `on_error` lifecycle hook fires on the target actor.

**For `ask` (request-reply):**
- The sender receives an error reply with the `SerializationError`.
- The sender's future resolves to `Err(...)`.

### Scenario: Migration Handler Returns None

**What happens:** A `MessageVersionHandler` is registered but returns `None`
for the given version pair (e.g., it only handles 1→2 but receives 1→3).

**Behavior:**
- `SerializationError` with message:
  `"myapp::Order: cannot migrate from v1 to v3"`
- Same error propagation as unknown message version above.

**Mitigation:** Implement chained migration (1→2→3) in the handler, or
register separate handlers for each version pair.

### Scenario: Unknown Message Type

**What happens:** Node-A (new version) sends a message with type
`myapp::NewFeatureMsg`.  Node-B (old version) has no deserializer
registered for that type.

**Behavior:**
- `TypeRegistry::deserialize()` returns `SerializationError`:
  `"unknown message type: myapp::NewFeatureMsg"`
- Same error propagation as above (logged + error hook for tell, error
  reply for ask).

**Mitigation:** This is expected during rolling updates.  The sender should
handle the error gracefully (e.g., retry after the target node is upgraded).

### ClusterEvents for Each Scenario

| Scenario | ClusterEvent Emitted |
|----------|---------------------|
| Incompatible wire version | `NodeRejected { reason: IncompatibleProtocol }` |
| Incompatible adapter | `NodeRejected { reason: IncompatibleAdapter }` |
| Handshake timeout | `NodeRejected { reason: ConnectionFailed }` |
| System message deserialization failure | `NodeRejected { reason: ProtocolError }` |
| App version mismatch | *None* — accepted with log warning |
| Message version mismatch | *None* — handled at message level |
| Unknown message type | *None* — handled at message level |

### Scenario: System Message Deserialization Failure

**What happens:** Two nodes successfully connect (handshake passes) but a
system-level protobuf message (SpawnRequest, WatchRequest, CancelRequest,
ConnectPeer, etc.) fails to deserialize on the receiving side.

This can occur when:
- The handshake was corrupted or skipped (e.g., direct TCP without handshake)
- A minor wire version bump added new required fields to a system message
  but the receiver is on an older minor version that doesn't know about them
- Protobuf schema evolution went wrong (reused field numbers, changed types)

**Behavior:**

```
1. Node-A sends system WireEnvelope (e.g., SYSTEM_MSG_TYPE_SPAWN)
2. Node-B calls proto::decode_spawn_request(&envelope.body)
3. Deserialization fails → RoutingError returned
4. Node-B logs: "system message deserialization failed: decode SpawnRequest: ..."
5. Node-B emits ClusterEvent::NodeRejected {
       node_id: "node-a",
       reason: ProtocolError,
       detail: "system message deserialization failed"
   }
6. Node-B disconnects from Node-A
```

**Why this is Category 1:** System messages are the foundation of cluster
coordination. If they can't be deserialized, nodes cannot manage actors,
watches, or peer membership. The connection must be severed.

**Key distinction from application message failure:** Application message
deserialization failures (Category 2) are handled per-message — the actor
gets an error, the cluster continues. System message failures indicate
fundamental protocol incompatibility — the connection must be terminated.

**Implementation in SystemMessageRouter:**

```rust
async fn route_system_envelope(&self, envelope: WireEnvelope) 
    -> Result<RoutingOutcome, RoutingError> 
{
    // If ANY system message fails to decode, this is a protocol error.
    // The transport layer should:
    // 1. Log the error with full context
    // 2. Emit ClusterEvent::NodeRejected { reason: ProtocolError }
    // 3. Disconnect the peer
    // 4. NOT retry (protocol mismatch won't resolve without redeployment)
}
```

### Core dactor Protocol Version

The dactor framework maintains a **core protocol version** as a compile-time
constant. This version is separate from the crate version (`Cargo.toml`
version) — it tracks only changes to the wire format.

```rust
// In dactor/src/remote.rs (or dactor/src/lib.rs)

/// Core dactor protocol version for inter-node communication.
///
/// This version is included in the connect handshake and determines
/// whether two nodes can form a cluster.
///
/// ⚠️  WIRE PROTOCOL — increment carefully:
/// - MAJOR: breaking wire change → nodes cannot cluster (Category 1)
/// - MINOR: backward-compatible addition → nodes can cluster
/// - PATCH: no wire changes
///
/// This is NOT the crate version (Cargo.toml). A crate version bump
/// does not necessarily change the protocol version. Only changes to
/// WireEnvelope structure, system message protobuf schemas, or
/// handshake format require a protocol version bump.
pub const DACTOR_PROTOCOL_VERSION: (u32, u32, u32) = (0, 2, 0);

/// Check if two protocol versions are compatible for clustering.
pub fn protocol_compatible(local: (u32, u32, u32), remote: (u32, u32, u32)) -> bool {
    local.0 == remote.0  // same MAJOR version required
}
```

**What bumps each component:**

| Component | Bumped When | Example Change |
|-----------|------------|----------------|
| **MAJOR** | WireEnvelope structure changes, protobuf system message schema breaks, handshake format changes | New required field in WireEnvelope, removed system message type |
| **MINOR** | New optional protobuf field, new system message type, new ClusterEvent variant | Added `ConnectPeer.metadata` optional field |
| **PATCH** | Internal bug fixes, performance improvements, no wire changes | Fixed deserialization edge case |

**Relationship to other versions:**

```
dactor crate version (Cargo.toml):  0.2.0 → 0.2.1 → 0.3.0 → 1.0.0
                                      │         │        │        │
protocol version:                   0.2.0    0.2.0    0.2.1    1.0.0
                                    (same)   (patch)  (minor)  (MAJOR)

Node compatibility:                   ✅       ✅       ✅       ❌
                                   (identical) (same)  (compat) (Category 1)
```

A crate can have many releases (0.2.0, 0.2.1, 0.2.2) that all share
the same protocol version (0.2.0), as long as no wire format changes
are made. Only when the wire format actually changes does the protocol
version bump.

---

## 9. Testing Strategy

### Testing Category 1: Cluster Split

**Goal:** Verify that nodes with incompatible wire versions reject each
other cleanly.

```rust
#[tokio::test]
async fn incompatible_wire_versions_reject_connection() {
    // Cluster A: wire version "0.2.0"
    let cluster_a = TestCluster::builder()
        .wire_version("0.2.0")
        .nodes(2)
        .build()
        .await;

    // Cluster B: wire version "1.0.0"
    let cluster_b = TestCluster::builder()
        .wire_version("1.0.0")
        .nodes(2)
        .build()
        .await;

    // Attempt cross-connection
    let result = cluster_a.node(0).connect_to(cluster_b.node(0)).await;

    assert!(result.is_err());
    assert!(matches!(
        cluster_a.last_event(),
        Some(ClusterEvent::NodeRejected {
            reason: RejectionReason::IncompatibleProtocol,
            ..
        })
    ));

    // Both clusters still function internally
    cluster_a.assert_healthy().await;
    cluster_b.assert_healthy().await;
}
```

**Key test cases:**

- Two nodes with same MAJOR, different MINOR → accept
- Two nodes with different MAJOR → reject with `IncompatibleProtocol`
- Two nodes with different adapters → reject with `IncompatibleAdapter`
- Handshake timeout → reject with `ConnectionFailed`
- Rejected node does not appear in `ClusterState.nodes`

### Testing Category 2: Mixed-Version Nodes

**Goal:** Verify that nodes with the same wire version but different app
versions can communicate, and that `MessageVersionHandler` works correctly.

```rust
#[tokio::test]
async fn mixed_app_versions_with_migration() {
    let mut cluster = TestCluster::builder()
        .wire_version("0.2.0")
        .nodes(3)
        .build()
        .await;

    // Node 0: app v1 (sends PlaceOrder v1)
    cluster.node_mut(0).set_app_version("1.0.0");
    cluster.node_mut(0).set_message_version::<PlaceOrder>(1);

    // Node 1: app v2 (expects PlaceOrder v2)
    cluster.node_mut(1).set_app_version("2.0.0");
    cluster.node_mut(1).set_message_version::<PlaceOrder>(2);
    cluster.node_mut(1).register_version_handler(PlaceOrderMigrator);

    // Send v1 message from node 0 to node 1
    let result = cluster.node(0)
        .tell(target_on_node_1, PlaceOrderV1 { item_id: "abc".into(), quantity: 3 })
        .await;

    assert!(result.is_ok()); // Migration handled it

    // Verify the actor on node 1 received the migrated v2 message
    let received = cluster.node(1).actor_state::<OrderActor>(target_id).await;
    assert_eq!(received.last_order.priority, None); // default from migration
}
```

**Key test cases:**

- v1 message → v2 node with handler → success (migrated)
- v1 message → v2 node without handler → falls through to serde defaults
- v1 message → v3 node with handler (chained migration) → success
- v2 message → v1 node (downgrade) → handler returns None → error
- Unknown message type from new node → old node returns error
- New actor type spawn from old node → SpawnResponse::Error
- All nodes same version → no migration attempted (even if handler exists)

### CI Pipeline Recommendations

```yaml
# .github/workflows/version-compat.yml
name: Version Compatibility Tests
on: [push, pull_request]

jobs:
  compat-tests:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        scenario:
          - "same-version"          # all nodes same version
          - "minor-version-diff"    # v0.2.0 + v0.3.0
          - "app-version-diff"      # same wire, different app
          - "migration-v1-v2"       # MessageVersionHandler test
    steps:
      - uses: actions/checkout@v4
      - name: Run compat test
        run: |
          cargo test --test version_compat -- --scenario ${{ matrix.scenario }}
```

**Recommended test matrix:**

| Test | Category | What It Verifies |
|------|----------|-----------------|
| `test_same_version_cluster` | Baseline | Normal operation |
| `test_minor_wire_version_diff` | 1 | Minor version accepted |
| `test_major_wire_version_rejection` | 1 | Major version rejected |
| `test_rolling_restart_serde_defaults` | 2 | Optional field addition |
| `test_rolling_restart_with_handler` | 2 | MessageVersionHandler migration |
| `test_unknown_message_type` | 2 | Graceful error for new types |
| `test_handler_returns_none` | 2 | Migration failure error path |
| `test_chained_migration` | 2 | v1→v2→v3 handler chaining |

---

## 10. FAQ

### Q: Do I need to worry about this for single-node deployments?

**No.** Version compatibility only matters when multiple nodes communicate
over the network.  A single-node deployment has no remote `WireEnvelope`
traffic, so wire protocol versions, handshakes, and `MessageVersionHandler`s
are irrelevant.

### Q: Can I skip Category 1 if I only change app code?

**Yes, that's exactly Category 2.** If the dactor framework version and
adapter version don't change, the wire protocol is identical.  System
actors communicate normally.  Use a rolling restart and handle app-level
schema changes with serde defaults or `MessageVersionHandler`.

### Q: What if I change both dactor and app code at the same time?

**Category 1 takes precedence.** If the dactor wire protocol version
changes, you must do a cluster split (blue/green) regardless of app
changes.  The app changes ride along with the infrastructure change — they
don't add extra complexity since you're replacing the entire cluster anyway.

### Q: Do I need MessageVersionHandler for every message type?

**No.**  Only register handlers for message types with **breaking** schema
changes that can't be absorbed by serde defaults.  Adding optional fields
with `#[serde(default)]` doesn't require a handler.

### Q: What about adapter-to-adapter compatibility?

Nodes running different adapters (e.g., `dactor-ractor` and `dactor-kameo`)
**can potentially form a cluster** — the wire protocol is the same, only
the local actor runtime differs.  However, this is **not recommended** and
is rejected in the handshake by default (`IncompatibleAdapter`).  The
adapter field is included in the handshake for visibility.

### Q: How do I know which wire version I'm running?

Check the `DACTOR_WIRE_VERSION` constant:

```rust
println!("wire version: {}", dactor::DACTOR_WIRE_VERSION);
```

Or at runtime via `ClusterState`:

```rust
let state = runtime.cluster_state().await;
println!("wire: {}, app: {:?}", state.wire_version, state.app_version);
for (node, info) in &state.peer_versions {
    println!("  {}: wire={}, app={:?}", node, info.wire_version, info.app_version);
}
```

### Q: Can I use canary deployments with Category 2?

**Yes.** Since all nodes share the same wire protocol, you can route a
small percentage of traffic to canary nodes running the new app version.
The `MessageVersionHandler` handles any schema differences.  Monitor
deserialization error rates on the canary to validate before full rollout.

### Q: What if a node is mid-upgrade when it receives a message?

During the brief window while a node is restarting:

- **Incoming connections** — the node is unreachable; `NodeDirectory` marks
  it as `Unreachable` and eventually `Disconnected`.
- **In-flight messages** — any undelivered `WireEnvelope`s are lost (tell)
  or return a transport error (ask).
- **After restart** — the node rejoins the cluster via discovery, performs
  the handshake, and resumes normal operation.

This is the same behavior as any node restart — version compatibility
doesn't change the restart mechanics.

---

*Document generated for dactor PUB5: Rolling Upgrade & Version Compatibility Design.*
*See also: [Cluster Node Join/Leave Behavior](cluster-behavior.md) for node lifecycle details.*
