# dactor-discover-k8s

Kubernetes-based cluster node discovery for the [dactor](https://github.com/ditas-team/dactor) distributed actor framework.

## Overview

This crate provides two Kubernetes-native implementations of dactor's `ClusterDiscovery` trait:

| Strategy | Mechanism | Best for |
|---|---|---|
| `KubernetesDiscovery` | Kubernetes API pod listing | Fine-grained control, label-based filtering |
| `HeadlessServiceDiscovery` | DNS resolution of a headless `Service` | Simple setups, no RBAC needed |

## How dactor uses discovery

The `ClusterDiscovery` trait provides a `discover()` method that returns peer node addresses. The dactor runtime uses this as follows:

1. **On startup**: The runtime calls `discover()` to find initial peers and connects via `AdapterCluster::connect()`.
2. **Periodic polling**: The application calls `discover()` periodically (e.g., every 10s) to detect topology changes — pods scaling up/down, rolling restarts.
3. **Diff and reconcile**: Compare discovered peers with connected nodes. Connect new peers, disconnect removed ones.
4. **Events**: `ClusterEventEmitter` fires `NodeJoined`/`NodeLeft` events for actor subscriptions.

```rust,ignore
// Example: periodic discovery loop
loop {
    let peers = discovery.discover();
    // reconcile with known_peers, connect/disconnect as needed
    tokio::time::sleep(Duration::from_secs(10)).await;
}
```

> **Note:** PUB4 (planned) will make ClusterDiscovery async with Result return type.
> A built-in polling/reconciliation loop may be added to dactor core in a future release.

## Quick Start

### API-based discovery

```rust
use dactor::ClusterDiscovery;
use dactor_discover_k8s::KubernetesDiscovery;

let discovery = KubernetesDiscovery::builder()
    .namespace("production")
    .label_selector("app=my-dactor-service")
    .port(9000)
    .port_name("dactor")
    .build();

// Sync (calls the K8s API internally via tokio)
let peers: Vec<String> = discovery.discover();

// Or use async directly
// let peers = discovery.discover_async().await?;
```

### Headless service discovery

```rust
use dactor::ClusterDiscovery;
use dactor_discover_k8s::HeadlessServiceDiscovery;

let discovery = HeadlessServiceDiscovery::new("dactor-cluster", "production", 9000);
let peers: Vec<String> = discovery.discover();
```

## Kubernetes Deployment Example

### Deployment with labels

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-dactor-service
  namespace: production
spec:
  replicas: 3
  selector:
    matchLabels:
      app: my-dactor-service
  template:
    metadata:
      labels:
        app: my-dactor-service
    spec:
      containers:
        - name: app
          image: my-dactor-service:latest
          ports:
            - name: dactor
              containerPort: 9000
          env:
            - name: POD_IP
              valueFrom:
                fieldRef:
                  fieldPath: status.podIP
```

### Headless Service (for DNS-based discovery)

```yaml
apiVersion: v1
kind: Service
metadata:
  name: dactor-cluster
  namespace: production
spec:
  clusterIP: None          # headless
  selector:
    app: my-dactor-service
  ports:
    - name: dactor
      port: 9000
      targetPort: dactor
```

### RBAC (for API-based discovery)

The pod's service account needs permission to list pods:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: dactor-pod-reader
  namespace: production
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: dactor-pod-reader-binding
  namespace: production
subjects:
  - kind: ServiceAccount
    name: default
    namespace: production
roleRef:
  kind: Role
  name: dactor-pod-reader
  apiGroup: rbac.authorization.k8s.io
```

## API vs Headless Service Discovery

| Feature | `KubernetesDiscovery` | `HeadlessServiceDiscovery` |
|---|---|---|
| Mechanism | Kubernetes API (pod list) | DNS A/AAAA record lookup |
| RBAC required | Yes (pod list/get) | No |
| Named port resolution | Yes | No |
| Pod phase filtering | Yes (Running only) | Handled by K8s readiness |
| Network dependency | API server | CoreDNS / kube-dns |
| Latency | Higher (API call) | Lower (DNS cache) |
| Granularity | Full pod metadata | IP addresses only |

## Environment Variables

| Variable | Description |
|---|---|
| `DACTOR_POD_IP` | Pod's own IP (preferred) |
| `POD_IP` | Pod's own IP (fallback) |

The namespace is auto-detected from the mounted service account token at
`/var/run/secrets/kubernetes.io/serviceaccount/namespace`.

## License

MIT
