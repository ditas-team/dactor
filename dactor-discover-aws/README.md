# dactor-discover-aws

AWS node discovery for the [dactor](https://github.com/Yaming-Hub/dactor) distributed actor framework.

## Discovery mechanisms

| Strategy | Struct | How it works |
|---|---|---|
| **Auto Scaling Group** | `AutoScalingDiscovery` | Lists instances in a named ASG, filters for `InService` + `Healthy`, and returns their IP addresses. |
| **EC2 Tags** | `Ec2TagDiscovery` | Queries `DescribeInstances` with a tag key/value filter and returns IPs of `running` instances. |

Both strategies implement the `dactor::ClusterDiscovery` trait.

## How dactor uses discovery

The `ClusterDiscovery` trait provides a `discover()` method that returns peer node addresses. The dactor runtime uses this as follows:

1. **On startup**: The runtime calls `discover()` to find initial peers and connects to them via `AdapterCluster::connect()`.
2. **Periodic polling**: The application is responsible for calling `discover()` periodically (e.g., via a timer) to detect topology changes — new nodes joining or leaving the ASG/cluster.
3. **Diff and reconcile**: Compare discovered peers with currently connected nodes. Connect to new peers, optionally disconnect removed ones.
4. **Events**: The `ClusterEventEmitter` fires `NodeJoined`/`NodeLeft` events that actors can subscribe to via `ClusterEvents`.

**Example: polling loop**

```rust,ignore
use std::collections::HashSet;
use std::time::Duration;

let discovery = AutoScalingDiscovery::builder()
    .asg_name("my-dactor-asg")
    .port(9000)
    .build();

let mut known_peers: HashSet<String> = HashSet::new();

loop {
    let current = discovery.discover().into_iter().collect::<HashSet<_>>();

    // New peers
    for peer in current.difference(&known_peers) {
        println!("New peer: {peer}");
        // runtime.connect_peer(NodeId(peer.clone()), Some(peer.clone()));
    }

    // Removed peers
    for peer in known_peers.difference(&current) {
        println!("Peer left: {peer}");
        // runtime.disconnect_peer(&NodeId(peer.clone()));
    }

    known_peers = current;
    tokio::time::sleep(Duration::from_secs(10)).await;
}
```

> **Note:** The current `ClusterDiscovery` trait is synchronous and one-shot.
> PUB4 (planned) will make it async and return `Result` for proper error handling.
> A built-in polling/reconciliation loop may be added to the dactor core in a future release.

## Usage

### Auto Scaling Group discovery

```rust
use dactor_discover_aws::AutoScalingDiscovery;
use dactor::ClusterDiscovery;

let discovery = AutoScalingDiscovery::builder()
    .asg_name("my-dactor-asg")
    .port(9000)
    .region("us-west-2")
    .build();

// Synchronous (requires a tokio runtime on the current thread)
let peers = discovery.discover();

// Async
// let peers = discovery.discover_async().await?;
```

### EC2 tag discovery

```rust
use dactor_discover_aws::Ec2TagDiscovery;
use dactor::ClusterDiscovery;

let discovery = Ec2TagDiscovery::builder()
    .tag_key("dactor-cluster")
    .tag_value("production")
    .port(9000)
    .build();

let peers = discovery.discover();
```

## IAM policy (least privilege)

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "autoscaling:DescribeAutoScalingGroups"
      ],
      "Resource": "*"
    },
    {
      "Effect": "Allow",
      "Action": [
        "ec2:DescribeInstances"
      ],
      "Resource": "*"
    }
  ]
}
```

## Environment variables

| Variable | Description |
|---|---|
| `DACTOR_INSTANCE_IP` | Override the current instance's private IP. |
| `DACTOR_INSTANCE_ID` | Override the current instance's ID. |
| `AWS_REGION` | AWS region (also used by the AWS SDK). |
| `AWS_DEFAULT_REGION` | Fallback region when `AWS_REGION` is not set. |

Helper functions `instance_private_ip()`, `instance_id()`, and `current_region()` read these variables.

## License

MIT
