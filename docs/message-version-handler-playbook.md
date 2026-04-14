# MessageVersionHandler Playbook

A task-oriented guide for handling message schema changes during rolling
upgrades. For the conceptual background and compatibility model, see
[Version Compatibility & Rolling Upgrades](version-compatibility.md).

---

## When Do You Need a Handler?

| Scenario | Approach | Handler needed? |
|----------|----------|-----------------|
| Added optional field | `#[serde(default)]` | **No** |
| Added required field with sensible default | `#[serde(default = "...")]` | **No** |
| Renamed field | `#[serde(alias = "old_name")]` or handler | Maybe |
| Changed field type (e.g. `u32` → `u64`) | Handler: parse old, convert, re-serialize | **Yes** |
| Removed field | Handler: inject placeholder in byte stream | **Yes** |
| Complex structural change | Handler: full v1→v2 transformation | **Yes** |

**Rule of thumb:** if serde can absorb the change with attributes alone,
you don't need a handler. Use a handler only for changes that would cause
deserialization failure on the receiving node.

---

## Common Pitfalls

### ⚠️ Pitfall 1: Version Not Set → Handler Never Runs

The `MessageVersionHandler` is **only invoked** when all three conditions are true:

1. The sender sets a concrete version on the `WireEnvelope` (`version: Some(N)`)
2. The receiver specifies an expected version (`expected_version: Some(M)`)
3. The two versions differ (`N ≠ M`)

If either side uses `None`, no migration is attempted — the handler is
silently skipped and the body is deserialized as-is.

**Common mistake:** implementing a handler but forgetting to set
`version: Some(N)` on outgoing envelopes. The convenience builders
(`build_tell_envelope`, `build_ask_envelope`) **do not accept a version
parameter** and always set `version: None`. Use `build_wire_envelope`
instead, or set `envelope.version` after construction.

```rust
use dactor::remote::build_wire_envelope;
use dactor::interceptor::SendMode;

// ❌ Wrong — build_tell_envelope always sets version: None
let envelope = build_tell_envelope(target, name, &msg, headers)?;

// ✅ Correct — build_wire_envelope accepts version parameter
let envelope = build_wire_envelope(
    target, name, &msg, SendMode::Tell, headers, None, Some(2),
)?;
```

### ⚠️ Pitfall 2: Message Type Name Must Be Stable

Handler dispatch is keyed by `WireEnvelope.message_type` — the string
registered in the version handler map. If a message type is renamed or
moved to a different Rust module path, the handler lookup will miss.

**Important:** the convenience builders (`build_tell_envelope`,
`build_ask_envelope`, `build_wire_envelope`) all set `message_type` to
`std::any::type_name::<M>()`. Your handler's `message_type()` string
must match this exactly for lookup to succeed. If you use a custom
string, you must also construct the `WireEnvelope` with the same string.

```rust
fn message_type(&self) -> &'static str {
    // Must match std::any::type_name::<PlaceOrder>() if using standard builders
    "myapp::orders::PlaceOrder"
}
```

If you rename the Rust struct or move it to a different module, the
`type_name` changes and the handler stops matching. Either keep the
type path stable or register the handler under the new path.

### ⚠️ Pitfall 3: Handler Returns `None` → Hard Error

If `migrate()` returns `None`, the framework treats this as an
unrecoverable failure and returns a `SerializationError`. Don't return
`None` for "I'll try my best" — return `None` only when migration is
truly impossible (e.g., downgrade from v2 to v1 when data was lost).

---

## Step-by-Step: Adding a MessageVersionHandler

### Step 1: Define the Handler

```rust
use dactor::MessageVersionHandler;

struct PlaceOrderMigrator;

impl MessageVersionHandler for PlaceOrderMigrator {
    fn message_type(&self) -> &'static str {
        "myapp::orders::PlaceOrder"
    }

    fn migrate(
        &self,
        payload: &[u8],
        from_version: u32,
        to_version: u32,
    ) -> Option<Vec<u8>> {
        match (from_version, to_version) {
            (1, 2) => self.v1_to_v2(payload),
            (2, 3) => self.v2_to_v3(payload),
            (1, 3) => {
                // Chain: v1 → v2 → v3
                let v2 = self.v1_to_v2(payload)?;
                self.v2_to_v3(&v2)
            }
            _ => None, // Unknown version pair
        }
    }
}
```

### Step 2: Implement Migration Logic

```rust
impl PlaceOrderMigrator {
    /// v1 → v2: add optional "priority" field
    fn v1_to_v2(&self, payload: &[u8]) -> Option<Vec<u8>> {
        let mut v: serde_json::Value = serde_json::from_slice(payload).ok()?;
        let obj = v.as_object_mut()?;
        obj.entry("priority").or_insert(serde_json::Value::Null);
        serde_json::to_vec(&v).ok()
    }

    /// v2 → v3: rename "quantity" → "qty"
    fn v2_to_v3(&self, payload: &[u8]) -> Option<Vec<u8>> {
        let mut v: serde_json::Value = serde_json::from_slice(payload).ok()?;
        let obj = v.as_object_mut()?;
        if let Some(qty) = obj.remove("quantity") {
            obj.insert("qty".into(), qty);
        }
        serde_json::to_vec(&v).ok()
    }
}
```

### Step 3: Register the Handler

```rust
use std::collections::HashMap;
use dactor::MessageVersionHandler;

let mut version_handlers: HashMap<String, Box<dyn MessageVersionHandler>> =
    HashMap::new();

let migrator = PlaceOrderMigrator;
version_handlers.insert(
    migrator.message_type().to_string(),
    Box::new(migrator),
);
```

### Step 4: Set Version on Outgoing Envelopes

Use `build_wire_envelope` (not the convenience builders, which hardcode
`version: None`):

```rust
use dactor::remote::build_wire_envelope;
use dactor::interceptor::SendMode;

let envelope = build_wire_envelope(
    target_id,
    "order-processor",
    &place_order_msg,
    SendMode::Tell,
    headers,
    None,       // request_id
    Some(2),    // ← message version (v2)
)?;
```

### Step 5: Receive with Version-Aware Deserialization

```rust
use dactor::remote::receive_envelope_body_versioned;

let result = receive_envelope_body_versioned(
    &incoming_envelope,
    &type_registry,
    &version_handlers,
    Some(2),    // ← this node expects v2
)?;
```

---

## Example Scenarios

### Scenario 1: Add Optional Field (No Handler Needed)

```rust
// v1
#[derive(Serialize, Deserialize)]
struct PlaceOrder {
    item_id: String,
    quantity: u32,
}

// v2 — just add #[serde(default)]
#[derive(Serialize, Deserialize)]
struct PlaceOrder {
    item_id: String,
    quantity: u32,
    #[serde(default)]
    priority: Option<String>,
}
```

A v1 message deserialized as v2 will have `priority: None`. No handler needed.

### Scenario 2: Rename Field (Handler or Serde Alias)

**Option A — Serde alias (simpler, but one-way):**
```rust
#[derive(Serialize, Deserialize)]
struct PlaceOrder {
    item_id: String,
    #[serde(alias = "quantity")]
    qty: u32,
}
```

> **⚠️ Rolling upgrade caveat:** `alias` only helps the new receiver accept
> old payloads. New nodes serialize as `"qty"`, so old nodes (expecting
> `"quantity"`) will fail. Use a handler for bidirectional compatibility
> during mixed-version windows, or ensure all old nodes are upgraded
> before new nodes start sending.

**Option B — Handler (when alias won't work):**
```rust
fn migrate(&self, payload: &[u8], from: u32, to: u32) -> Option<Vec<u8>> {
    if from == 1 && to == 2 {
        let mut v: serde_json::Value = serde_json::from_slice(payload).ok()?;
        let obj = v.as_object_mut()?;
        if let Some(val) = obj.remove("quantity") {
            obj.insert("qty".into(), val);
        }
        return serde_json::to_vec(&v).ok();
    }
    None
}
```

### Scenario 3: Chained Migration (v1 → v3)

When a node running v3 receives a message from a v1 node, the handler
can chain intermediate migrations:

```rust
(1, 3) => {
    let v2 = self.migrate(payload, 1, 2)?;
    self.migrate(&v2, 2, 3)
}
```

This reuses the individual step handlers and guarantees consistency.

### Scenario 4: Downgrade Rejection

When a new node sends a v2 message to an old node expecting v1, and the
migration is not reversible:

```rust
(2, 1) => None,  // Cannot downgrade — data would be lost
```

The receiving node gets a `SerializationError` with a clear message:
`"myapp::orders::PlaceOrder: cannot migrate from v2 to v1"`.

---

## Best Practices

1. **Bump version only for breaking schema changes.** If serde defaults
   handle it, don't bump.

2. **Keep `message_type()` strings stable.** Never tie them to Rust
   module paths that might change during refactoring.

3. **Test migration in both directions.** Even if downgrade returns
   `None`, test that the error is clear.

4. **Chain intermediate migrations** rather than writing direct v1→v3
   transforms. This reduces maintenance and ensures consistency.

5. **Deploy handlers before bumping the version.** During a rolling
   upgrade, old nodes should already have the handler registered before
   new nodes start sending the new version.

6. **Set `version` on all envelopes** for message types that have
   handlers. If you forget, the handler is silently skipped.

7. **Log migration events** in production. Track how many messages are
   being migrated to monitor rollout progress.

---

## Testing Your Handler

Use `receive_envelope_body_versioned()` directly in unit tests.
See `dactor/tests/version_migration_tests.rs` for complete working
examples. Here's the pattern:

```rust
use dactor::remote::{receive_envelope_body_versioned, WireEnvelope, WireHeaders};
use dactor::interceptor::SendMode;
use dactor::node::{ActorId, NodeId};
use dactor::type_registry::TypeRegistry;

#[test]
fn test_v1_to_v2_migration() {
    let mut registry = TypeRegistry::new();
    registry.register("myapp::PlaceOrder", |bytes| {
        let order: PlaceOrderV2 = serde_json::from_slice(bytes)
            .map_err(|e| dactor::SerializationError::new(e.to_string()))?;
        Ok(Box::new(order))
    });

    let mut handlers: HashMap<String, Box<dyn MessageVersionHandler>> =
        HashMap::new();
    handlers.insert(
        "myapp::PlaceOrder".into(),
        Box::new(PlaceOrderMigrator),
    );

    let v1_json = br#"{"item_id":"abc","quantity":3}"#;
    let envelope = WireEnvelope {
        target: ActorId { node: NodeId("n".into()), local: 1 },
        target_name: "order-processor".into(),
        message_type: "myapp::PlaceOrder".into(),
        send_mode: SendMode::Tell,
        headers: WireHeaders::new(),
        body: v1_json.to_vec(),
        request_id: None,
        version: Some(1),
    };

    let result = receive_envelope_body_versioned(
        &envelope,
        &registry,
        &handlers,
        Some(2),
    ).unwrap();

    let order = result.downcast::<PlaceOrderV2>().unwrap();
    assert_eq!(order.item_id, "abc");
    assert_eq!(order.quantity, 3);
    assert_eq!(order.priority, None); // default from migration
}
```

---

*See also: [Version Compatibility & Rolling Upgrades](version-compatibility.md)
for the full conceptual model, deployment guides, and testing strategy.*
