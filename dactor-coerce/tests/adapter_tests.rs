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
    let actor = runtime.spawn::<ConformanceCounter>("counter", 0).await.unwrap();
    actor.tell(Increment(10)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 10);
}

#[tokio::test]
async fn test_coerce_stop() {
    let runtime = CoerceRuntime::new();
    let actor = runtime.spawn::<ConformanceCounter>("counter", 0).await.unwrap();
    assert!(actor.is_alive());
    actor.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!actor.is_alive());
}

#[tokio::test]
async fn test_coerce_multiple_actors() {
    let runtime = CoerceRuntime::new();
    let a1 = runtime.spawn::<ConformanceCounter>("c1", 100).await.unwrap();
    let a2 = runtime.spawn::<ConformanceCounter>("c2", 200).await.unwrap();

    let v1 = a1.ask(GetCount, None).unwrap().await.unwrap();
    let v2 = a2.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(v1, 100);
    assert_eq!(v2, 200);
    assert_ne!(a1.id(), a2.id());
}

#[tokio::test]
async fn test_coerce_default_runtime() {
    let runtime = CoerceRuntime::default();
    let actor = runtime.spawn::<ConformanceCounter>("counter", 42).await.unwrap();
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 42);
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
    use dactor_coerce::{CoerceRuntime, SpawnOptions};

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

        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn_with_options::<Echo>("echo-inbound-called", (), options).await.unwrap();

        actor.tell(Fire(1)).unwrap();
        actor.tell(Fire(2)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(receive_count.load(Ordering::SeqCst), 2);
        assert_eq!(complete_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_outbound_interceptor_called() {
        let (interceptor, send_count) = OutboundLogger::new();
        let mut runtime = CoerceRuntime::new();
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

        let runtime = CoerceRuntime::new();
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

        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn_with_options::<Echo>("echo-on-complete", (), options).await.unwrap();

        let reply = actor.ask(Ping("test".into()), None).unwrap().await.unwrap();
        assert_eq!(reply, "pong:test");

        actor.tell(Fire(42)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(ask_count.load(Ordering::SeqCst), 1);
        assert_eq!(tell_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_outbound_reject_ask() {
        let mut runtime = CoerceRuntime::new();
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

        let mut runtime = CoerceRuntime::new();
        runtime.add_outbound_interceptor(Box::new(outbound));

        let options = SpawnOptions {
            interceptors: vec![Box::new(inbound)],
            ..Default::default()
        };
        let actor = runtime.spawn_with_options::<Echo>("echo-both-pipelines", (), options).await.unwrap();

        let reply = actor.ask(Ping("x".into()), None).unwrap().await.unwrap();
        assert_eq!(reply, "pong:x");

        assert_eq!(out_send.load(Ordering::SeqCst), 1);
        assert_eq!(in_recv.load(Ordering::SeqCst), 1);
        assert_eq!(in_comp.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_inbound_reject_tell_silently_dropped() {
        let count = Arc::new(AtomicU64::new(0));
        let count_clone = count.clone();

        // Use a counter actor to verify the handler was NOT called
        struct CountActor(Arc<AtomicU64>);
        impl dactor::actor::Actor for CountActor {
            type Args = Arc<AtomicU64>;
            type Deps = ();
            fn create(args: Arc<AtomicU64>, _: ()) -> Self { CountActor(args) }
        }
        #[async_trait::async_trait]
        impl dactor::actor::Handler<Fire> for CountActor {
            async fn handle(&mut self, _msg: Fire, _ctx: &mut dactor::actor::ActorContext) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let options = SpawnOptions {
            interceptors: vec![Box::new(RejectInterceptor {
                reason: "nope".into(),
            })],
            ..Default::default()
        };

        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn_with_options::<CountActor>("echo-reject-tell", count_clone, options).await.unwrap();

        actor.tell(Fire(1)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(actor.is_alive());
        assert_eq!(count.load(Ordering::SeqCst), 0, "handler should not have been called");
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
    use dactor_coerce::CoerceRuntime;

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
        let runtime = CoerceRuntime::new();
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
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<LifecycleActor>("lifecycle-stop", events.clone()).await.unwrap();

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
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<PanicActor>(
            "panic-resume",
            (
                ErrorAction::Resume,
                error_count.clone(),
                handle_count.clone(),
            ),
        ).await.unwrap();

        actor.tell(DoPanic).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(error_count.load(Ordering::SeqCst), 1);

        let reply = actor.ask(SafeMsg, None).unwrap().await.unwrap();
        assert_eq!(reply, "ok");
        assert_eq!(handle_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_on_error_stop_terminates() {
        let error_count = Arc::new(AtomicU64::new(0));
        let handle_count = Arc::new(AtomicU64::new(0));
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<PanicActor>(
            "panic-stop",
            (ErrorAction::Stop, error_count.clone(), handle_count.clone()),
        ).await.unwrap();

        actor.tell(DoPanic).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        assert_eq!(error_count.load(Ordering::SeqCst), 1);
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
    use dactor_coerce::CoerceRuntime;

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
    impl ExpandHandler<StreamN> for Streamer {
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
    impl ExpandHandler<StreamEmpty> for Streamer {
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
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<Streamer>("streamer-items", ()).await.unwrap();

        let stream = actor.expand(StreamN(5), 8, None, None).unwrap();
        let items: Vec<u32> = stream.collect().await;
        assert_eq!(items, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_stream_empty() {
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<Streamer>("streamer-empty", ()).await.unwrap();

        let stream = actor.expand(StreamEmpty, 8, None, None).unwrap();
        let items: Vec<u32> = stream.collect().await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_stream_consumer_drops_early() {
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<Streamer>("streamer-drop", ()).await.unwrap();

        let stream = actor.expand(StreamN(1000), 1, None, None).unwrap();
        let items: Vec<u32> = stream.take(2).collect().await;
        assert_eq!(items, vec![0, 1]);

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
    use dactor_coerce::CoerceRuntime;

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
        let runtime = CoerceRuntime::new();
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
        let runtime = CoerceRuntime::new();
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
    use dactor_coerce::CoerceRuntime;

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
        let runtime = CoerceRuntime::new();
        let actor = runtime.spawn::<SlowActor>("cancel-pre", ()).await.unwrap();

        let token = CancellationToken::new();
        token.cancel();

        let result = actor.ask(SlowPing, Some(token)).unwrap().await;
        match result {
            Err(RuntimeError::Cancelled) => {}
            Err(RuntimeError::ActorNotFound(_)) => {}
            other => panic!("expected Cancelled or ActorNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_no_cancel_runs_normally() {
        let runtime = CoerceRuntime::new();
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
// Watch / Unwatch (DeathWatch)
// ---------------------------------------------------------------------------

mod watch_tests {
    use async_trait::async_trait;
    use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
    use dactor::message::Message;
    use dactor::supervision::ChildTerminated;
    use dactor_coerce::CoerceRuntime;
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
        let runtime = CoerceRuntime::new();
        let terminated = Arc::new(AtomicBool::new(false));

        let watcher = runtime.spawn::<Watcher>("watch-watcher-1", terminated.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("watch-worker-1", ()).await.unwrap();

        runtime.watch(&watcher, worker.id());

        tokio::time::sleep(Duration::from_millis(50)).await;

        worker.stop();

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            terminated.load(Ordering::SeqCst),
            "watcher should have received ChildTerminated",
        );
    }

    #[tokio::test]
    async fn test_unwatch_stops_notifications() {
        let runtime = CoerceRuntime::new();
        let terminated = Arc::new(AtomicBool::new(false));

        let watcher = runtime.spawn::<Watcher>("unwatch-watcher-1", terminated.clone()).await.unwrap();
        let worker = runtime.spawn::<Worker>("unwatch-worker-1", ()).await.unwrap();

        let watcher_id = watcher.id();
        let worker_id = worker.id();
        runtime.watch(&watcher, worker_id.clone());
        runtime.unwatch(&watcher_id, &worker_id);

        tokio::time::sleep(Duration::from_millis(50)).await;

        worker.stop();

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            !terminated.load(Ordering::SeqCst),
            "watcher should NOT have received ChildTerminated after unwatch",
        );
    }
}

// ---------------------------------------------------------------------------
// Mailbox tests
// ---------------------------------------------------------------------------

mod mailbox_tests {
    use super::*;
    use dactor::mailbox::{MailboxConfig, OverflowStrategy};
    use dactor::message::Message;
    use dactor_coerce::SpawnOptions;

    struct MailboxEcho;
    impl dactor::actor::Actor for MailboxEcho {
        type Args = ();
        type Deps = ();
        fn create(_: (), _: ()) -> Self { MailboxEcho }
    }

    struct MailboxPing(String);
    impl Message for MailboxPing { type Reply = String; }

    #[async_trait::async_trait]
    impl dactor::actor::Handler<MailboxPing> for MailboxEcho {
        async fn handle(&mut self, msg: MailboxPing, _ctx: &mut dactor::actor::ActorContext) -> String {
            format!("pong:{}", msg.0)
        }
    }

    #[tokio::test]
    async fn test_bounded_mailbox_falls_back_to_unbounded() {
        let runtime = CoerceRuntime::new();
        let options = SpawnOptions {
            interceptors: vec![],
            mailbox: MailboxConfig::Bounded {
                capacity: 5,
                overflow: OverflowStrategy::DropNewest,
            },
        };
        let actor = runtime.spawn_with_options::<MailboxEcho>("bounded-test", (), options).await.unwrap();
        let reply = actor.ask(MailboxPing("hi".into()), None).unwrap().await.unwrap();
        assert_eq!(reply, "pong:hi");
    }
}

