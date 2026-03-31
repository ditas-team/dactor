//! Interceptors for logging, timing, and header stamping.
//!
//! Run with: cargo run --example interceptors --features test-support

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, Handler, TypedActorRef};
use dactor::interceptor::{
    Disposition, InboundContext, InboundInterceptor, Outcome, OutboundContext,
    OutboundInterceptor,
};
use dactor::mailbox::MailboxConfig;
use dactor::message::{Headers, Message, Priority, RuntimeHeaders};
use dactor::{SpawnOptions, V2TestRuntime};

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Greeter;

impl Actor for Greeter {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Greeter
    }
}

struct SayHello(String);
impl Message for SayHello {
    type Reply = String;
}

#[async_trait]
impl Handler<SayHello> for Greeter {
    async fn handle(&mut self, msg: SayHello, ctx: &mut ActorContext) -> String {
        // Check if a Priority header was stamped by the outbound interceptor.
        let priority = ctx.headers.get::<Priority>().map(|p| p.0);
        println!(
            "  [Greeter] handling '{}' (priority header: {:?})",
            msg.0, priority
        );
        format!("Hello, {}!", msg.0)
    }
}

// ---------------------------------------------------------------------------
// Inbound interceptor — logs message arrival and handler outcome
// ---------------------------------------------------------------------------

struct LoggingInterceptor {
    log: Arc<Mutex<Vec<String>>>,
}

impl InboundInterceptor for LoggingInterceptor {
    fn name(&self) -> &'static str {
        "logging"
    }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        let entry = format!("RECV  actor={} msg={}", ctx.actor_name, ctx.message_type);
        println!("  [LoggingInterceptor] {}", entry);
        self.log.lock().unwrap().push(entry);
        Disposition::Continue
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &Headers,
        outcome: &Outcome<'_>,
    ) {
        let entry = format!(
            "DONE  actor={} msg={} outcome={:?}",
            ctx.actor_name, ctx.message_type, outcome
        );
        println!("  [LoggingInterceptor] {}", entry);
        self.log.lock().unwrap().push(entry);
    }
}

// ---------------------------------------------------------------------------
// Inbound interceptor — records handler latency
// ---------------------------------------------------------------------------

struct TimingInterceptor {
    starts: Mutex<Vec<(&'static str, Instant)>>,
}

impl TimingInterceptor {
    fn new() -> Self {
        Self {
            starts: Mutex::new(Vec::new()),
        }
    }
}

impl InboundInterceptor for TimingInterceptor {
    fn name(&self) -> &'static str {
        "timing"
    }

    fn on_receive(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        self.starts
            .lock()
            .unwrap()
            .push((ctx.message_type, Instant::now()));
        Disposition::Continue
    }

    fn on_complete(
        &self,
        ctx: &InboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &Headers,
        _outcome: &Outcome<'_>,
    ) {
        let starts = self.starts.lock().unwrap();
        if let Some((_, t)) = starts.iter().rev().find(|(n, _)| *n == ctx.message_type) {
            println!(
                "  [TimingInterceptor] {} took {:?}",
                ctx.message_type,
                t.elapsed()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound interceptor — stamps every outgoing message with HIGH priority
// ---------------------------------------------------------------------------

struct HeaderStampInterceptor;

impl OutboundInterceptor for HeaderStampInterceptor {
    fn name(&self) -> &'static str {
        "header-stamp"
    }

    fn on_send(
        &self,
        ctx: &OutboundContext<'_>,
        _rh: &RuntimeHeaders,
        headers: &mut Headers,
        _msg: &dyn Any,
    ) -> Disposition {
        headers.insert(Priority::HIGH);
        println!(
            "  [HeaderStampInterceptor] stamped Priority::HIGH on {} → {}",
            ctx.message_type, ctx.target_name
        );
        Disposition::Continue
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Interceptors Example ===\n");

    let log = Arc::new(Mutex::new(Vec::<String>::new()));

    // Register a global outbound interceptor BEFORE spawning actors.
    let mut runtime = V2TestRuntime::new();
    runtime.add_outbound_interceptor(Box::new(HeaderStampInterceptor));

    // Spawn the actor with per-actor inbound interceptors via SpawnOptions.
    let greeter = runtime.spawn_with_options::<Greeter>(
        "greeter",
        (),
        SpawnOptions {
            interceptors: vec![
                Box::new(LoggingInterceptor { log: log.clone() }),
                Box::new(TimingInterceptor::new()),
            ],
            mailbox: MailboxConfig::Unbounded,
        },
    );

    // Send a request-reply message — interceptors will fire on both sides.
    println!("--- Sending ask(SayHello) ---");
    let reply = greeter
        .ask(SayHello("World".into()))
        .unwrap()
        .await
        .unwrap();
    println!("  [Main] reply: {}\n", reply);

    // Send another message to see a second round of interceptor output.
    println!("--- Sending ask(SayHello) ---");
    let reply = greeter
        .ask(SayHello("Rust".into()))
        .unwrap()
        .await
        .unwrap();
    println!("  [Main] reply: {}\n", reply);

    // Print the accumulated log.
    println!("--- Interceptor log ---");
    for entry in log.lock().unwrap().iter() {
        println!("  {}", entry);
    }

    // Allow async processing to settle.
    tokio::time::sleep(Duration::from_millis(50)).await;
    println!("\n=== Done ===");
}
