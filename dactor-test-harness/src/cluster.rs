use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::events::EventStream;
use crate::protocol::test_node_service_client::TestNodeServiceClient;
use crate::protocol::*;

pub struct TestNodeHandle {
    pub process: Child,
    pub node_id: String,
    pub control_port: u16,
    client: Option<TestNodeServiceClient<tonic::transport::Channel>>,
}

pub struct TestCluster {
    nodes: HashMap<String, TestNodeHandle>,
}

pub struct TestClusterBuilder {
    nodes: Vec<(String, String, Vec<String>, u16)>,
}

impl TestCluster {
    pub fn builder() -> TestClusterBuilder {
        TestClusterBuilder { nodes: Vec::new() }
    }

    /// Ping a node to verify it's alive.
    pub async fn ping(
        &mut self,
        node_id: &str,
        echo: &str,
    ) -> Result<PingResponse, Box<dyn std::error::Error>> {
        let handle = self.nodes.get_mut(node_id).ok_or("node not found")?;
        let client = handle.client.as_mut().ok_or("not connected")?;
        let response = client
            .ping(PingRequest {
                echo: echo.to_string(),
            })
            .await?;
        Ok(response.into_inner())
    }

    /// Get node info.
    pub async fn get_node_info(
        &mut self,
        node_id: &str,
    ) -> Result<NodeInfoResponse, Box<dyn std::error::Error>> {
        let handle = self.nodes.get_mut(node_id).ok_or("node not found")?;
        let client = handle.client.as_mut().ok_or("not connected")?;
        let response = client.get_node_info(Empty {}).await?;
        Ok(response.into_inner())
    }

    /// Inject a fault on a node.
    pub async fn inject_fault(
        &mut self,
        node_id: &str,
        fault_type: &str,
        target: &str,
        duration_ms: u64,
        count: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let handle = self.nodes.get_mut(node_id).ok_or("node not found")?;
        let client = handle.client.as_mut().ok_or("not connected")?;
        client
            .inject_fault(FaultRequest {
                fault_type: fault_type.to_string(),
                target: target.to_string(),
                duration_ms,
                count,
                detail: String::new(),
            })
            .await?;
        Ok(())
    }

    /// Clear all faults on a node.
    pub async fn clear_faults(
        &mut self,
        node_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let handle = self.nodes.get_mut(node_id).ok_or("node not found")?;
        let client = handle.client.as_mut().ok_or("not connected")?;
        client.clear_faults(Empty {}).await?;
        Ok(())
    }

    /// Subscribe to events from a node.
    pub async fn subscribe_events(
        &mut self,
        node_id: &str,
        event_types: &[&str],
    ) -> Result<EventStream, Box<dyn std::error::Error>> {
        let handle = self.nodes.get_mut(node_id).ok_or("node not found")?;
        let client = handle.client.as_mut().ok_or("not connected")?;
        let response = client
            .subscribe_events(EventFilter {
                event_types: event_types.iter().map(|s| s.to_string()).collect(),
            })
            .await?;
        Ok(EventStream::new(response.into_inner()))
    }

    /// Send a custom command.
    pub async fn custom(
        &mut self,
        node_id: &str,
        command_type: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let handle = self.nodes.get_mut(node_id).ok_or("node not found")?;
        let client = handle.client.as_mut().ok_or("not connected")?;
        let response = client
            .custom_command(CustomRequest {
                command_type: command_type.to_string(),
                payload: payload.to_vec(),
            })
            .await?;
        Ok(response.into_inner().payload)
    }

    /// Graceful shutdown of a specific node.
    pub async fn shutdown_node(
        &mut self,
        node_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(handle) = self.nodes.get_mut(node_id) {
            if let Some(client) = handle.client.as_mut() {
                let _ = client
                    .shutdown(ShutdownRequest {
                        graceful: true,
                        timeout_ms: 5000,
                    })
                    .await;
            }
            let _ = handle.process.kill();
            let _ = handle.process.wait();
        }
        Ok(())
    }

    /// Shutdown all nodes.
    pub async fn shutdown(&mut self) {
        let node_ids: Vec<String> = self.nodes.keys().cloned().collect();
        for node_id in node_ids {
            let _ = self.shutdown_node(&node_id).await;
        }
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        for (_, handle) in self.nodes.iter_mut() {
            let _ = handle.process.kill();
            let _ = handle.process.wait(); // reap to avoid zombies and port reuse races
        }
    }
}

impl TestClusterBuilder {
    /// Add a node to the cluster.
    pub fn node(mut self, node_id: &str, binary: &str, args: &[&str], port: u16) -> Self {
        self.nodes.push((
            node_id.to_string(),
            binary.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
            port,
        ));
        self
    }

    /// Build and launch all nodes. Waits for each node to become reachable.
    /// Panics if any node fails to start or connect within the retry window.
    pub async fn build(self) -> TestCluster {
        let mut nodes = HashMap::new();

        for (node_id, binary, args, port) in self.nodes {
            let process = Command::new(&binary)
                .args(&args)
                .env("DACTOR_NODE_ID", &node_id)
                .env("DACTOR_CONTROL_PORT", port.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|e| {
                    panic!(
                        "Failed to launch test node '{}' ({}): {}",
                        node_id, binary, e
                    )
                });

            // Connect gRPC client with retry
            let addr = format!("http://127.0.0.1:{}", port);
            let mut client = None;
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                match TestNodeServiceClient::connect(addr.clone()).await {
                    Ok(c) => {
                        client = Some(c);
                        break;
                    }
                    Err(_) => continue,
                }
            }

            if client.is_none() {
                panic!(
                    "Failed to connect to test node '{}' at {} after 5s of retries",
                    node_id, addr
                );
            }

            nodes.insert(
                node_id.clone(),
                TestNodeHandle {
                    process,
                    node_id,
                    control_port: port,
                    client,
                },
            );
        }

        TestCluster { nodes }
    }
}
