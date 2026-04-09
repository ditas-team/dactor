//! AWS node discovery for the dactor distributed actor framework.
//!
//! Provides two discovery mechanisms:
//! - [`AutoScalingDiscovery`]: Lists instances in an EC2 Auto Scaling Group.
//! - [`Ec2TagDiscovery`]: Queries EC2 instances by tag key/value filters.

use dactor::{ClusterDiscovery, DiscoveryError};
use std::fmt;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by AWS discovery operations.
#[derive(Debug)]
pub enum AwsDiscoveryError {
    /// Error from the Auto Scaling API.
    AutoScaling(String),
    /// Error from the EC2 API.
    Ec2(String),
    /// No instances matched the discovery criteria.
    NoInstances,
    /// Invalid or missing configuration.
    Config(String),
}

impl fmt::Display for AwsDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AwsDiscoveryError::AutoScaling(e) => write!(f, "Auto Scaling API error: {e}"),
            AwsDiscoveryError::Ec2(e) => write!(f, "EC2 API error: {e}"),
            AwsDiscoveryError::NoInstances => write!(f, "no instances found"),
            AwsDiscoveryError::Config(e) => write!(f, "configuration error: {e}"),
        }
    }
}

impl std::error::Error for AwsDiscoveryError {}

// ---------------------------------------------------------------------------
// ASG configuration
// ---------------------------------------------------------------------------

/// Configuration for Auto Scaling Group discovery.
#[derive(Debug, Clone)]
pub struct AsgDiscoveryConfig {
    /// Name of the Auto Scaling Group.
    pub asg_name: String,
    /// Port to append to each discovered IP address.
    pub port: u16,
    /// AWS region override. Uses the SDK default chain when `None`.
    pub region: Option<String>,
    /// When `true`, return public IPs instead of private IPs.
    pub use_public_ip: bool,
}

impl Default for AsgDiscoveryConfig {
    fn default() -> Self {
        Self {
            asg_name: String::new(),
            port: 9000,
            region: None,
            use_public_ip: false,
        }
    }
}

// ---------------------------------------------------------------------------
// AutoScalingDiscovery
// ---------------------------------------------------------------------------

/// Discovers peer nodes by listing instances in an EC2 Auto Scaling Group.
///
/// Filters for `InService` + healthy instances and returns their private
/// (or public) IP addresses with the configured port.
pub struct AutoScalingDiscovery {
    config: AsgDiscoveryConfig,
}

impl AutoScalingDiscovery {
    /// Returns a new builder with default configuration.
    pub fn builder() -> AsgDiscoveryBuilder {
        AsgDiscoveryBuilder {
            config: AsgDiscoveryConfig::default(),
        }
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &AsgDiscoveryConfig {
        &self.config
    }

    /// Asynchronously discover peer addresses from the Auto Scaling Group.
    pub async fn discover_async(&self) -> Result<Vec<String>, AwsDiscoveryError> {
        if self.config.asg_name.is_empty() {
            return Err(AwsDiscoveryError::Config(
                "asg_name must not be empty".to_string(),
            ));
        }

        let mut config_loader =
            aws_config::defaults(aws_config::BehaviorVersion::latest());
        if let Some(region) = &self.config.region {
            config_loader =
                config_loader.region(aws_config::Region::new(region.clone()));
        }
        let sdk_config = config_loader.load().await;

        // 1. List instances in the ASG.
        let asg_client = aws_sdk_autoscaling::Client::new(&sdk_config);
        let asg_resp = asg_client
            .describe_auto_scaling_groups()
            .auto_scaling_group_names(&self.config.asg_name)
            .send()
            .await
            .map_err(|e| AwsDiscoveryError::AutoScaling(e.to_string()))?;

        let asg = asg_resp
            .auto_scaling_groups()
            .first()
            .ok_or(AwsDiscoveryError::NoInstances)?;

        let instance_ids: Vec<String> = asg
            .instances()
            .iter()
            .filter(|i| {
                i.lifecycle_state()
                    .map(|s| s.as_str() == "InService")
                    .unwrap_or(false)
                    && i.health_status().map(|h| h == "Healthy").unwrap_or(false)
            })
            .filter_map(|i| i.instance_id().map(String::from))
            .collect();

        if instance_ids.is_empty() {
            return Err(AwsDiscoveryError::NoInstances);
        }

        // 2. Describe instances to get their IP addresses.
        let ec2_client = aws_sdk_ec2::Client::new(&sdk_config);
        let ec2_resp = ec2_client
            .describe_instances()
            .set_instance_ids(Some(instance_ids))
            .send()
            .await
            .map_err(|e| AwsDiscoveryError::Ec2(e.to_string()))?;

        let mut addresses = Vec::new();
        for reservation in ec2_resp.reservations() {
            for instance in reservation.instances() {
                let ip = if self.config.use_public_ip {
                    instance.public_ip_address()
                } else {
                    instance.private_ip_address()
                };
                if let Some(ip) = ip {
                    addresses.push(format!("{ip}:{}", self.config.port));
                }
            }
        }

        Ok(addresses)
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for AutoScalingDiscovery {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError> {
        self.discover_async()
            .await
            .map_err(|e| DiscoveryError::new(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// ASG Builder
// ---------------------------------------------------------------------------

/// Builder for [`AutoScalingDiscovery`].
pub struct AsgDiscoveryBuilder {
    config: AsgDiscoveryConfig,
}

impl AsgDiscoveryBuilder {
    /// Set the Auto Scaling Group name.
    pub fn asg_name(mut self, name: &str) -> Self {
        self.config.asg_name = name.to_string();
        self
    }

    /// Set the port number (default: `9000`).
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set an explicit AWS region override.
    pub fn region(mut self, region: &str) -> Self {
        self.config.region = Some(region.to_string());
        self
    }

    /// Return public IPs instead of private IPs.
    pub fn use_public_ip(mut self, yes: bool) -> Self {
        self.config.use_public_ip = yes;
        self
    }

    /// Build the [`AutoScalingDiscovery`] instance.
    pub fn build(self) -> AutoScalingDiscovery {
        AutoScalingDiscovery {
            config: self.config,
        }
    }
}

// ---------------------------------------------------------------------------
// EC2 Tag configuration
// ---------------------------------------------------------------------------

/// Configuration for EC2 tag-based discovery.
#[derive(Debug, Clone)]
pub struct Ec2TagConfig {
    /// Tag key to filter on (e.g., `"dactor-cluster"`).
    pub tag_key: String,
    /// Tag value to match (e.g., `"my-cluster"`).
    pub tag_value: String,
    /// Port to append to each discovered IP address.
    pub port: u16,
    /// AWS region override. Uses the SDK default chain when `None`.
    pub region: Option<String>,
    /// When `true`, return public IPs instead of private IPs.
    pub use_public_ip: bool,
}

impl Default for Ec2TagConfig {
    fn default() -> Self {
        Self {
            tag_key: String::new(),
            tag_value: String::new(),
            port: 9000,
            region: None,
            use_public_ip: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Ec2TagDiscovery
// ---------------------------------------------------------------------------

/// Discovers peer nodes by querying EC2 instances with matching tags.
///
/// Uses the `DescribeInstances` API with tag filters and only returns
/// instances in the `running` state.
pub struct Ec2TagDiscovery {
    config: Ec2TagConfig,
}

impl Ec2TagDiscovery {
    /// Returns a new builder with default configuration.
    pub fn builder() -> Ec2TagDiscoveryBuilder {
        Ec2TagDiscoveryBuilder {
            config: Ec2TagConfig::default(),
        }
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &Ec2TagConfig {
        &self.config
    }

    /// Asynchronously discover peer addresses by EC2 tags.
    pub async fn discover_async(&self) -> Result<Vec<String>, AwsDiscoveryError> {
        if self.config.tag_key.is_empty() {
            return Err(AwsDiscoveryError::Config(
                "tag_key must not be empty".to_string(),
            ));
        }

        let mut config_loader =
            aws_config::defaults(aws_config::BehaviorVersion::latest());
        if let Some(region) = &self.config.region {
            config_loader =
                config_loader.region(aws_config::Region::new(region.clone()));
        }
        let sdk_config = config_loader.load().await;

        let ec2_client = aws_sdk_ec2::Client::new(&sdk_config);

        let tag_filter = aws_sdk_ec2::types::Filter::builder()
            .name(format!("tag:{}", self.config.tag_key))
            .values(&self.config.tag_value)
            .build();

        let running_filter = aws_sdk_ec2::types::Filter::builder()
            .name("instance-state-name")
            .values("running")
            .build();

        let resp = ec2_client
            .describe_instances()
            .filters(tag_filter)
            .filters(running_filter)
            .send()
            .await
            .map_err(|e| AwsDiscoveryError::Ec2(e.to_string()))?;

        let mut addresses = Vec::new();
        for reservation in resp.reservations() {
            for instance in reservation.instances() {
                let ip = if self.config.use_public_ip {
                    instance.public_ip_address()
                } else {
                    instance.private_ip_address()
                };
                if let Some(ip) = ip {
                    addresses.push(format!("{ip}:{}", self.config.port));
                }
            }
        }

        if addresses.is_empty() {
            return Err(AwsDiscoveryError::NoInstances);
        }

        Ok(addresses)
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for Ec2TagDiscovery {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError> {
        self.discover_async()
            .await
            .map_err(|e| DiscoveryError::new(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// EC2 Tag Builder
// ---------------------------------------------------------------------------

/// Builder for [`Ec2TagDiscovery`].
pub struct Ec2TagDiscoveryBuilder {
    config: Ec2TagConfig,
}

impl Ec2TagDiscoveryBuilder {
    /// Set the tag key to filter on.
    pub fn tag_key(mut self, key: &str) -> Self {
        self.config.tag_key = key.to_string();
        self
    }

    /// Set the tag value to match.
    pub fn tag_value(mut self, value: &str) -> Self {
        self.config.tag_value = value.to_string();
        self
    }

    /// Set the port number (default: `9000`).
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set an explicit AWS region override.
    pub fn region(mut self, region: &str) -> Self {
        self.config.region = Some(region.to_string());
        self
    }

    /// Return public IPs instead of private IPs.
    pub fn use_public_ip(mut self, yes: bool) -> Self {
        self.config.use_public_ip = yes;
        self
    }

    /// Build the [`Ec2TagDiscovery`] instance.
    pub fn build(self) -> Ec2TagDiscovery {
        Ec2TagDiscovery {
            config: self.config,
        }
    }
}

// ---------------------------------------------------------------------------
// Environment helpers
// ---------------------------------------------------------------------------

/// Read the current instance's private IP from the `DACTOR_INSTANCE_IP`
/// environment variable.
pub fn instance_private_ip() -> Option<String> {
    std::env::var("DACTOR_INSTANCE_IP").ok()
}

/// Read the current instance's ID from the `DACTOR_INSTANCE_ID`
/// environment variable.
pub fn instance_id() -> Option<String> {
    std::env::var("DACTOR_INSTANCE_ID").ok()
}

/// Read the current AWS region from `AWS_REGION` or `AWS_DEFAULT_REGION`.
pub fn current_region() -> Option<String> {
    std::env::var("AWS_REGION")
        .ok()
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ASG builder --------------------------------------------------------

    #[test]
    fn asg_builder_creates_valid_config() {
        let discovery = AutoScalingDiscovery::builder()
            .asg_name("my-asg")
            .port(8080)
            .region("us-west-2")
            .use_public_ip(true)
            .build();

        assert_eq!(discovery.config().asg_name, "my-asg");
        assert_eq!(discovery.config().port, 8080);
        assert_eq!(discovery.config().region.as_deref(), Some("us-west-2"));
        assert!(discovery.config().use_public_ip);
    }

    #[test]
    fn asg_builder_default_values() {
        let discovery = AutoScalingDiscovery::builder()
            .asg_name("test-asg")
            .build();

        assert_eq!(discovery.config().asg_name, "test-asg");
        assert_eq!(discovery.config().port, 9000);
        assert!(discovery.config().region.is_none());
        assert!(!discovery.config().use_public_ip);
    }

    #[test]
    fn asg_default_config() {
        let cfg = AsgDiscoveryConfig::default();
        assert!(cfg.asg_name.is_empty());
        assert_eq!(cfg.port, 9000);
        assert!(cfg.region.is_none());
        assert!(!cfg.use_public_ip);
    }

    // -- EC2 tag builder ----------------------------------------------------

    #[test]
    fn ec2_tag_builder_creates_valid_config() {
        let discovery = Ec2TagDiscovery::builder()
            .tag_key("dactor-cluster")
            .tag_value("production")
            .port(7000)
            .region("eu-west-1")
            .use_public_ip(false)
            .build();

        assert_eq!(discovery.config().tag_key, "dactor-cluster");
        assert_eq!(discovery.config().tag_value, "production");
        assert_eq!(discovery.config().port, 7000);
        assert_eq!(discovery.config().region.as_deref(), Some("eu-west-1"));
        assert!(!discovery.config().use_public_ip);
    }

    #[test]
    fn ec2_tag_builder_default_values() {
        let discovery = Ec2TagDiscovery::builder()
            .tag_key("cluster")
            .tag_value("dev")
            .build();

        assert_eq!(discovery.config().port, 9000);
        assert!(discovery.config().region.is_none());
        assert!(!discovery.config().use_public_ip);
    }

    #[test]
    fn ec2_tag_default_config() {
        let cfg = Ec2TagConfig::default();
        assert!(cfg.tag_key.is_empty());
        assert!(cfg.tag_value.is_empty());
        assert_eq!(cfg.port, 9000);
        assert!(cfg.region.is_none());
        assert!(!cfg.use_public_ip);
    }

    // -- Environment helpers ------------------------------------------------

    #[test]
    fn instance_private_ip_returns_none_outside_aws() {
        std::env::remove_var("DACTOR_INSTANCE_IP");
        assert!(instance_private_ip().is_none());
    }

    #[test]
    fn instance_id_returns_none_outside_aws() {
        std::env::remove_var("DACTOR_INSTANCE_ID");
        assert!(instance_id().is_none());
    }

    #[test]
    fn current_region_returns_none_when_unset() {
        // NOTE: env var tests must not race. This test only removes vars,
        // which is safe if no other test is concurrently setting them.
        // The set_var tests are consolidated below.
        std::env::remove_var("AWS_REGION");
        std::env::remove_var("AWS_DEFAULT_REGION");
        assert!(current_region().is_none());
    }

    #[test]
    fn current_region_preference_order() {
        // Consolidated test: avoids env var race between parallel tests.
        // Step 1: AWS_REGION takes precedence over AWS_DEFAULT_REGION
        std::env::set_var("AWS_REGION", "us-east-1");
        std::env::set_var("AWS_DEFAULT_REGION", "eu-west-1");
        assert_eq!(current_region(), Some("us-east-1".to_string()));

        // Step 2: Falls back to AWS_DEFAULT_REGION when AWS_REGION absent
        std::env::remove_var("AWS_REGION");
        assert_eq!(current_region(), Some("eu-west-1".to_string()));

        // Cleanup
        std::env::remove_var("AWS_DEFAULT_REGION");
    }

    // -- Error display ------------------------------------------------------

    #[test]
    fn error_display_autoscaling() {
        let err = AwsDiscoveryError::AutoScaling("timeout".to_string());
        assert_eq!(err.to_string(), "Auto Scaling API error: timeout");
    }

    #[test]
    fn error_display_ec2() {
        let err = AwsDiscoveryError::Ec2("access denied".to_string());
        assert_eq!(err.to_string(), "EC2 API error: access denied");
    }

    #[test]
    fn error_display_no_instances() {
        let err = AwsDiscoveryError::NoInstances;
        assert_eq!(err.to_string(), "no instances found");
    }

    #[test]
    fn error_display_config() {
        let err = AwsDiscoveryError::Config("missing asg_name".to_string());
        assert_eq!(err.to_string(), "configuration error: missing asg_name");
    }
}
