//! ActorRateLimiter — outbound interceptor that throttles message sends.
//!
//! Run with: cargo run --example rate_limiting --features test-support

use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::errors::RuntimeError;
use dactor::message::Message;
use dactor::throttle::ActorRateLimiter;
use dactor::TestRuntime;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct Work(u32);
impl Message for Work {
    type Reply = String;
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Worker;

impl Actor for Worker {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Worker
    }
}

#[async_trait]
impl Handler<Work> for Worker {
    async fn handle(&mut self, msg: Work, _ctx: &mut ActorContext) -> String {
        format!("done-{}", msg.0)
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Rate Limiting Example ===\n");

    // Rate limiter: max 3 messages per 1-second window
    let limiter = ActorRateLimiter::new(3, Duration::from_secs(1));

    let mut runtime = TestRuntime::new();
    runtime.add_outbound_interceptor(Box::new(limiter));

    let worker = runtime.spawn::<Worker>("worker", ());

    println!("--- Sending 8 rapid asks (limit: 3/sec) ---");
    let mut delivered = 0u32;
    let mut rejected = 0u32;

    for i in 1..=8 {
        let result = worker.ask(Work(i), None);
        match result {
            Ok(reply_future) => match reply_future.await {
                Ok(reply) => {
                    println!("  [{}] delivered: {}", i, reply);
                    delivered += 1;
                }
                Err(RuntimeError::Rejected {
                    interceptor,
                    reason,
                }) => {
                    println!("  [{}] rejected by {}: {}", i, interceptor, reason);
                    rejected += 1;
                }
                Err(e) => {
                    println!("  [{}] error: {}", i, e);
                }
            },
            Err(e) => {
                println!("  [{}] send error: {:?}", i, e);
            }
        }
    }

    println!("\n--- Summary ---");
    println!("  delivered: {}", delivered);
    println!("  rejected:  {}", rejected);

    // ActorRateLimiter: max_rate=3, so:
    //   Messages 1-3: Continue (under limit)
    //   Messages 4-6: Delay (over limit but ≤2×) — TestRuntime treats
    //     Delay as Continue (no async sleep in outbound pipeline), so
    //     these are delivered immediately.
    //   Messages 7-8: Reject (>2× limit)
    assert!(delivered >= 3, "at least 3 messages should be delivered");
    assert!(rejected > 0, "some messages should be rejected");
    println!("  ✓ rate limiting enforced");

    println!("\n=== Done ===");
}
