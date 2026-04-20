# dactor-discover-azure

Azure node discovery for the [dactor](https://github.com/Yaming-Hub/dactor) distributed actor framework.

## Discovery mechanisms

| Strategy | Struct | How it works |
|---|---|---|
| **VMSS** | `VmssDiscovery` | Queries the Azure Instance Metadata Service (IMDS) to determine the current VM's VMSS, then lists all network interfaces in the scale set via the ARM API. |
| **Azure Tags** | `AzureTagDiscovery` | Lists VMs with a specific tag key/value via the ARM API, then resolves their private IPs from the associated network interfaces. |

Both strategies implement the `dactor::ClusterDiscovery` trait.

## How dactor uses discovery

The `ClusterDiscovery` trait provides a `discover()` method that returns peer node addresses. The dactor runtime uses this as follows:

1. **On startup**: The runtime calls `discover()` to find initial peers and connects to them via `AdapterCluster::connect()`.
2. **Periodic polling**: The application is responsible for calling `discover()` periodically (e.g., via a timer) to detect topology changes — new nodes joining or leaving the VMSS/cluster.
3. **Diff and reconcile**: Compare discovered peers with currently connected nodes. Connect to new peers, optionally disconnect removed ones.
4. **Events**: The `ClusterEventEmitter` fires `NodeJoined`/`NodeLeft` events that actors can subscribe to via `ClusterEvents`.

**Example: polling loop**

```rust,ignore
use std::collections::HashSet;
use std::time::Duration;

let discovery = VmssDiscovery::builder()
    .port(9000)
    .build();

let mut known_peers: HashSet<String> = HashSet::new();

loop {
    match discovery.discover().await {
        Ok(current_vec) => {
            let current: HashSet<String> = current_vec.into_iter().collect();

            for peer in current.difference(&known_peers) {
                println!("New peer: {peer}");
                // runtime.connect_peer(NodeId(peer.clone()), Some(peer.clone()));
            }

            for peer in known_peers.difference(&current) {
                println!("Peer left: {peer}");
                // runtime.disconnect_peer(&NodeId(peer.clone()));
            }

            known_peers = current;
        }
        Err(e) => {
            eprintln!("Discovery error: {e}");
        }
    }
    tokio::time::sleep(Duration::from_secs(10)).await;
}
```

## Usage

### VMSS discovery (auto-detect via IMDS)

```rust,ignore
use dactor_discover_azure::VmssDiscovery;
use dactor::ClusterDiscovery;

let discovery = VmssDiscovery::builder()
    .port(9000)
    .build();

let peers = discovery.discover().await?;
```

### VMSS discovery (explicit config)

```rust,ignore
use dactor_discover_azure::VmssDiscovery;

let discovery = VmssDiscovery::builder()
    .subscription_id("00000000-0000-0000-0000-000000000000")
    .resource_group("my-resource-group")
    .vmss_name("my-vmss")
    .port(9000)
    .use_imds(false)
    .build();

let peers = discovery.discover().await?;
```

### Tag-based discovery

```rust,ignore
use dactor_discover_azure::AzureTagDiscovery;
use dactor::ClusterDiscovery;

let discovery = AzureTagDiscovery::builder()
    .tag_key("dactor-cluster")
    .tag_value("production")
    .subscription_id("00000000-0000-0000-0000-000000000000")
    .port(9000)
    .build();

let peers = discovery.discover().await?;
```

## Azure RBAC / Managed Identity requirements

Both discovery strategies require a **Managed Identity** (system-assigned or user-assigned) with at least the **Reader** role on the relevant resources.

### VMSS discovery

The Managed Identity needs:
- `Microsoft.Compute/virtualMachineScaleSets/read` — to list VMSS instances
- `Microsoft.Network/networkInterfaces/read` — to read NIC IP configurations

The built-in **Reader** role covers both.

### Tag-based discovery

The Managed Identity needs:
- `Microsoft.Compute/virtualMachines/read` — to list VMs and their tags
- `Microsoft.Network/networkInterfaces/read` — to resolve NIC private IPs

### Example role assignment (Azure CLI)

```bash
# Assign Reader role to the VMSS system-assigned identity on its own resource group
az role assignment create \
  --assignee <managed-identity-principal-id> \
  --role Reader \
  --scope /subscriptions/<sub-id>/resourceGroups/<rg-name>
```

## Instance Metadata Service (IMDS)

The [Azure IMDS](https://learn.microsoft.com/en-us/azure/virtual-machines/instance-metadata-service) is a REST endpoint available at `http://169.254.169.254` on every Azure VM. It provides information about the running instance without requiring authentication.

This crate uses IMDS to:
1. **Discover the current VM's identity** — subscription ID, resource group, and VMSS name.
2. **Acquire a Managed Identity token** — used to authenticate ARM API calls.

All IMDS requests include the `Metadata: true` header as required by Azure.

## Environment variables

| Variable | Description |
|---|---|
| `DACTOR_VM_IP` | Override the current VM's private IP. |
| `AZURE_SUBSCRIPTION_ID` | Override the Azure subscription ID. |
| `AZURE_RESOURCE_GROUP` | Override the Azure resource group name. |

Helper functions `vm_private_ip()`, `subscription_id()`, and `resource_group()` read these variables.

## License

MIT
