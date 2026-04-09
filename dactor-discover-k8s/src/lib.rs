//! Kubernetes node discovery for the dactor distributed actor framework.
//!
//! Provides two discovery mechanisms:
//! - [`KubernetesDiscovery`]: Uses the Kubernetes API to list pods by label selector.
//! - [`HeadlessServiceDiscovery`]: Uses DNS resolution of a headless Kubernetes service.

use dactor::{ClusterDiscovery, DiscoveryError};
use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api, Client};
use std::fmt;
use std::net::ToSocketAddrs;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by Kubernetes discovery operations.
#[derive(Debug)]
pub enum K8sDiscoveryError {
    /// Error from the Kubernetes API client.
    KubeError(kube::Error),
    /// A pod was found but has no IP assigned.
    NoPodIp(String),
    /// An I/O error (e.g., DNS resolution failure).
    Io(std::io::Error),
}

impl fmt::Display for K8sDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            K8sDiscoveryError::KubeError(e) => write!(f, "Kubernetes API error: {e}"),
            K8sDiscoveryError::NoPodIp(name) => write!(f, "Pod '{name}' has no IP assigned"),
            K8sDiscoveryError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for K8sDiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            K8sDiscoveryError::KubeError(e) => Some(e),
            K8sDiscoveryError::Io(e) => Some(e),
            K8sDiscoveryError::NoPodIp(_) => None,
        }
    }
}

impl From<kube::Error> for K8sDiscoveryError {
    fn from(e: kube::Error) -> Self {
        K8sDiscoveryError::KubeError(e)
    }
}

impl From<std::io::Error> for K8sDiscoveryError {
    fn from(e: std::io::Error) -> Self {
        K8sDiscoveryError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for Kubernetes pod discovery.
#[derive(Debug, Clone)]
pub struct K8sDiscoveryConfig {
    /// Kubernetes namespace to query.
    pub namespace: String,
    /// Label selector to filter pods (e.g., `"app=my-dactor-service"`).
    pub label_selector: String,
    /// Fallback port number when `port_name` is unset or cannot be resolved.
    pub port: u16,
    /// Named container port to look up (e.g., `"dactor"`).
    pub port_name: Option<String>,
}

impl Default for K8sDiscoveryConfig {
    fn default() -> Self {
        Self {
            namespace: current_namespace().unwrap_or_else(|| "default".to_string()),
            label_selector: String::new(),
            port: 9000,
            port_name: None,
        }
    }
}

// ---------------------------------------------------------------------------
// KubernetesDiscovery
// ---------------------------------------------------------------------------

/// Discovers peer pods using the Kubernetes API.
///
/// Queries the API server for pods matching a label selector and extracts
/// their IP addresses.  A tokio runtime must be available (either the
/// caller is inside one, or one is created externally).
pub struct KubernetesDiscovery {
    config: K8sDiscoveryConfig,
}

impl KubernetesDiscovery {
    /// Returns a new builder with default configuration.
    pub fn builder() -> K8sDiscoveryBuilder {
        K8sDiscoveryBuilder {
            config: K8sDiscoveryConfig::default(),
        }
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &K8sDiscoveryConfig {
        &self.config
    }

    /// Asynchronously discover peer pod addresses.
    pub async fn discover_async(&self) -> Result<Vec<String>, K8sDiscoveryError> {
        let client = Client::try_default().await?;
        let pods: Api<Pod> = Api::namespaced(client, &self.config.namespace);
        let lp = ListParams::default().labels(&self.config.label_selector);
        let pod_list = pods.list(&lp).await?;

        let mut addresses = Vec::new();
        for pod in pod_list.items {
            let pod_name = pod
                .metadata
                .name
                .as_deref()
                .unwrap_or("<unknown>");

            let phase = pod
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref());

            if phase != Some("Running") {
                tracing::debug!(pod = pod_name, ?phase, "skipping non-running pod");
                continue;
            }

            let ip = pod
                .status
                .as_ref()
                .and_then(|s| s.pod_ip.as_deref());

            match ip {
                Some(ip) => {
                    let port = self.resolve_port(&pod).unwrap_or(self.config.port);
                    addresses.push(format!("{ip}:{port}"));
                }
                None => {
                    tracing::warn!(pod = pod_name, "pod has no IP assigned");
                }
            }
        }

        Ok(addresses)
    }

    /// Resolve the port for a pod by looking up the configured `port_name`.
    fn resolve_port(&self, pod: &Pod) -> Option<u16> {
        let port_name = self.config.port_name.as_deref()?;
        let spec = pod.spec.as_ref()?;
        for container in &spec.containers {
            if let Some(ports) = &container.ports {
                for p in ports {
                    if p.name.as_deref() == Some(port_name) {
                        return u16::try_from(p.container_port).ok();
                    }
                }
            }
        }
        None
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for KubernetesDiscovery {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError> {
        self.discover_async()
            .await
            .map_err(|e| DiscoveryError::new(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`KubernetesDiscovery`].
pub struct K8sDiscoveryBuilder {
    config: K8sDiscoveryConfig,
}

impl K8sDiscoveryBuilder {
    /// Set the Kubernetes namespace to query.
    pub fn namespace(mut self, ns: &str) -> Self {
        self.config.namespace = ns.to_string();
        self
    }

    /// Set the pod label selector (e.g., `"app=my-dactor-service"`).
    pub fn label_selector(mut self, selector: &str) -> Self {
        self.config.label_selector = selector.to_string();
        self
    }

    /// Set the fallback port number (default: `9000`).
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set the named port to resolve from the pod spec (e.g., `"dactor"`).
    pub fn port_name(mut self, name: &str) -> Self {
        self.config.port_name = Some(name.to_string());
        self
    }

    /// Build the [`KubernetesDiscovery`] instance.
    pub fn build(self) -> KubernetesDiscovery {
        KubernetesDiscovery {
            config: self.config,
        }
    }
}

// ---------------------------------------------------------------------------
// HeadlessServiceDiscovery
// ---------------------------------------------------------------------------

/// Discovers peer pods via DNS resolution of a Kubernetes headless service.
///
/// A headless service (`clusterIP: None`) causes the Kubernetes DNS to return
/// A/AAAA records for every ready pod endpoint.  This discovery method simply
/// resolves that DNS name and returns the resulting addresses.
pub struct HeadlessServiceDiscovery {
    service_name: String,
    namespace: String,
    port: u16,
    cluster_domain: String,
}

impl HeadlessServiceDiscovery {
    /// Create a new headless-service discovery with default cluster domain (`cluster.local`).
    pub fn new(service_name: &str, namespace: &str, port: u16) -> Self {
        Self {
            service_name: service_name.to_string(),
            namespace: namespace.to_string(),
            port,
            cluster_domain: "cluster.local".to_string(),
        }
    }

    /// Override the cluster DNS domain (default: `cluster.local`).
    ///
    /// Some clusters use a custom domain. Set this to match your
    /// cluster's `--cluster-domain` kubelet configuration.
    pub fn with_cluster_domain(mut self, domain: &str) -> Self {
        self.cluster_domain = domain.to_string();
        self
    }

    /// Returns the FQDN used for DNS resolution.
    pub fn dns_name(&self) -> String {
        format!(
            "{}.{}.svc.{}",
            self.service_name, self.namespace, self.cluster_domain
        )
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for HeadlessServiceDiscovery {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError> {
        let dns = self.dns_name();
        let lookup = format!("{dns}:{}", self.port);
        match lookup.to_socket_addrs() {
            Ok(addrs) => Ok(addrs.map(|a| a.to_string()).collect()),
            Err(e) => Err(DiscoveryError::new(format!(
                "DNS resolution failed for {dns}: {e}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Environment helpers
// ---------------------------------------------------------------------------

/// Read the current namespace from the pod's mounted service account.
///
/// Returns `None` when not running inside a Kubernetes pod.
pub fn current_namespace() -> Option<String> {
    std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read the pod's own IP from the `DACTOR_POD_IP` or `POD_IP` environment variable.
pub fn pod_ip() -> Option<String> {
    std::env::var("DACTOR_POD_IP")
        .ok()
        .or_else(|| std::env::var("POD_IP").ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_valid_config() {
        let discovery = KubernetesDiscovery::builder()
            .namespace("production")
            .label_selector("app=my-service")
            .port(8080)
            .port_name("dactor")
            .build();

        assert_eq!(discovery.config().namespace, "production");
        assert_eq!(discovery.config().label_selector, "app=my-service");
        assert_eq!(discovery.config().port, 8080);
        assert_eq!(discovery.config().port_name.as_deref(), Some("dactor"));
    }

    #[test]
    fn builder_default_values() {
        let discovery = KubernetesDiscovery::builder()
            .label_selector("app=test")
            .build();

        // Outside K8s, namespace defaults to "default"
        assert_eq!(discovery.config().namespace, "default");
        assert_eq!(discovery.config().port, 9000);
        assert!(discovery.config().port_name.is_none());
    }

    #[test]
    fn headless_service_dns_formatting() {
        let discovery = HeadlessServiceDiscovery::new("my-service", "production", 9000);
        assert_eq!(
            discovery.dns_name(),
            "my-service.production.svc.cluster.local"
        );
    }

    #[test]
    fn headless_service_dns_default_namespace() {
        let discovery = HeadlessServiceDiscovery::new("dactor-cluster", "default", 9000);
        assert_eq!(
            discovery.dns_name(),
            "dactor-cluster.default.svc.cluster.local"
        );
    }

    #[test]
    fn current_namespace_returns_none_outside_k8s() {
        assert!(current_namespace().is_none());
    }

    #[test]
    fn pod_ip_returns_none_when_env_not_set() {
        std::env::remove_var("DACTOR_POD_IP");
        std::env::remove_var("POD_IP");
        assert!(pod_ip().is_none());
    }

    #[test]
    fn pod_ip_reads_dactor_pod_ip_first() {
        std::env::set_var("DACTOR_POD_IP", "10.0.0.1");
        std::env::set_var("POD_IP", "10.0.0.2");
        assert_eq!(pod_ip(), Some("10.0.0.1".to_string()));
        std::env::remove_var("DACTOR_POD_IP");
        std::env::remove_var("POD_IP");
    }

    #[test]
    fn pod_ip_falls_back_to_pod_ip_env() {
        std::env::remove_var("DACTOR_POD_IP");
        std::env::set_var("POD_IP", "10.0.0.99");
        assert_eq!(pod_ip(), Some("10.0.0.99".to_string()));
        std::env::remove_var("POD_IP");
    }

    #[test]
    fn config_default_outside_k8s() {
        let cfg = K8sDiscoveryConfig::default();
        assert_eq!(cfg.namespace, "default");
        assert_eq!(cfg.port, 9000);
        assert!(cfg.label_selector.is_empty());
        assert!(cfg.port_name.is_none());
    }
}
