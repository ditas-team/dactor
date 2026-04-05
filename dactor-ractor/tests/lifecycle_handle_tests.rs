//! Tests for JH1-JH3: actor lifecycle handles in ractor adapter.

use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::node::NodeId;
use dactor_ractor::RactorRuntime;

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
async fn jh1_join_handle_stored_on_spawn() {
    let runtime = RactorRuntime::new();
    let _actor = runtime.spawn::<SlowActor>("lh-stored-a1", ());
    assert_eq!(runtime.active_handle_count(), 1);

    let _actor2 = runtime.spawn::<SlowActor>("lh-stored-a2", ());
    assert_eq!(runtime.active_handle_count(), 2);
}

#[tokio::test]
async fn jh2_await_stop_resolves_after_actor_stops() {
    let runtime = RactorRuntime::new();
    let actor = runtime.spawn::<SlowActor>("lh-stopper", ());
    let actor_id = actor.id();

    actor.stop();
    let result = runtime.await_stop(&actor_id).await;
    assert!(result.is_ok());

    // Handle should be consumed
    assert_eq!(runtime.active_handle_count(), 0);
}

#[tokio::test]
async fn jh2_await_stop_unknown_id_returns_ok() {
    let runtime = RactorRuntime::new();
    let fake_id = dactor::node::ActorId {
        node: NodeId("fake".into()),
        local: 999,
    };
    let result = runtime.await_stop(&fake_id).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn jh3_await_all_waits_for_all_actors() {
    let runtime = RactorRuntime::new();
    let a1 = runtime.spawn::<SlowActor>("lh-all-x1", ());
    let a2 = runtime.spawn::<SlowActor>("lh-all-x2", ());
    let a3 = runtime.spawn::<SlowActor>("lh-all-x3", ());

    assert_eq!(runtime.active_handle_count(), 3);

    a1.stop();
    a2.stop();
    a3.stop();

    let result = runtime.await_all().await;
    assert!(result.is_ok());
    assert_eq!(runtime.active_handle_count(), 0);
}

#[tokio::test]
async fn jh_cleanup_finished_removes_stopped_actors() {
    let runtime = RactorRuntime::new();
    let actor = runtime.spawn::<SlowActor>("lh-cleanup", ());
    assert_eq!(runtime.active_handle_count(), 1);

    actor.stop();
    // Give the actor time to stop
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    runtime.cleanup_finished();
    assert_eq!(runtime.active_handle_count(), 0);
}

// -- JH4: Panic propagation through await_stop --------------------------------

struct PanickingActor;

#[async_trait::async_trait]
impl Actor for PanickingActor {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self { PanickingActor }

    async fn on_stop(&mut self) {
        panic!("intentional on_stop panic");
    }
}

#[async_trait::async_trait]
impl Handler<Stop> for PanickingActor {
    async fn handle(&mut self, _msg: Stop, _ctx: &mut ActorContext) {}
}

#[tokio::test]
async fn jh4_panic_propagated_through_await_stop() {
    let runtime = RactorRuntime::new();
    let actor = runtime.spawn::<PanickingActor>("panic-actor", ());
    let actor_id = actor.id();

    actor.stop();
    let result = runtime.await_stop(&actor_id).await;
    assert!(result.is_err(), "expected error from panicking on_stop");
    let err = result.unwrap_err();
    assert!(err.contains("on_stop"), "error should mention on_stop, got: {err}");
}