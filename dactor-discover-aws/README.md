# dactor-discover-aws

AWS node discovery for the [dactor](https://github.com/Yaming-Hub/dactor) distributed actor framework.

## Discovery mechanisms

| Strategy | Struct | How it works |
|---|---|---|
| **Auto Scaling Group** | `AutoScalingDiscovery` | Lists instances in a named ASG, filters for `InService` + `Healthy`, and returns their IP addresses. |
| **EC2 Tags** | `Ec2TagDiscovery` | Queries `DescribeInstances` with a tag key/value filter and returns IPs of `running` instances. |

Both strategies implement the `dactor::ClusterDiscovery` trait.

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
