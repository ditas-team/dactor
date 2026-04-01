use std::time::Duration;

use dactor::actor::ActorRef;
use dactor::node::NodeId;
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
    let actor = cluster.node("node-1").runtime.spawn::<ConformanceCounter>("c1", 0);
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
    cluster.unfreeze_node("node-1", frozen);
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
    let state = cluster.state();
    assert_eq!(state.node_count(), 3);
    assert!(state.contains(&NodeId("node-2".into())));
}
