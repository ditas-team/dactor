//! Integration tests for MessageVersionHandler migration scenarios.
//!
//! These tests exercise `receive_envelope_body_versioned()` with realistic
//! migration patterns: field addition, field rename, chained migration,
//! downgrade rejection, and handler-not-set pitfalls.
//!
//! See also: docs/message-version-handler-playbook.md

use std::collections::HashMap;

use dactor::interceptor::SendMode;
use dactor::node::{ActorId, NodeId};
use dactor::remote::{
    receive_envelope_body_versioned, MessageVersionHandler, WireEnvelope,
    WireHeaders,
};
use dactor::type_registry::TypeRegistry;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn make_envelope(message_type: &str, body: Vec<u8>, version: Option<u32>) -> WireEnvelope {
    WireEnvelope {
        target: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        target_name: "test-actor".into(),
        message_type: message_type.into(),
        send_mode: SendMode::Tell,
        headers: WireHeaders::new(),
        body,
        request_id: None,
        version,
    }
}

fn empty_handlers() -> HashMap<String, Box<dyn MessageVersionHandler>> {
    HashMap::new()
}

// ---------------------------------------------------------------------------
// Migration handler: simulates PlaceOrder v1 → v2 → v3
// ---------------------------------------------------------------------------

/// Simulates a PlaceOrder message migrator for testing.
///
/// v1 body: [item_id_len, item_id_bytes..., quantity_u32_be]
/// v2 body: [item_id_len, item_id_bytes..., quantity_u32_be, priority_u8]
/// v3 body: [item_id_len, item_id_bytes..., qty_u32_be, priority_u8]
///          (field renamed in schema; binary format unchanged for simplicity)
struct PlaceOrderMigrator;

impl MessageVersionHandler for PlaceOrderMigrator {
    fn message_type(&self) -> &'static str {
        "test::PlaceOrder"
    }

    fn migrate(&self, payload: &[u8], from_version: u32, to_version: u32) -> Option<Vec<u8>> {
        match (from_version, to_version) {
            (1, 2) => {
                // v1→v2: append a default priority byte (0)
                let mut v2 = payload.to_vec();
                v2.push(0); // priority = 0 (none)
                Some(v2)
            }
            (2, 3) => {
                // v2→v3: binary format unchanged (rename is schema-level)
                Some(payload.to_vec())
            }
            (1, 3) => {
                // Chain: v1→v2→v3
                let v2 = self.migrate(payload, 1, 2)?;
                self.migrate(&v2, 2, 3)
            }
            (2, 1) => {
                // Downgrade: not supported
                None
            }
            _ => None,
        }
    }
}

fn place_order_handlers() -> HashMap<String, Box<dyn MessageVersionHandler>> {
    let mut h: HashMap<String, Box<dyn MessageVersionHandler>> = HashMap::new();
    h.insert("test::PlaceOrder".into(), Box::new(PlaceOrderMigrator));
    h
}

// TypeRegistry that accepts any bytes as Box<Vec<u8>>
fn bytes_registry() -> TypeRegistry {
    let mut r = TypeRegistry::new();
    r.register("test::PlaceOrder", |bytes: &[u8]| {
        Ok(Box::new(bytes.to_vec()))
    });
    r.register("test::OtherMsg", |bytes: &[u8]| {
        Ok(Box::new(bytes.to_vec()))
    });
    r
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn v1_to_v2_migration_adds_priority_byte() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    // v1 body: item_id="AB" (len=2), quantity=10
    let v1_body = vec![2, b'A', b'B', 0, 0, 0, 10];
    let envelope = make_envelope("test::PlaceOrder", v1_body, Some(1));

    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(2)).unwrap();
    let body = result.downcast::<Vec<u8>>().unwrap();

    // v2 body should have priority byte appended
    assert_eq!(*body, vec![2, b'A', b'B', 0, 0, 0, 10, 0]);
}

#[test]
fn v2_to_v3_migration_passes_through() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let v2_body = vec![2, b'A', b'B', 0, 0, 0, 10, 5];
    let envelope = make_envelope("test::PlaceOrder", v2_body.clone(), Some(2));

    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(3)).unwrap();
    let body = result.downcast::<Vec<u8>>().unwrap();

    // v2→v3 is a schema-level rename; binary format unchanged
    assert_eq!(*body, v2_body);
}

#[test]
fn chained_v1_to_v3_migration() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let v1_body = vec![2, b'A', b'B', 0, 0, 0, 10];
    let envelope = make_envelope("test::PlaceOrder", v1_body, Some(1));

    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(3)).unwrap();
    let body = result.downcast::<Vec<u8>>().unwrap();

    // v1→v2 adds priority, v2→v3 passes through
    assert_eq!(*body, vec![2, b'A', b'B', 0, 0, 0, 10, 0]);
}

#[test]
fn downgrade_v2_to_v1_rejected() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let v2_body = vec![2, b'A', b'B', 0, 0, 0, 10, 5];
    let envelope = make_envelope("test::PlaceOrder", v2_body, Some(2));

    let result = receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(1));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message.contains("cannot migrate from v2 to v1"),
        "error: {}",
        err.message
    );
}

#[test]
fn same_version_skips_handler() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let body = vec![1, 2, 3];
    let envelope = make_envelope("test::PlaceOrder", body.clone(), Some(2));

    // Same version — handler should not be called
    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(2)).unwrap();
    let received = result.downcast::<Vec<u8>>().unwrap();
    assert_eq!(*received, body);
}

#[test]
fn no_handler_registered_falls_through() {
    let registry = bytes_registry();
    let handlers = empty_handlers(); // no handlers

    let body = vec![1, 2, 3];
    let envelope = make_envelope("test::PlaceOrder", body.clone(), Some(1));

    // Version mismatch but no handler — uses body as-is (serde defaults)
    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(2)).unwrap();
    let received = result.downcast::<Vec<u8>>().unwrap();
    assert_eq!(*received, body);
}

#[test]
fn sender_version_none_skips_handler() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let body = vec![1, 2, 3];
    let envelope = make_envelope("test::PlaceOrder", body.clone(), None); // no version

    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(2)).unwrap();
    let received = result.downcast::<Vec<u8>>().unwrap();
    assert_eq!(*received, body);
}

#[test]
fn receiver_version_none_skips_handler() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let body = vec![1, 2, 3];
    let envelope = make_envelope("test::PlaceOrder", body.clone(), Some(1));

    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, None).unwrap();
    let received = result.downcast::<Vec<u8>>().unwrap();
    assert_eq!(*received, body);
}

#[test]
fn unknown_message_type_with_version_mismatch_falls_through() {
    let registry = bytes_registry();
    let handlers = place_order_handlers(); // handler for PlaceOrder, not OtherMsg

    let body = vec![42];
    let envelope = make_envelope("test::OtherMsg", body.clone(), Some(1));

    // OtherMsg has no handler — version mismatch falls through
    let result =
        receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(2)).unwrap();
    let received = result.downcast::<Vec<u8>>().unwrap();
    assert_eq!(*received, body);
}

#[test]
fn unknown_version_pair_in_handler_rejected() {
    let registry = bytes_registry();
    let handlers = place_order_handlers();

    let body = vec![1, 2, 3];
    let envelope = make_envelope("test::PlaceOrder", body, Some(5)); // v5 → v2: unknown

    let result = receive_envelope_body_versioned(&envelope, &registry, &handlers, Some(2));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("cannot migrate from v5 to v2"));
}
