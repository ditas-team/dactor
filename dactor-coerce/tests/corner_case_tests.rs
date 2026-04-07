//! Corner-case conformance tests (T7–T10) for the dactor-coerce adapter.
//!
//! Wires the cross-adapter conformance functions from `dactor::test_support::conformance`
//! to the coerce runtime, following the same pattern as the ractor adapter.

use dactor::test_support::conformance::*;
use dactor_coerce::CoerceRuntime;

// ---------------------------------------------------------------------------
// T7 — message ordering under load (100 messages)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_message_ordering_under_load() {
    let runtime = CoerceRuntime::new();
    test_message_ordering_under_load(|name, init| runtime.spawn::<ConformanceCounter>(name, init))
        .await;
}

// ---------------------------------------------------------------------------
// T8 — send to stopped actor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_tell_after_stop() {
    let runtime = CoerceRuntime::new();
    test_tell_after_stop(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_ask_after_stop() {
    let runtime = CoerceRuntime::new();
    test_ask_after_stop(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

// ---------------------------------------------------------------------------
// T9 — concurrent asks from multiple callers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_concurrent_asks() {
    let runtime = CoerceRuntime::new();
    test_concurrent_asks(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

// ---------------------------------------------------------------------------
// T10 — stream with slow consumer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_stream_slow_consumer() {
    let runtime = CoerceRuntime::new();
    test_stream_slow_consumer(|name, init| runtime.spawn::<ConformanceStreamer>(name, init)).await;
}

// ---------------------------------------------------------------------------
// Bonus — rapid stop and send
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_rapid_stop_and_send() {
    let runtime = CoerceRuntime::new();
    test_rapid_stop_and_send(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}
