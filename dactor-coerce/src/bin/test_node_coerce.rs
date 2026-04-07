//! Coerce-backed test node binary for E2E integration tests.
//!
//! Runs a [`TestNode`] gRPC server with a [`CoerceCommandHandler`] that
//! manages a simple counter actor via the `dactor-coerce` runtime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::supervision::ChildTerminated;
use dactor_coerce::{CoerceActorRef, CoerceRuntime};
use dactor_test_harness::handler::CommandHandler;
use dactor_test_harness::node::{TestNode, TestNodeConfig};

// ---------------------------------------------------------------------------
// Counter actor — the test actor used by T6 E2E tests
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

// Tell message: set the counter directly
struct SetCount {
    value: i64,
}
impl Message for SetCount {
    type Reply = ();
}

#[async_trait]
impl Handler<SetCount> for CounterActor {
    async fn handle(&mut self, msg: SetCount, _ctx: &mut ActorContext) {
        self.count = msg.value;
    }
}

// Tell message: increment after a delay (for concurrency testing)
struct SlowIncrement {
    amount: i64,
}
impl Message for SlowIncrement {
    type Reply = ();
}

#[async_trait]
impl Handler<SlowIncrement> for CounterActor {
    async fn handle(&mut self, msg: SlowIncrement, _ctx: &mut ActorContext) {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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

// Ask message: echo back payload bytes
struct Echo(Vec<u8>);
impl Message for Echo {
    type Reply = Vec<u8>;
}

#[async_trait]
impl Handler<Echo> for CounterActor {
    async fn handle(&mut self, msg: Echo, _ctx: &mut ActorContext) -> Vec<u8> {
        msg.0
    }
}

// Ask message: echo back payload bytes after a 2s delay (for timeout testing)
struct SlowEcho(Vec<u8>);
impl Message for SlowEcho {
    type Reply = Vec<u8>;
}

#[async_trait]
impl Handler<SlowEcho> for CounterActor {
    async fn handle(&mut self, msg: SlowEcho, _ctx: &mut ActorContext) -> Vec<u8> {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        msg.0
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
// CoerceCommandHandler — bridges gRPC commands to the coerce runtime
// ---------------------------------------------------------------------------

struct CoerceCommandHandler {
    runtime: CoerceRuntime,
    actors: Mutex<HashMap<String, CoerceActorRef<CounterActor>>>,
    watches: Mutex<Vec<(String, String)>>,
    live_count: AtomicU32,
}

impl CoerceCommandHandler {
    fn new(runtime: CoerceRuntime) -> Self {
        Self {
            runtime,
            actors: Mutex::new(HashMap::new()),
            watches: Mutex::new(Vec::new()),
            live_count: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl CommandHandler for CoerceCommandHandler {
    fn adapter_name(&self) -> &str {
        "coerce"
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
            "set_count" => {
                let value: i64 =
                    serde_json::from_slice(payload).map_err(|e| format!("bad payload: {}", e))?;
                actor_ref
                    .tell(SetCount { value })
                    .map_err(|e| format!("tell failed: {}", e))
            }
            "slow_increment" => {
                let amount: i64 =
                    serde_json::from_slice(payload).map_err(|e| format!("bad payload: {}", e))?;
                actor_ref
                    .tell(SlowIncrement { amount })
                    .map_err(|e| format!("tell failed: {}", e))
            }
            _ => Err(format!("unknown message type: {}", message_type)),
        }
    }

    async fn ask_actor(
        &self,
        actor_name: &str,
        message_type: &str,
        payload: &[u8],
        timeout_ms: u64,
    ) -> Result<Vec<u8>, String> {
        let actor_ref = {
            let actors = self.actors.lock().await;
            actors
                .get(actor_name)
                .ok_or_else(|| format!("actor '{}' not found", actor_name))?
                .clone()
        };

        let ask_fut = async {
            match message_type {
                "get_count" => {
                    let reply = actor_ref
                        .ask(GetCount, None)
                        .map_err(|e| format!("ask failed: {}", e))?;
                    let count = reply.await.map_err(|e| format!("reply failed: {}", e))?;
                    serde_json::to_vec(&count).map_err(|e| format!("serialize failed: {}", e))
                }
                "echo" => {
                    let reply = actor_ref
                        .ask(Echo(payload.to_vec()), None)
                        .map_err(|e| format!("ask failed: {}", e))?;
                    reply.await.map_err(|e| format!("reply failed: {}", e))
                }
                "slow_echo" => {
                    let reply = actor_ref
                        .ask(SlowEcho(payload.to_vec()), None)
                        .map_err(|e| format!("ask failed: {}", e))?;
                    reply.await.map_err(|e| format!("reply failed: {}", e))
                }
                _ => Err(format!("unknown message type: {}", message_type)),
            }
        };

        if timeout_ms > 0 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                ask_fut,
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err("ask timed out".into()),
            }
        } else {
            ask_fut.await
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

                // Collect watcher refs, then drop lock before notifying
                let watcher_refs: Vec<_> = {
                    let watches = self.watches.lock().await;
                    let actors = self.actors.lock().await;
                    watches
                        .iter()
                        .filter(|(_, target)| target == actor_name)
                        .filter_map(|(watcher, _)| actors.get(watcher).cloned())
                        .collect()
                };
                for watcher_ref in watcher_refs {
                    let _ = watcher_ref.tell(ChildTerminated {
                        child_id: dactor::node::ActorId {
                            node: dactor::node::NodeId("local".into()),
                            local: 0,
                        },
                        child_name: actor_name.to_string(),
                        reason: None,
                    });
                }

                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        self.live_count.fetch_sub(1, Ordering::Relaxed);
        Err(format!("actor '{}' did not terminate within 1s", actor_name))
    }

    async fn watch_actor(&self, watcher_name: &str, target_name: &str) -> Result<(), String> {
        let actors = self.actors.lock().await;
        if !actors.contains_key(watcher_name) {
            return Err(format!("watcher '{}' not found", watcher_name));
        }
        if !actors.contains_key(target_name) {
            return Err(format!("target '{}' not found", target_name));
        }
        drop(actors);
        let mut watches = self.watches.lock().await;
        if watches.iter().any(|(w, t)| w == watcher_name && t == target_name) {
            return Ok(());
        }
        watches.push((watcher_name.to_string(), target_name.to_string()));
        Ok(())
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

    let runtime = CoerceRuntime::with_node_id(dactor::node::NodeId(node_id.clone()));
    let handler = Arc::new(CoerceCommandHandler::new(runtime));

    let config = TestNodeConfig::from_args(&node_id, port);
    let node = TestNode::with_handler(config, handler);

    if let Err(e) = node.run().await {
        eprintln!("Test node error: {}", e);
        std::process::exit(1);
    }
}
