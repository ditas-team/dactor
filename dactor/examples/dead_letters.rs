//! DropObserver for monitoring interceptor-driven message drops.
//!
//! Run with: cargo run --example dead_letters --features test-support

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::interceptor::{
    Disposition, DropNotice, DropObserver, OutboundContext, OutboundInterceptor,
};
use dactor::message::{Headers, Message, RuntimeHeaders};
use dactor::TestRuntime;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct ProcessItem(String);
impl Message for ProcessItem {
    type Reply = ();
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Processor {
    processed: Vec<String>,
}

impl Actor for Processor {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Processor { processed: Vec::new() }
    }
}

#[async_trait]
impl Handler<ProcessItem> for Processor {
    async fn handle(&mut self, msg: ProcessItem, _ctx: &mut ActorContext) {
        println!("  [Processor] handled: {}", msg.0);
        self.processed.push(msg.0);
    }
}

// ---------------------------------------------------------------------------
// DropObserver — collects drop notices
// ---------------------------------------------------------------------------

struct CollectingObserver {
    notices: Arc<Mutex<Vec<String>>>,
}

impl DropObserver for CollectingObserver {
    fn on_drop(&self, notice: DropNotice) {
        let entry = format!(
            "DROPPED target={} msg={} by={} ctx={}",
            notice.target_name, notice.message_type, notice.interceptor_name, notice.context
        );
        println!("  [DropObserver] {}", entry);
        self.notices.lock().unwrap().push(entry);
    }
}

// ---------------------------------------------------------------------------
// Outbound interceptor — drops messages containing "spam"
// ---------------------------------------------------------------------------

struct SpamFilter;

impl OutboundInterceptor for SpamFilter {
    fn name(&self) -> &'static str {
        "spam-filter"
    }

    fn on_send(
        &self,
        _ctx: &OutboundContext<'_>,
        _rh: &RuntimeHeaders,
        _headers: &mut Headers,
        msg: &dyn Any,
    ) -> Disposition {
        if let Some(item) = msg.downcast_ref::<ProcessItem>() {
            if item.0.contains("spam") {
                println!("  [SpamFilter] dropping: {}", item.0);
                return Disposition::Drop;
            }
        }
        Disposition::Continue
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Dead Letters / Drop Observer Example ===\n");

    let drop_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let mut runtime = TestRuntime::new();
    runtime.set_drop_observer(Arc::new(CollectingObserver {
        notices: drop_log.clone(),
    }));
    runtime.add_outbound_interceptor(Box::new(SpamFilter));

    let processor = runtime.spawn::<Processor>("processor", ());

    // Send a mix of valid and spam messages
    println!("--- Sending messages ---");
    let items = vec![
        "order-001",
        "spam-promo",
        "order-002",
        "spam-newsletter",
        "order-003",
    ];

    for item in &items {
        processor.tell(ProcessItem(item.to_string())).unwrap();
    }

    // Allow processing to complete
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Show collected drop notices
    println!("\n--- Drop notices ---");
    let notices = drop_log.lock().unwrap();
    for notice in notices.iter() {
        println!("  {}", notice);
    }

    assert_eq!(notices.len(), 2, "expected 2 dropped messages");
    println!("\n  ✓ 2 spam messages dropped, 3 valid messages processed");
    println!("\n=== Done ===");
}
