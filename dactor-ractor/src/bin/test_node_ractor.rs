//! Ractor-backed test node binary for E2E integration tests.
//!
//! Runs a [`TestNode`] gRPC server with a [`RactorCommandHandler`] that
//! manages a simple counter actor via the `dactor-ractor` runtime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::supervision::ChildTerminated;
use dactor_ractor::{RactorActorRef, RactorRuntime};
use dactor_test_harness::handler::CommandHandler;
use dactor_test_harness::node::{TestNode, TestNodeConfig};

// ---------------------------------------------------------------------------
// Counter actor — the test actor used by T1–T3 E2E tests
// ---------------------------------------------------------------------------

struct CounterActor {
    count: i64,
}

impl Actor for CounterActor {
    type Args = i64; // initial count
    type Deps = ();
    fn create(args: Self::Args, _deps: ()) -> Self {
        CounterActor { count: args }
    }
}

// Tell message: increment the counter
struct Increment {
    amount: i64,
}
impl Message for Increment {
    type Reply = ();
}

#[async_trait]
impl Handler<Increment> for CounterActor {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.amount;
    }
}

// Ask message: get the current count
struct GetCount;
impl Message for GetCount {
    type Reply = i64;
}

#[async_trait]
impl Handler<GetCount> for CounterActor {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> i64 {
        self.count
    }
}

// Watch handler: receive notification when a watched actor stops
#[async_trait]
impl Handler<ChildTerminated> for CounterActor {
    async fn handle(&mut self, msg: ChildTerminated, _ctx: &mut ActorContext) {
        tracing::info!(
            child_name = %msg.child_name,
            "watched actor terminated"
        );
        // Encode notification as a negative count so tests can detect it
        self.count = -999;
    }
}

// ---------------------------------------------------------------------------
// RactorCommandHandler — bridges gRPC commands to the ractor runtime
// ---------------------------------------------------------------------------

struct RactorCommandHandler {
    runtime: RactorRuntime,
    actors: Mutex<HashMap<String, RactorActorRef<CounterActor>>>,
    live_count: AtomicU32,
}

impl RactorCommandHandler {
    fn new(runtime: RactorRuntime) -> Self {
        Self {
            runtime,
            actors: Mutex::new(HashMap::new()),
            live_count: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl CommandHandler for RactorCommandHandler {
    fn adapter_name(&self) -> &str {
        "ractor"
    }

    async fn spawn_actor(
        &self,
        actor_type: &str,
        actor_name: &str,
        args: &[u8],
    ) -> Result<String, String> {
        if actor_type != "counter" {
            return Err(format!("unknown actor type: {}", actor_type));
        }

        let initial: i64 = if args.is_empty() {
            0
        } else {
            serde_json::from_slice(args).map_err(|e| format!("bad args: {}", e))?
        };

        let actor_ref = self
            .runtime
            .spawn::<CounterActor>(actor_name, initial)
            .await
            .map_err(|e| format!("spawn failed: {}", e))?;

        let id = actor_ref.id().to_string();
        let mut actors = self.actors.lock().await;
        if actors.contains_key(actor_name) {
            return Err(format!("actor '{}' already exists", actor_name));
        }
        actors.insert(actor_name.to_string(), actor_ref);
        self.live_count.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    async fn tell_actor(
        &self,
        actor_name: &str,
        message_type: &str,
        payload: &[u8],
    ) -> Result<(), String> {
        let actor_ref = {
            let actors = self.actors.lock().await;
            actors
                .get(actor_name)
                .ok_or_else(|| format!("actor '{}' not found", actor_name))?
                .clone()
        };

        match message_type {
            "increment" => {
                let amount: i64 = if payload.is_empty() {
                    1
                } else {
                    serde_json::from_slice(payload).map_err(|e| format!("bad payload: {}", e))?
                };
                actor_ref
                    .tell(Increment { amount })
                    .map_err(|e| format!("tell failed: {}", e))
            }
            _ => Err(format!("unknown message type: {}", message_type)),
        }
    }

    async fn ask_actor(
        &self,
        actor_name: &str,
        message_type: &str,
        _payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        let actor_ref = {
            let actors = self.actors.lock().await;
            actors
                .get(actor_name)
                .ok_or_else(|| format!("actor '{}' not found", actor_name))?
                .clone()
        };

        match message_type {
            "get_count" => {
                let reply = actor_ref
                    .ask(GetCount, None)
                    .map_err(|e| format!("ask failed: {}", e))?;
                let count = reply.await.map_err(|e| format!("reply failed: {}", e))?;
                serde_json::to_vec(&count).map_err(|e| format!("serialize failed: {}", e))
            }
            _ => Err(format!("unknown message type: {}", message_type)),
        }
    }

    async fn stop_actor(&self, actor_name: &str) -> Result<(), String> {
        let actor_ref = {
            let mut actors = self.actors.lock().await;
            actors
                .remove(actor_name)
                .ok_or_else(|| format!("actor '{}' not found", actor_name))?
        };
        actor_ref.stop();
        for _ in 0..100 {
            if !actor_ref.is_alive() {
                self.live_count.fetch_sub(1, Ordering::Relaxed);
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        self.live_count.fetch_sub(1, Ordering::Relaxed);
        Err(format!("actor '{}' did not terminate within 1s", actor_name))
    }

    fn actor_count(&self) -> u32 {
        self.live_count.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let node_id = std::env::var("DACTOR_NODE_ID").unwrap_or_else(|_| "test-node".to_string());
    let port: u16 = std::env::var("DACTOR_CONTROL_PORT")
        .unwrap_or_else(|_| "50051".to_string())
        .parse()
        .expect("invalid port");

    let runtime = RactorRuntime::with_node_id(dactor::node::NodeId(node_id.clone()));
    let handler = Arc::new(RactorCommandHandler::new(runtime));

    let config = TestNodeConfig::from_args(&node_id, port);
    let node = TestNode::with_handler(config, handler);

    if let Err(e) = node.run().await {
        eprintln!("Test node error: {}", e);
        std::process::exit(1);
    }
}
