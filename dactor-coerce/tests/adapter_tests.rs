use std::time::Duration;

use dactor::actor::ActorRef;
use dactor::test_support::conformance::*;
use dactor_coerce::CoerceRuntime;

// ---------------------------------------------------------------------------
// Conformance tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_tell_and_ask() {
    let runtime = CoerceRuntime::new();
    test_tell_and_ask(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_message_ordering() {
    let runtime = CoerceRuntime::new();
    test_message_ordering(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_ask_reply() {
    let runtime = CoerceRuntime::new();
    test_ask_reply(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_stop() {
    let runtime = CoerceRuntime::new();
    test_stop(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_unique_ids() {
    let runtime = CoerceRuntime::new();
    test_unique_ids(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_actor_name() {
    let runtime = CoerceRuntime::new();
    test_actor_name(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_stream_items() {
    let runtime = CoerceRuntime::new();
    test_stream_items(|name, args| runtime.spawn::<ConformanceStreamer>(name, args)).await;
}

#[tokio::test]
async fn conformance_stream_empty() {
    let runtime = CoerceRuntime::new();
    test_stream_empty(|name, args| runtime.spawn::<ConformanceStreamer>(name, args)).await;
}

#[tokio::test]
async fn conformance_feed_sum() {
    let runtime = CoerceRuntime::new();
    test_feed_sum(|name, args| runtime.spawn::<ConformanceAggregator>(name, args)).await;
}

#[tokio::test]
async fn conformance_lifecycle_ordering() {
    let runtime = CoerceRuntime::new();
    test_lifecycle_ordering(|name, args| runtime.spawn::<ConformanceLifecycle>(name, args)).await;
}

#[tokio::test]
async fn conformance_cancel_ask() {
    let runtime = CoerceRuntime::new();
    test_cancel_ask(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_on_error_resume() {
    let runtime = CoerceRuntime::new();
    test_on_error_resume(|name, args| runtime.spawn::<ConformanceResumeActor>(name, args)).await;
}

// ---------------------------------------------------------------------------
// Coerce-specific tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_coerce_tell_ask() {
    let runtime = CoerceRuntime::new();
    let actor = runtime.spawn::<ConformanceCounter>("counter", 0);
    actor.tell(Increment(10)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 10);
}

#[tokio::test]
async fn test_coerce_stop() {
    let runtime = CoerceRuntime::new();
    let actor = runtime.spawn::<ConformanceCounter>("counter", 0);
    assert!(actor.is_alive());
    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive());
}

#[tokio::test]
async fn test_coerce_multiple_actors() {
    let runtime = CoerceRuntime::new();
    let a1 = runtime.spawn::<ConformanceCounter>("c1", 100);
    let a2 = runtime.spawn::<ConformanceCounter>("c2", 200);

    let v1 = a1.ask(GetCount, None).unwrap().await.unwrap();
    let v2 = a2.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(v1, 100);
    assert_eq!(v2, 200);
    assert_ne!(a1.id(), a2.id());
}

#[tokio::test]
async fn test_coerce_default_runtime() {
    let runtime = CoerceRuntime::default();
    let actor = runtime.spawn::<ConformanceCounter>("counter", 42);
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 42);
}
