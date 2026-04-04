use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorRef, Handler};
use dactor::message::Message;
use dactor::node::NodeId;
use dactor::supervision::ChildTerminated;
use dactor::test_support::conformance::*;
use dactor_mock::MockCluster;

#[tokio::test]
async fn test_create_cluster() {
    let cluster = MockCluster::new(&["node-1", "node-2", "node-3"]);
    assert_eq!(cluster.node_count(), 3);
}

#[tokio::test]
async fn test_spawn_on_node() {
    let cluster = MockCluster::new(&["node-1"]);
    let node = cluster.node("node-1");
    let actor = node.runtime.spawn::<ConformanceCounter>("counter", 0);
    assert!(actor.is_alive());
    assert_eq!(actor.id().node, NodeId("node-1".into()));
}

#[tokio::test]
async fn test_tell_ask_on_node() {
    let cluster = MockCluster::new(&["node-1"]);
    let node = cluster.node("node-1");
    let actor = node.runtime.spawn::<ConformanceCounter>("counter", 0);

    actor.tell(Increment(5)).unwrap();
    actor.tell(Increment(3)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn test_actors_on_different_nodes_have_different_node_ids() {
    let cluster = MockCluster::new(&["node-1", "node-2"]);
    let a1 = cluster
        .node("node-1")
        .runtime
        .spawn::<ConformanceCounter>("c1", 0);
    let a2 = cluster
        .node("node-2")
        .runtime
        .spawn::<ConformanceCounter>("c2", 0);

    assert_eq!(a1.id().node, NodeId("node-1".into()));
    assert_eq!(a2.id().node, NodeId("node-2".into()));
    assert_ne!(a1.id(), a2.id());
}

#[tokio::test]
async fn test_network_partition() {
    let cluster = MockCluster::new(&["node-1", "node-2"]);
    let network = cluster.network();

    assert!(!network.is_partitioned(&NodeId("node-1".into()), &NodeId("node-2".into())));

    network.partition(&NodeId("node-1".into()), &NodeId("node-2".into()));
    assert!(network.is_partitioned(&NodeId("node-1".into()), &NodeId("node-2".into())));

    network.remove_partition(&NodeId("node-1".into()), &NodeId("node-2".into()));
    assert!(!network.is_partitioned(&NodeId("node-1".into()), &NodeId("node-2".into())));
}

#[tokio::test]
async fn test_network_counters() {
    let cluster = MockCluster::new(&["node-1"]);
    let network = cluster.network();
    assert_eq!(network.delivered_count(), 0);
    assert_eq!(network.dropped_count(), 0);
}

#[tokio::test]
async fn conformance_tell_and_ask_on_mock() {
    let cluster = MockCluster::new(&["node-1"]);
    let runtime = &cluster.node("node-1").runtime;
    test_tell_and_ask(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn conformance_ordering_on_mock() {
    let cluster = MockCluster::new(&["node-1"]);
    let runtime = &cluster.node("node-1").runtime;
    test_message_ordering(|name, init| runtime.spawn::<ConformanceCounter>(name, init)).await;
}

#[tokio::test]
async fn test_crash_node() {
    let mut cluster = MockCluster::new(&["node-1", "node-2"]);
    assert_eq!(cluster.node_count(), 2);

    // Spawn actor on node-1
    let actor = cluster
        .node("node-1")
        .runtime
        .spawn::<ConformanceCounter>("c1", 0);
    assert!(actor.is_alive());

    // Crash node-1
    cluster.crash_node("node-1");
    assert_eq!(cluster.node_count(), 1);

    // Actor should no longer be accessible via cluster
    // (the node is gone)
}

#[tokio::test]
async fn test_restart_node() {
    let mut cluster = MockCluster::new(&["node-1"]);

    // Spawn actor on node-1
    cluster
        .node("node-1")
        .runtime
        .spawn::<ConformanceCounter>("c1", 42);

    // Restart — fresh node, old actors gone
    cluster.restart_node("node-1");

    // Node exists again but with fresh runtime
    let actor = cluster
        .node("node-1")
        .runtime
        .spawn::<ConformanceCounter>("c2", 0);
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 0); // fresh actor
}

#[tokio::test]
async fn test_freeze_unfreeze() {
    let mut cluster = MockCluster::new(&["node-1"]);

    let actor = cluster
        .node("node-1")
        .runtime
        .spawn::<ConformanceCounter>("c1", 0);
    actor.tell(Increment(5)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Freeze: remove node
    let frozen = cluster.freeze_node("node-1").unwrap();
    assert_eq!(cluster.node_count(), 0);

    // Unfreeze: restore node
    cluster.unfreeze_node(frozen);
    assert_eq!(cluster.node_count(), 1);

    // Actor should still be accessible (same runtime instance)
    let count = actor.ask(GetCount, None).unwrap().await.unwrap();
    assert_eq!(count, 5);
}

#[tokio::test]
async fn test_partition_blocks_delivery() {
    let cluster = MockCluster::new(&["node-1", "node-2"]);
    let network = cluster.network();

    let src = NodeId("node-1".into());
    let dst = NodeId("node-2".into());

    assert!(network.can_deliver(&src, &dst));

    network.partition(&src, &dst);
    assert!(!network.can_deliver(&src, &dst));

    // Local always ok
    assert!(network.can_deliver(&src, &src));

    network.remove_partition(&src, &dst);
    assert!(network.can_deliver(&src, &dst));
}

#[tokio::test]
async fn test_network_delivery_counters() {
    let cluster = MockCluster::new(&["node-1", "node-2"]);
    let network = cluster.network();

    network.record_delivered();
    network.record_delivered();
    network.record_dropped();

    assert_eq!(network.delivered_count(), 2);
    assert_eq!(network.dropped_count(), 1);
}

#[tokio::test]
async fn test_cluster_state() {
    let cluster = MockCluster::new(&["node-1", "node-2", "node-3"]);
    let state = cluster.state().expect("cluster should not be empty");
    assert_eq!(state.node_count(), 3);
    assert!(state.contains(&NodeId("node-2".into())));
}

// --- Watch/unwatch test actors ---

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

struct Worker;

impl Actor for Worker {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Worker
    }
}

struct WorkerMsg;
impl Message for WorkerMsg {
    type Reply = ();
}

#[async_trait]
impl Handler<WorkerMsg> for Worker {
    async fn handle(&mut self, _: WorkerMsg, _: &mut ActorContext) {}
}

#[tokio::test]
async fn test_mock_cluster_watch() {
    let cluster = MockCluster::new(&["node-1"]);
    let node = cluster.node("node-1");

    let terminated = Arc::new(AtomicBool::new(false));
    let watcher = node.runtime.spawn::<Watcher>("watcher", terminated.clone());
    let worker = node.runtime.spawn::<Worker>("worker", ());

    let worker_id = worker.id();
    cluster.watch("node-1", &watcher, worker_id.clone());

    worker.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        terminated.load(Ordering::SeqCst),
        "watcher should receive ChildTerminated"
    );
}

#[tokio::test]
async fn test_mock_cluster_unwatch() {
    let cluster = MockCluster::new(&["node-1"]);
    let node = cluster.node("node-1");

    let terminated = Arc::new(AtomicBool::new(false));
    let watcher = node.runtime.spawn::<Watcher>("watcher", terminated.clone());
    let worker = node.runtime.spawn::<Worker>("worker", ());

    let worker_id = worker.id();
    let watcher_id = watcher.id();
    cluster.watch("node-1", &watcher, worker_id.clone());
    cluster.unwatch("node-1", &watcher_id, &worker_id);

    worker.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        !terminated.load(Ordering::SeqCst),
        "unwatch should prevent notification"
    );
}

// -- System actor integration tests (SA9) --

#[tokio::test]
async fn test_node_directory_auto_connected() {
    let cluster = MockCluster::new(&["n1", "n2", "n3"]);
    let n1 = cluster.node("n1");
    assert!(n1.node_directory.is_connected(&NodeId("n2".into())));
    assert!(n1.node_directory.is_connected(&NodeId("n3".into())));
    assert_eq!(n1.node_directory.connected_count(), 2);
}

#[tokio::test]
async fn test_crash_node_updates_peers() {
    let mut cluster = MockCluster::new(&["n1", "n2", "n3"]);
    cluster.crash_node("n2");

    // Surviving nodes should see n2 as disconnected
    let n1 = cluster.node("n1");
    assert!(!n1.node_directory.is_connected(&NodeId("n2".into())));
    assert!(n1.node_directory.is_connected(&NodeId("n3".into())));
}

#[tokio::test]
async fn test_restart_node_reconnects_peers() {
    let mut cluster = MockCluster::new(&["n1", "n2", "n3"]);
    cluster.crash_node("n2");
    cluster.restart_node("n2");

    // Restarted node should be connected to existing peers
    let n2 = cluster.node("n2");
    assert!(n2.node_directory.is_connected(&NodeId("n1".into())));
    assert!(n2.node_directory.is_connected(&NodeId("n3".into())));

    // Existing peers should see restarted node
    let n1 = cluster.node("n1");
    assert!(n1.node_directory.is_connected(&NodeId("n2".into())));
}

#[tokio::test]
async fn test_remote_watch_and_notify() {
    let mut cluster = MockCluster::new(&["n1", "n2"]);
    let target = dactor::node::ActorId {
        node: NodeId("n1".into()),
        local: 42,
    };
    let watcher = dactor::node::ActorId {
        node: NodeId("n2".into()),
        local: 10,
    };

    cluster.remote_watch("n1", target.clone(), watcher.clone());

    // Verify watcher is registered
    let n1 = cluster.node("n1");
    assert_eq!(n1.watch_manager.watchers_of(&target).len(), 1);

    // Notify termination
    let notifications = cluster.notify_terminated("n1", &target);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].watcher, watcher);
}

#[tokio::test]
async fn test_remote_unwatch() {
    let mut cluster = MockCluster::new(&["n1", "n2"]);
    let target = dactor::node::ActorId {
        node: NodeId("n1".into()),
        local: 42,
    };
    let watcher = dactor::node::ActorId {
        node: NodeId("n2".into()),
        local: 10,
    };

    cluster.remote_watch("n1", target.clone(), watcher.clone());
    cluster.remote_unwatch("n1", &target, &watcher);

    let notifications = cluster.notify_terminated("n1", &target);
    assert!(notifications.is_empty());
}

#[tokio::test]
async fn test_cancel_manager() {
    let mut cluster = MockCluster::new(&["n1"]);
    let token = tokio_util::sync::CancellationToken::new();
    let token_check = token.clone();

    cluster.register_cancel("n1", "req-1".into(), token);
    assert!(!token_check.is_cancelled());

    let response = cluster.cancel_request("n1", "req-1");
    assert!(matches!(
        response,
        dactor::system_actors::CancelResponse::Acknowledged
    ));
    assert!(token_check.is_cancelled());
}

#[tokio::test]
async fn test_cancel_unknown_request() {
    let mut cluster = MockCluster::new(&["n1"]);
    let response = cluster.cancel_request("n1", "nonexistent");
    assert!(matches!(
        response,
        dactor::system_actors::CancelResponse::NotFound { .. }
    ));
}
