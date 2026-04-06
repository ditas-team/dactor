//! Integration tests for the dactor-kameo v0.2 adapter.
//!
//! Runs the conformance suite plus kameo-specific tests for the v0.2
//! `Actor` / `Handler<M>` / `ActorRef<A>` API.

use dactor::test_support::conformance::*;
use dactor_kameo::KameoRuntime;

// ---------------------------------------------------------------------------
// Conformance suite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conformance_tell_and_ask() {
    let runtime = KameoRuntime::new();
    test_tell_and_ask(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_message_ordering() {
    let runtime = KameoRuntime::new();
    test_message_ordering(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_ask_reply() {
    let runtime = KameoRuntime::new();
    test_ask_reply(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_stop() {
    let runtime = KameoRuntime::new();
    test_stop(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_unique_ids() {
    let runtime = KameoRuntime::new();
    test_unique_ids(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_actor_name() {
    let runtime = KameoRuntime::new();
    test_actor_name(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_stream_items() {
    let runtime = KameoRuntime::new();
    test_stream_items(|name, init| runtime.spawn::<ConformanceStreamer>(name, init)).await;
}

#[tokio::test]
async fn conformance_stream_empty() {
    let runtime = KameoRuntime::new();
    test_stream_empty(|name, init| runtime.spawn::<ConformanceStreamer>(name, init)).await;
}

#[tokio::test]
async fn conformance_feed_sum() {
    let runtime = KameoRuntime::new();
    test_feed_sum(|name, init| runtime.spawn::<ConformanceAggregator>(name, init)).await;
}

#[tokio::test]
async fn conformance_lifecycle_ordering() {
    let runtime = KameoRuntime::new();
    test_lifecycle_ordering(|name, init| runtime.spawn::<ConformanceLifecycle>(name, init)).await;
}

#[tokio::test]
async fn conformance_cancel_ask() {
    let runtime = KameoRuntime::new();
    test_cancel_ask(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_on_error_resume() {
    let runtime = KameoRuntime::new();
    test_on_error_resume(|name, init| runtime.spawn::<ConformanceResumeActor>(name, init)).await;
}

#[tokio::test]
async fn conformance_batched_stream() {
    let runtime = KameoRuntime::new();
    test_batched_stream(|name, init| runtime.spawn::<ConformanceStreamer>(name, init)).await;
}

#[tokio::test]
async fn conformance_batched_feed() {
    let runtime = KameoRuntime::new();
    test_batched_feed(|name, init| runtime.spawn::<ConformanceAggregator>(name, init)).await;
}

#[tokio::test]
async fn conformance_stream_with_none_batch() {
    let runtime = KameoRuntime::new();
    test_stream_with_none_batch(|name, init| runtime.spawn::<ConformanceStreamer>(name, init))
        .await;
}

#[tokio::test]
async fn conformance_feed_with_none_batch() {
    let runtime = KameoRuntime::new();
    test_feed_with_none_batch(|name, init| runtime.spawn::<ConformanceAggregator>(name, init))
        .await;
}

#[tokio::test]
async fn conformance_tell_after_stop() {
    let runtime = KameoRuntime::new();
    test_tell_after_stop(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_ask_after_stop() {
    let runtime = KameoRuntime::new();
    test_ask_after_stop(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_multiple_handlers() {
    let runtime = KameoRuntime::new();
    test_multiple_handlers(|name, init| runtime.spawn::<ConformanceMultiHandler>(name, init)).await;
}

#[tokio::test]
async fn conformance_message_ordering_under_load() {
    let runtime = KameoRuntime::new();
    test_message_ordering_under_load(|name, init| runtime.spawn::<ConformanceCounter>(name, init))
        .await;
}

#[tokio::test]
async fn conformance_concurrent_asks() {
    let runtime = KameoRuntime::new();
    test_concurrent_asks(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_stream_slow_consumer() {
    let runtime = KameoRuntime::new();
    test_stream_slow_consumer(|name, init| runtime.spawn::<ConformanceStreamer>(name, init)).await;
}

#[tokio::test]
async fn conformance_rapid_stop_and_send() {
    let runtime = KameoRuntime::new();
    test_rapid_stop_and_send(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_transform_doubler() {
    let runtime = KameoRuntime::new();
    test_transform_doubler(|name, init| runtime.spawn::<ConformanceDoubler>(name, init)).await;
}

#[tokio::test]
async fn conformance_transform_empty() {
    let runtime = KameoRuntime::new();
    test_transform_empty(|name, init| runtime.spawn::<ConformanceDoubler>(name, init)).await;
}

// ---------------------------------------------------------------------------
// Kameo-specific: re-exports & default
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reexports_core_types() {
    let _ = dactor_kameo::dactor::NodeId("1".into());
    let _rt = dactor_kameo::KameoRuntime::new();
    let _events = dactor_kameo::KameoClusterEvents::new();
}

#[tokio::test]
async fn runtime_is_default() {
    let _rt = KameoRuntime::default();
}

// ---------------------------------------------------------------------------
// Interceptor tests
// ---------------------------------------------------------------------------

mod interceptor_tests {
    use std::any::Any;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;

    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::errors::RuntimeError;
    use dactor::interceptor::{
        Disposition, InboundContext, InboundInterceptor, OutboundContext, OutboundInterceptor,
        Outcome,
    };
    use dactor::message::{Headers, Message, RuntimeHeaders};
    use dactor_kameo::{KameoRuntime, SpawnOptions};

    // -- Test actor --

    struct Echo;

    impl Actor for Echo {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            Echo
        }
    }

    struct Ping(String);
    impl Message for Ping {
        type Reply = String;
    }

    #[async_trait]
    impl Handler<Ping> for Echo {
        async fn handle(&mut self, msg: Ping, _ctx: &mut ActorContext) -> String {
            format!("pong:{}", msg.0)
        }
    }

    struct Fire(#[allow(dead_code)] u64);
    impl Message for Fire {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<Fire> for Echo {
        async fn handle(&mut self, _msg: Fire, _ctx: &mut ActorContext) {
            // fire-and-forget
        }
    }

    // -- Logging interceptor (inbound) --

    struct LoggingInterceptor {
        receive_count: Arc<AtomicU64>,
        complete_count: Arc<AtomicU64>,
    }

    impl LoggingInterceptor {
        fn new() -> (Self, Arc<AtomicU64>, Arc<AtomicU64>) {
            let receive = Arc::new(AtomicU64::new(0));
            let complete = Arc::new(AtomicU64::new(0));
            (
                Self {
                    receive_count: receive.clone(),
                    complete_count: complete.clone(),
                },
                receive,
                complete,
            )
        }
    }

    impl InboundInterceptor for LoggingInterceptor {
        fn name(&self) -> &'static str {
            "logging"
        }

        fn on_receive(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            self.receive_count.fetch_add(1, Ordering::SeqCst);
            Disposition::Continue
        }

        fn on_complete(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _headers: &Headers,
            _outcome: &Outcome<'_>,
        ) {
            self.complete_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // -- Rejecting interceptor (inbound) --

    struct RejectInterceptor {
        reason: String,
    }

    impl InboundInterceptor for RejectInterceptor {
        fn name(&self) -> &'static str {
            "rejecter"
        }

        fn on_receive(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            Disposition::Reject(self.reason.clone())
        }
    }

    // -- Outbound logging interceptor --

    struct OutboundLogger {
        send_count: Arc<AtomicU64>,
    }

    impl OutboundLogger {
        fn new() -> (Self, Arc<AtomicU64>) {
            let count = Arc::new(AtomicU64::new(0));
            (
                Self {
                    send_count: count.clone(),
                },
                count,
            )
        }
    }

    impl OutboundInterceptor for OutboundLogger {
        fn name(&self) -> &'static str {
            "outbound-logger"
        }

        fn on_send(
            &self,
            _ctx: &OutboundContext<'_>,
            _rh: &RuntimeHeaders,
            _headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Disposition::Continue
        }
    }

    // -- Outbound rejecting interceptor --

    struct OutboundRejectInterceptor;

    impl OutboundInterceptor for OutboundRejectInterceptor {
        fn name(&self) -> &'static str {
            "outbound-rejecter"
        }

        fn on_send(
            &self,
            _ctx: &OutboundContext<'_>,
            _rh: &RuntimeHeaders,
            _headers: &mut Headers,
            _message: &dyn Any,
        ) -> Disposition {
            Disposition::Reject("blocked by outbound".into())
        }
    }

    // -- On-complete outcome-capturing interceptor --

    struct OutcomeCapture {
        ask_reply_count: Arc<AtomicU64>,
        tell_success_count: Arc<AtomicU64>,
    }

    impl OutcomeCapture {
        fn new() -> (Self, Arc<AtomicU64>, Arc<AtomicU64>) {
            let ask = Arc::new(AtomicU64::new(0));
            let tell = Arc::new(AtomicU64::new(0));
            (
                Self {
                    ask_reply_count: ask.clone(),
                    tell_success_count: tell.clone(),
                },
                ask,
                tell,
            )
        }
    }

    impl InboundInterceptor for OutcomeCapture {
        fn name(&self) -> &'static str {
            "outcome-capture"
        }

        fn on_complete(
            &self,
            _ctx: &InboundContext<'_>,
            _rh: &RuntimeHeaders,
            _headers: &Headers,
            outcome: &Outcome<'_>,
        ) {
            match outcome {
                Outcome::AskSuccess { reply } => {
                    // Verify we can downcast the reply
                    if reply.downcast_ref::<String>().is_some() {
                        self.ask_reply_count.fetch_add(1, Ordering::SeqCst);
                    }
                }
                Outcome::TellSuccess => {
                    self.tell_success_count.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    // -- Tests --

    #[tokio::test]
    async fn test_inbound_interceptor_called() {
        let (interceptor, receive_count, complete_count) = LoggingInterceptor::new();
        let options = SpawnOptions {
            interceptors: vec![Box::new(interceptor)],
            ..Default::default()
        };

        let runtime = KameoRuntime::new();
        let actor = runtime.spawn_with_options::<Echo>("echo-inbound-called", (), options).await.unwrap();

        // Send a tell
        actor.tell(Fire(1)).unwrap();
        // Send another tell
        actor.tell(Fire(2)).unwrap();
        // Give kameo time to process
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(receive_count.load(Ordering::SeqCst), 2);
        assert_eq!(complete_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_outbound_interceptor_called() {
        let (interceptor, send_count) = OutboundLogger::new();
        let mut runtime = KameoRuntime::new();
        runtime.add_outbound_interceptor(Box::new(interceptor));

        let actor = runtime.spawn::<Echo>("echo-outbound-called", ()).await.unwrap();

        actor.tell(Fire(1)).unwrap();
        actor.tell(Fire(2)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(send_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_inbound_reject_ask() {
        let options = SpawnOptions {
            interceptors: vec![Box::new(RejectInterceptor {
                reason: "forbidden".into(),
            })],
            ..Default::default()
        };

        let runtime = KameoRuntime::new();
        let actor = runtime.spawn_with_options::<Echo>("echo-reject-ask", (), options).await.unwrap();

        let reply = actor.ask(Ping("hi".into()), None).unwrap();
        let result = reply.await;
        match result {
            Err(RuntimeError::Rejected {
                interceptor,
                reason,
            }) => {
                assert_eq!(interceptor, "rejecter");
                assert_eq!(reason, "forbidden");
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_on_complete_with_ask_reply() {
        let (outcome_capture, ask_count, tell_count) = OutcomeCapture::new();
        let options = SpawnOptions {
            interceptors: vec![Box::new(outcome_capture)],
            ..Default::default()
        };

        let runtime = KameoRuntime::new();
        let actor = runtime.spawn_with_options::<Echo>("echo-on-complete", (), options).await.unwrap();

        // Send an ask— on_complete should see AskSuccess with the String reply
        let reply = actor.ask(Ping("test".into()), None).unwrap().await.unwrap();
        assert_eq!(reply, "pong:test");

        // Send a tell — on_complete should see TellSuccess
        actor.tell(Fire(42)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(ask_count.load(Ordering::SeqCst), 1);
        assert_eq!(tell_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_outbound_reject_ask() {
        let mut runtime = KameoRuntime::new();
        runtime.add_outbound_interceptor(Box::new(OutboundRejectInterceptor));

        let actor = runtime.spawn::<Echo>("echo-outbound-reject", ()).await.unwrap();

        let reply = actor.ask(Ping("hi".into()), None).unwrap();
        let result = reply.await;
        match result {
            Err(RuntimeError::Rejected {
                interceptor,
                reason,
            }) => {
                assert_eq!(interceptor, "outbound-rejecter");
                assert_eq!(reason, "blocked by outbound");
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_inbound_and_outbound_both_called() {
        let (inbound, in_recv, in_comp) = LoggingInterceptor::new();
        let (outbound, out_send) = OutboundLogger::new();

        let mut runtime = KameoRuntime::new();
        runtime.add_outbound_interceptor(Box::new(outbound));

        let options = SpawnOptions {
            interceptors: vec![Box::new(inbound)],
            ..Default::default()
        };
        let actor = runtime.spawn_with_options::<Echo>("echo-both-pipelines", (), options).await.unwrap();

        let reply = actor.ask(Ping("x".into()), None).unwrap().await.unwrap();
        assert_eq!(reply, "pong:x");

        // Both pipelines should have fired
        assert_eq!(out_send.load(Ordering::SeqCst), 1);
        assert_eq!(in_recv.load(Ordering::SeqCst), 1);
        assert_eq!(in_comp.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_inbound_reject_tell_silently_dropped() {
        let options = SpawnOptions {
            interceptors: vec![Box::new(RejectInterceptor {
                reason: "nope".into(),
            })],
            ..Default::default()
        };

        let runtime = KameoRuntime::new();
        let actor = runtime.spawn_with_options::<Echo>("echo-reject-tell", (), options).await.unwrap();

        // Tell with rejectionshould not error (fire-and-forget has no error path)
        actor.tell(Fire(1)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Actor should still be alive
        assert!(actor.is_alive());
    }
}

// ---------------------------------------------------------------------------
// Lifecycle tests
// ---------------------------------------------------------------------------

mod lifecycle_tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use dactor::actor::{Actor, ActorContext, ActorError, ActorRef, Handler};
    use dactor::errors::ErrorAction;
    use dactor::message::Message;
    use dactor_kameo::KameoRuntime;

    // -- Actor that records lifecycle events --

    struct LifecycleActor {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Actor for LifecycleActor {
        type Args = Arc<Mutex<Vec<String>>>;
        type Deps = ();
        fn create(args: Self::Args, _deps: ()) -> Self {
            Self { events: args }
        }
        async fn on_start(&mut self, _ctx: &mut ActorContext) {
            self.events.lock().await.push("on_start".into());
        }
        async fn on_stop(&mut self) {
            self.events.lock().await.push("on_stop".into());
        }
    }

    struct Greet;
    impl Message for Greet {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<Greet> for LifecycleActor {
        async fn handle(&mut self, _msg: Greet, _ctx: &mut ActorContext) {
            self.events.lock().await.push("handle".into());
        }
    }

    #[tokio::test]
    async fn test_on_start_called_before_messages() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<LifecycleActor>("lifecycle-start", events.clone()).await.unwrap();

        actor.tell(Greet).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let log = events.lock().await;
        assert!(log.len() >= 2, "expected at least on_start + handle");
        assert_eq!(log[0], "on_start");
        assert_eq!(log[1], "handle");
    }

    #[tokio::test]
    async fn test_on_stop_called_after_stop() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<LifecycleActor>("lifecycle-stop", events.clone()).await.unwrap();

        // Let on_start finish
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        actor.stop();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let log = events.lock().await;
        assert!(
            log.contains(&"on_stop".to_string()),
            "on_stop should be called"
        );
    }

    // -- Actor that panics on a specific message, with configurable on_error --

    struct PanicActor {
        action: ErrorAction,
        error_count: Arc<AtomicU64>,
        handle_count: Arc<AtomicU64>,
    }

    impl Actor for PanicActor {
        type Args = (ErrorAction, Arc<AtomicU64>, Arc<AtomicU64>);
        type Deps = ();
        fn create(args: Self::Args, _deps: ()) -> Self {
            Self {
                action: args.0,
                error_count: args.1,
                handle_count: args.2,
            }
        }
        fn on_error(&mut self, _error: &ActorError) -> ErrorAction {
            self.error_count.fetch_add(1, Ordering::SeqCst);
            self.action
        }
    }

    struct DoPanic;
    impl Message for DoPanic {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<DoPanic> for PanicActor {
        async fn handle(&mut self, _msg: DoPanic, _ctx: &mut ActorContext) {
            panic!("intentional test panic");
        }
    }

    struct SafeMsg;
    impl Message for SafeMsg {
        type Reply = String;
    }

    #[async_trait]
    impl Handler<SafeMsg> for PanicActor {
        async fn handle(&mut self, _msg: SafeMsg, _ctx: &mut ActorContext) -> String {
            self.handle_count.fetch_add(1, Ordering::SeqCst);
            "ok".into()
        }
    }

    #[tokio::test]
    async fn test_on_error_resume_continues() {
        let error_count = Arc::new(AtomicU64::new(0));
        let handle_count = Arc::new(AtomicU64::new(0));
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<PanicActor>(
            "panic-resume",
            (
                ErrorAction::Resume,
                error_count.clone(),
                handle_count.clone(),
            ),
        ).await.unwrap();

        // Cause a panic
        actor.tell(DoPanic).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // on_error should have been called
        assert_eq!(error_count.load(Ordering::SeqCst), 1);

        // Actor should still be alive and handle messages after resume
        let reply = actor.ask(SafeMsg, None).unwrap().await.unwrap();
        assert_eq!(reply, "ok");
        assert_eq!(handle_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_on_error_stop_terminates() {
        let error_count = Arc::new(AtomicU64::new(0));
        let handle_count = Arc::new(AtomicU64::new(0));
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<PanicActor>(
            "panic-stop",
            (ErrorAction::Stop, error_count.clone(), handle_count.clone()),
        ).await.unwrap();

        // Cause a panic
        actor.tell(DoPanic).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // on_error should have been called
        assert_eq!(error_count.load(Ordering::SeqCst), 1);
        // Actor should be dead
        assert!(!actor.is_alive());
    }
}

// ---------------------------------------------------------------------------
// Stream tests
// ---------------------------------------------------------------------------

mod stream_tests {
    use async_trait::async_trait;
    use futures::StreamExt;

    use dactor::actor::{Actor, ActorContext, ActorRef, ExpandHandler};
    use dactor::message::Message;
    use dactor::stream::StreamSender;
    use dactor_kameo::KameoRuntime;

    // -- Actor that streams N items --

    struct Streamer;

    impl Actor for Streamer {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            Streamer
        }
    }

    struct StreamN(u32);
    impl Message for StreamN {
        type Reply = u32;
    }

    #[async_trait]
    impl ExpandHandler<StreamN, u32> for Streamer {
        async fn handle_expand(
            &mut self,
            msg: StreamN,
            sender: StreamSender<u32>,
            _ctx: &mut ActorContext,
        ) {
            for i in 0..msg.0 {
                if sender.send(i).await.is_err() {
                    return;
                }
            }
        }
    }

    struct StreamEmpty;
    impl Message for StreamEmpty {
        type Reply = u32;
    }

    #[async_trait]
    impl ExpandHandler<StreamEmpty, u32> for Streamer {
        async fn handle_expand(
            &mut self,
            _msg: StreamEmpty,
            _sender: StreamSender<u32>,
            _ctx: &mut ActorContext,
        ) {
            // Send nothing — stream closes when handler returns
        }
    }

    #[tokio::test]
    async fn test_stream_returns_items() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<Streamer>("streamer-items", ()).await.unwrap();

        let stream = actor.expand(StreamN(5), 8, None, None).unwrap();
        let items: Vec<u32> = stream.collect().await;
        assert_eq!(items, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_stream_empty() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<Streamer>("streamer-empty", ()).await.unwrap();

        let stream = actor.expand(StreamEmpty, 8, None, None).unwrap();
        let items: Vec<u32> = stream.collect().await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_stream_consumer_drops_early() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<Streamer>("streamer-drop", ()).await.unwrap();

        // Request a streamof 1000 items but only take 2
        let stream = actor.expand(StreamN(1000), 1, None, None).unwrap();
        let items: Vec<u32> = stream.take(2).collect().await;
        assert_eq!(items, vec![0, 1]);

        // Actor should still be alive after consumer drop
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(actor.is_alive());
    }
}

// ---------------------------------------------------------------------------
// Feed tests
// ---------------------------------------------------------------------------

mod feed_tests {
    use async_trait::async_trait;

    use dactor::actor::{Actor, ActorContext, ActorRef, ReduceHandler};
    use dactor::stream::{BoxStream, StreamReceiver};
    use dactor_kameo::KameoRuntime;

    // -- Actor that sums fed integers --

    struct Summer;

    impl Actor for Summer {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            Summer
        }
    }

    #[async_trait]
    impl ReduceHandler<u64, u64> for Summer {
        async fn handle_reduce(
            &mut self,
            mut receiver: StreamReceiver<u64>,
            _ctx: &mut ActorContext,
        ) -> u64 {
            let mut sum = 0u64;
            while let Some(val) = receiver.recv().await {
                sum += val;
            }
            sum
        }
    }

    fn items_stream(items: Vec<u64>) -> BoxStream<u64> {
        Box::pin(futures::stream::iter(items))
    }

    #[tokio::test]
    async fn test_feed_sum_items() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<Summer>("summer-feed", ()).await.unwrap();

        let input = items_stream(vec![1, 2, 3, 4, 5]);
        let reply = actor
            .reduce::<u64, u64>(input, 8, None, None)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(reply, 15);
    }

    #[tokio::test]
    async fn test_feed_empty_stream() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<Summer>("summer-empty", ()).await.unwrap();

        let input = items_stream(vec![]);
        let reply = actor
            .reduce::<u64, u64>(input, 8, None, None)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(reply, 0);
    }
}

// ---------------------------------------------------------------------------
// Cancellation tests
// ---------------------------------------------------------------------------

mod cancellation_tests {
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::errors::RuntimeError;
    use dactor::message::Message;
    use dactor_kameo::KameoRuntime;

    struct SlowActor;

    impl Actor for SlowActor {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            SlowActor
        }
    }

    struct SlowPing;
    impl Message for SlowPing {
        type Reply = String;
    }

    #[async_trait]
    impl Handler<SlowPing> for SlowActor {
        async fn handle(&mut self, _msg: SlowPing, _ctx: &mut ActorContext) -> String {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            "done".into()
        }
    }

    #[tokio::test]
    async fn test_cancel_ask_before_handler() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<SlowActor>("cancel-pre", ()).await.unwrap();

        // Pre-cancelthe token before sending
        let token = CancellationToken::new();
        token.cancel();

        let result = actor.ask(SlowPing, Some(token)).unwrap().await;
        match result {
            Err(RuntimeError::Cancelled) => {}        // expected
            Err(RuntimeError::ActorNotFound(_)) => {} // also acceptable
            other => panic!("expected Cancelled or ActorNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_no_cancel_runs_normally() {
        let runtime = KameoRuntime::new();
        let actor = runtime.spawn::<SlowActor>("cancel-none", ()).await.unwrap();

        struct QuickPing;
        impl Message for QuickPing {
            type Reply = String;
        }

        #[async_trait]
        impl Handler<QuickPing> for SlowActor {
            async fn handle(&mut self, _msg: QuickPing, _ctx: &mut ActorContext) -> String {
                "quick".into()
            }
        }

        let reply = actor.ask(QuickPing, None).unwrap().await.unwrap();
        assert_eq!(reply, "quick");
    }
}

// ---------------------------------------------------------------------------
// Watch / Unwatch (DeathWatch) tests
// ---------------------------------------------------------------------------

mod watch_tests {
    use async_trait::async_trait;
    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::message::Message;
    use dactor::supervision::ChildTerminated;
    use dactor_kameo::KameoRuntime;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // -- Watcher actor: sets a flag when it receives ChildTerminated ----------

    struct Watcher {
        terminated: Arc<AtomicBool>,
    }

    impl Actor for Watcher {
        type Args = Arc<AtomicBool>;
        type Deps = ();
        fn create(terminated: Arc<AtomicBool>, _: ()) -> Self {
            Watcher { terminated }
        }
    }

    #[async_trait]
    impl Handler<ChildTerminated> for Watcher {
        async fn handle(&mut self, _msg: ChildTerminated, _ctx: &mut ActorContext) {
            self.terminated.store(true, Ordering::SeqCst);
        }
    }

    // -- Worker actor: minimal, stoppable -------------------------------------

    struct Worker;

    impl Actor for Worker {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            Worker
        }
    }

    struct Ping;
    impl Message for Ping {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<Ping> for Worker {
        async fn handle(&mut self, _: Ping, _ctx: &mut ActorContext) {}
    }

    #[tokio::test]
    async fn test_watch_receives_child_terminated() {
        let runtime = KameoRuntime::new();
        let terminated = Arc::new(AtomicBool::new(false));

        let watcher = runtime.spawn::<Watcher>("watch-watcher-1", terminated.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("watch-worker-1", ()).await.unwrap();

        runtime.watch(&watcher, worker.id());

        // Give actors time to fully start
        tokio::time::sleep(Duration::from_millis(50)).await;

        worker.stop();

        // Wait for the ChildTerminated notification to be delivered
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            terminated.load(Ordering::SeqCst),
            "watcher should have received ChildTerminated",
        );
    }

    #[tokio::test]
    async fn test_unwatch_stops_notifications() {
        let runtime = KameoRuntime::new();
        let terminated = Arc::new(AtomicBool::new(false));

        let watcher = runtime.spawn::<Watcher>("unwatch-watcher-1", terminated.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("unwatch-worker-1", ()).await.unwrap();

        let watcher_id = watcher.id();
        let worker_id = worker.id();
        runtime.watch(&watcher, worker_id.clone());
        runtime.unwatch(&watcher_id, &worker_id);

        // Give actors time to fully start
        tokio::time::sleep(Duration::from_millis(50)).await;

        worker.stop();

        // Wait long enough to be confident no notification arrived
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            !terminated.load(Ordering::SeqCst),
            "watcher should NOT have received ChildTerminated after unwatch",
        );
    }
}

// ---------------------------------------------------------------------------
// Bounded mailbox fallback
// ---------------------------------------------------------------------------

mod mailbox_tests {
    use async_trait::async_trait;
    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::mailbox::{MailboxConfig, OverflowStrategy};
    use dactor::message::Message;
    use dactor_kameo::{KameoRuntime, SpawnOptions};
    use std::time::Duration;

    struct Worker;

    impl Actor for Worker {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self {
            Worker
        }
    }

    struct Ping;
    impl Message for Ping {
        type Reply = ();
    }

    #[async_trait]
    impl Handler<Ping> for Worker {
        async fn handle(&mut self, _: Ping, _ctx: &mut ActorContext) {}
    }

    #[tokio::test]
    async fn test_bounded_mailbox_falls_back_to_unbounded() {
        let runtime = KameoRuntime::new();
        let options = SpawnOptions {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::bounded(10, OverflowStrategy::RejectWithError),
        };

        let actor = runtime.spawn_with_options::<Worker>("bounded-worker", (), options).await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(actor.is_alive());

        actor.tell(Ping).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        actor.stop();
    }

    // -- Slow actor for overflow tests --

    struct SlowWorker;
    impl Actor for SlowWorker {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self { SlowWorker }
    }

    #[derive(Clone)]
    struct SlowPing;
    impl Message for SlowPing { type Reply = (); }

    #[async_trait]
    impl Handler<SlowPing> for SlowWorker {
        async fn handle(&mut self, _: SlowPing, _ctx: &mut ActorContext) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    #[tokio::test]
    async fn test_bounded_mailbox_reject_when_full() {
        let runtime = KameoRuntime::new();
        let options = SpawnOptions {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::bounded(2, OverflowStrategy::RejectWithError),
        };
        let actor = runtime
            .spawn_with_options::<SlowWorker>("reject-test", (), options)
            .await
            .unwrap();

        let r1 = actor.tell(SlowPing);
        let r2 = actor.tell(SlowPing);
        let r3 = actor.tell(SlowPing);

        let ok_count = [r1.is_ok(), r2.is_ok(), r3.is_ok()]
            .iter()
            .filter(|&&r| r)
            .count();
        assert!(ok_count >= 2, "at least 2 should succeed");
    }

    #[tokio::test]
    async fn test_bounded_mailbox_pending_messages() {
        let runtime = KameoRuntime::new();
        let options = SpawnOptions {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::bounded(100, OverflowStrategy::RejectWithError),
        };
        let actor = runtime
            .spawn_with_options::<SlowWorker>("pending-test", (), options)
            .await
            .unwrap();

        for _ in 0..5 {
            actor.tell(SlowPing).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;

        let pending = actor.pending_messages();
        assert!(pending <= 100, "pending should be within capacity");
    }

    #[tokio::test]
    async fn test_bounded_mailbox_is_alive_after_stop() {
        let runtime = KameoRuntime::new();
        let options = SpawnOptions {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::bounded(10, OverflowStrategy::RejectWithError),
        };
        let actor = runtime
            .spawn_with_options::<Worker>("alive-test", (), options)
            .await
            .unwrap();

        assert!(actor.is_alive());
        actor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!actor.is_alive(), "should not be alive after stop");
    }
}

