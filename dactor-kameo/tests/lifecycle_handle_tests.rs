//! Tests for JH1-JH3: actor lifecycle handles in kameo adapter.

use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::node::NodeId;
use dactor_kameo::KameoRuntime;

struct SlowActor;

impl Actor for SlowActor {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self { SlowActor }
}

struct Stop;
impl Message for Stop { type Reply = (); }

#[async_trait::async_trait]
impl Handler<Stop> for SlowActor {
    async fn handle(&mut self, _msg: Stop, _ctx: &mut ActorContext) {}
}

#[tokio::test]
async fn jh1_stop_receiver_stored_on_spawn() {
    let runtime = KameoRuntime::new();
    let _actor = runtime.spawn::<SlowActor>("a1", ());
    assert_eq!(runtime.active_handle_count(), 1);

    let _actor2 = runtime.spawn::<SlowActor>("a2", ());
    assert_eq!(runtime.active_handle_count(), 2);
}

#[tokio::test]
async fn jh2_await_stop_resolves_after_actor_stops() {
    let runtime = KameoRuntime::new();
    let actor = runtime.spawn::<SlowActor>("stopper", ());
    let actor_id = actor.id();

    actor.stop();
    // Give kameo time to run on_stop
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = runtime.await_stop(&actor_id).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn jh2_await_stop_unknown_id_returns_ok() {
    let runtime = KameoRuntime::new();
    let fake_id = dactor::node::ActorId {
        node: NodeId("fake".into()),
        local: 999,
    };
    let result = runtime.await_stop(&fake_id).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn jh3_await_all_waits_for_all_actors() {
    let runtime = KameoRuntime::new();
    let a1 = runtime.spawn::<SlowActor>("a1", ());
    let a2 = runtime.spawn::<SlowActor>("a2", ());
    let a3 = runtime.spawn::<SlowActor>("a3", ());

    assert_eq!(runtime.active_handle_count(), 3);

    a1.stop();
    a2.stop();
    a3.stop();

    // Give kameo time to run on_stop
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let result = runtime.await_all().await;
    assert!(result.is_ok());
    assert_eq!(runtime.active_handle_count(), 0);
}

#[tokio::test]
async fn jh_cleanup_finished_removes_stopped_actors() {
    let runtime = KameoRuntime::new();
    let actor = runtime.spawn::<SlowActor>("cleanup-test", ());
    assert_eq!(runtime.active_handle_count(), 1);

    actor.stop();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    runtime.cleanup_finished();
    assert_eq!(runtime.active_handle_count(), 0);
}
