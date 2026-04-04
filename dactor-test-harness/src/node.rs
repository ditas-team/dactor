use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Notify};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::fault::FaultInjector;
use crate::protocol::test_node_service_server::{TestNodeService, TestNodeServiceServer};
use crate::protocol::*;

pub struct TestNodeConfig {
    pub node_id: String,
    pub control_port: u16,
}

impl TestNodeConfig {
    pub fn from_args(node_id: &str, port: u16) -> Self {
        Self {
            node_id: node_id.to_string(),
            control_port: port,
        }
    }
}

pub struct TestNode {
    config: TestNodeConfig,
    start_time: Instant,
    fault_injector: Arc<FaultInjector>,
    event_tx: broadcast::Sender<NodeEvent>,
    shutdown_flag: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    actor_count: Arc<AtomicU32>,
}

impl TestNode {
    pub fn new(config: TestNodeConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            config,
            start_time: Instant::now(),
            fault_injector: Arc::new(FaultInjector::new()),
            event_tx,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            actor_count: Arc::new(AtomicU32::new(0)),
        }
    }

    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = format!("0.0.0.0:{}", self.config.control_port).parse()?;
        let node_id = self.config.node_id.clone();
        let shutdown_notify = self.shutdown_notify.clone();

        let svc = TestNodeServiceServer::new(self);

        tracing::info!(node_id = %node_id, addr = %addr, "Test node starting");

        tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_shutdown(addr, async move {
                shutdown_notify.notified().await;
            })
            .await?;

        Ok(())
    }

    pub fn emit_event(&self, event_type: &str, detail: &str) {
        let event = NodeEvent {
            event_type: event_type.to_string(),
            detail: detail.to_string(),
            timestamp_ms: self.start_time.elapsed().as_millis() as u64,
        };
        let _ = self.event_tx.send(event);
    }
}

#[tonic::async_trait]
impl TestNodeService for TestNode {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        let req = request.into_inner();
        Ok(Response::new(PingResponse {
            echo: req.echo,
            node_id: self.config.node_id.clone(),
            uptime_ms: self.start_time.elapsed().as_millis() as u64,
        }))
    }

    async fn get_node_info(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<NodeInfoResponse>, Status> {
        Ok(Response::new(NodeInfoResponse {
            node_id: self.config.node_id.clone(),
            uptime_ms: self.start_time.elapsed().as_millis() as u64,
            adapter: "none".to_string(),
            actor_count: self.actor_count.load(Ordering::Relaxed),
        }))
    }

    async fn shutdown(&self, request: Request<ShutdownRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        self.shutdown_flag.store(true, Ordering::SeqCst);
        self.emit_event(
            "node_shutdown",
            &serde_json::json!({
                "graceful": req.graceful,
                "timeout_ms": req.timeout_ms,
            })
            .to_string(),
        );
        // Signal the server to shut down gracefully
        self.shutdown_notify.notify_one();
        Ok(Response::new(Empty {}))
    }

    async fn inject_fault(
        &self,
        request: Request<FaultRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        self.fault_injector
            .add_fault(&req.fault_type, &req.target, req.duration_ms, req.count);
        self.emit_event(
            "fault_injected",
            &serde_json::json!({
                "fault_type": req.fault_type,
                "target": req.target,
            })
            .to_string(),
        );
        Ok(Response::new(Empty {}))
    }

    async fn clear_faults(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.fault_injector.clear_all();
        self.emit_event("faults_cleared", "{}");
        Ok(Response::new(Empty {}))
    }

    type SubscribeEventsStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<NodeEvent, Status>> + Send>>;

    async fn subscribe_events(
        &self,
        request: Request<EventFilter>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        let filter = request.into_inner();
        let event_types = filter.event_types;
        let rx = self.event_tx.subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(|result| result.ok())
            .filter(move |event| event_types.is_empty() || event_types.contains(&event.event_type))
            .map(|event| Ok(event));
        Ok(Response::new(Box::pin(stream)))
    }

    async fn custom_command(
        &self,
        request: Request<CustomRequest>,
    ) -> Result<Response<CustomResponse>, Status> {
        let req = request.into_inner();
        Err(Status::unimplemented(format!(
            "custom command '{}' not registered",
            req.command_type
        )))
    }
}
