//! Azure node discovery for the dactor distributed actor framework.
//!
//! Provides two discovery mechanisms:
//! - [`VmssDiscovery`]: Discovers peers via Azure VMSS using the Instance Metadata Service (IMDS).
//! - [`AzureTagDiscovery`]: Discovers peers by querying Azure VMs with matching tags via the ARM API.

use dactor::{ClusterDiscovery, DiscoveryError};
use std::fmt;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by Azure discovery operations.
#[derive(Debug)]
pub enum AzureDiscoveryError {
    /// Error from the Instance Metadata Service.
    ImdsError(String),
    /// Error from the Azure Resource Manager API.
    ArmApiError(String),
    /// Error from the HTTP client.
    HttpError(reqwest::Error),
    /// Failed to parse an API response.
    ParseError(String),
    /// Invalid or missing configuration.
    Config(String),
}

impl fmt::Display for AzureDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AzureDiscoveryError::ImdsError(e) => write!(f, "IMDS error: {e}"),
            AzureDiscoveryError::ArmApiError(e) => write!(f, "ARM API error: {e}"),
            AzureDiscoveryError::HttpError(e) => write!(f, "HTTP error: {e}"),
            AzureDiscoveryError::ParseError(e) => write!(f, "parse error: {e}"),
            AzureDiscoveryError::Config(e) => write!(f, "configuration error: {e}"),
        }
    }
}

impl std::error::Error for AzureDiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AzureDiscoveryError::HttpError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for AzureDiscoveryError {
    fn from(e: reqwest::Error) -> Self {
        AzureDiscoveryError::HttpError(e)
    }
}

// ---------------------------------------------------------------------------
// IMDS response types
// ---------------------------------------------------------------------------

/// Subset of the Azure IMDS instance metadata response.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImdsResponse {
    compute: ImdsCompute,
}

/// Compute section of the IMDS response.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImdsCompute {
    subscription_id: String,
    resource_group_name: String,
    #[serde(default)]
    vmss_name: Option<String>,
}

// ---------------------------------------------------------------------------
// ARM API response types
// ---------------------------------------------------------------------------

/// Paginated response from the ARM API.
#[derive(Debug, Clone, serde::Deserialize)]
struct ArmListResponse<T> {
    value: Vec<T>,
    #[serde(default, rename = "nextLink")]
    next_link: Option<String>,
}

/// A VMSS network interface from the ARM API.
#[derive(Debug, Clone, serde::Deserialize)]
struct ArmNetworkInterface {
    properties: ArmNicProperties,
}

/// Properties of a VMSS network interface.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArmNicProperties {
    ip_configurations: Vec<ArmIpConfiguration>,
}

/// An IP configuration within a network interface.
#[derive(Debug, Clone, serde::Deserialize)]
struct ArmIpConfiguration {
    properties: ArmIpConfigProperties,
}

/// Properties of an IP configuration.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArmIpConfigProperties {
    private_ip_address: Option<String>,
}

// ---------------------------------------------------------------------------
// Managed Identity token
// ---------------------------------------------------------------------------

/// Response from the IMDS token endpoint.
#[derive(Debug, Clone, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Acquire a Managed Identity access token from IMDS for the ARM resource.
async fn acquire_managed_identity_token(
    client: &reqwest::Client,
) -> Result<String, AzureDiscoveryError> {
    let resp = client
        .get("http://169.254.169.254/metadata/identity/oauth2/token")
        .header("Metadata", "true")
        .query(&[
            ("api-version", "2019-08-01"),
            ("resource", "https://management.azure.com/"),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AzureDiscoveryError::ImdsError(format!(
            "token request failed ({status}): {body}"
        )));
    }

    let token: TokenResponse = resp
        .json()
        .await
        .map_err(|e| AzureDiscoveryError::ParseError(format!("token response: {e}")))?;

    Ok(token.access_token)
}

// ---------------------------------------------------------------------------
// IMDS helpers
// ---------------------------------------------------------------------------

const IMDS_BASE: &str = "http://169.254.169.254";
const IMDS_API_VERSION: &str = "2021-02-01";
const ARM_API_VERSION_NIC: &str = "2023-09-01";
const ARM_API_VERSION_VM: &str = "2023-09-01";

/// Query Azure IMDS for the current VM's metadata.
async fn query_imds(client: &reqwest::Client) -> Result<ImdsResponse, AzureDiscoveryError> {
    let resp = client
        .get(format!("{IMDS_BASE}/metadata/instance"))
        .header("Metadata", "true")
        .query(&[("api-version", IMDS_API_VERSION)])
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AzureDiscoveryError::ImdsError(format!(
            "IMDS returned {status}: {body}"
        )));
    }

    resp.json()
        .await
        .map_err(|e| AzureDiscoveryError::ParseError(format!("IMDS response: {e}")))
}

/// Get the current VM's subscription ID from IMDS.
pub async fn current_subscription_id() -> Option<String> {
    let client = reqwest::Client::new();
    query_imds(&client)
        .await
        .ok()
        .map(|r| r.compute.subscription_id)
}

/// Get the current VM's resource group name from IMDS.
pub async fn current_resource_group() -> Option<String> {
    let client = reqwest::Client::new();
    query_imds(&client)
        .await
        .ok()
        .map(|r| r.compute.resource_group_name)
}

/// Returns the IMDS instance metadata URL for reference / diagnostics.
pub fn imds_instance_url() -> String {
    format!(
        "{IMDS_BASE}/metadata/instance?api-version={IMDS_API_VERSION}"
    )
}

// ---------------------------------------------------------------------------
// VMSS Discovery configuration
// ---------------------------------------------------------------------------

/// Configuration for VMSS-based discovery.
#[derive(Debug, Clone)]
pub struct VmssDiscoveryConfig {
    /// Port to append to each discovered IP address.
    pub port: u16,
    /// When `true`, use IMDS to automatically resolve the subscription,
    /// resource group, and VMSS name of the current VM.
    pub use_imds: bool,
    /// Explicit subscription ID override (skips IMDS for this value).
    pub subscription_id: Option<String>,
    /// Explicit resource group override (skips IMDS for this value).
    pub resource_group: Option<String>,
    /// Explicit VMSS name override (skips IMDS for this value).
    pub vmss_name: Option<String>,
}

impl Default for VmssDiscoveryConfig {
    fn default() -> Self {
        Self {
            port: 9000,
            use_imds: true,
            subscription_id: None,
            resource_group: None,
            vmss_name: None,
        }
    }
}

// ---------------------------------------------------------------------------
// VmssDiscovery
// ---------------------------------------------------------------------------

/// Discovers peer nodes via Azure Virtual Machine Scale Set (VMSS).
///
/// Uses the Azure Instance Metadata Service (IMDS) to determine the current
/// VM's VMSS, then queries the ARM API to list all network interfaces in the
/// scale set and extract their private IP addresses.
///
/// Requires a Managed Identity with **Reader** role on the VMSS resource.
pub struct VmssDiscovery {
    config: VmssDiscoveryConfig,
    client: reqwest::Client,
}

impl VmssDiscovery {
    /// Returns a new builder with default configuration.
    pub fn builder() -> VmssDiscoveryBuilder {
        VmssDiscoveryBuilder {
            config: VmssDiscoveryConfig::default(),
        }
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &VmssDiscoveryConfig {
        &self.config
    }

    /// Resolve the subscription ID, resource group, and VMSS name — either
    /// from explicit config or by querying IMDS.
    async fn resolve_vmss_info(
        &self,
    ) -> Result<(String, String, String), AzureDiscoveryError> {
        if let (Some(sub), Some(rg), Some(vmss)) = (
            self.config.subscription_id.clone(),
            self.config.resource_group.clone(),
            self.config.vmss_name.clone(),
        ) {
            return Ok((sub, rg, vmss));
        }

        if !self.config.use_imds {
            return Err(AzureDiscoveryError::Config(
                "use_imds is false but subscription_id, resource_group, or vmss_name is missing"
                    .to_string(),
            ));
        }

        let imds = query_imds(&self.client).await?;
        let sub = self
            .config
            .subscription_id
            .clone()
            .unwrap_or(imds.compute.subscription_id);
        let rg = self
            .config
            .resource_group
            .clone()
            .unwrap_or(imds.compute.resource_group_name);
        let vmss = self.config.vmss_name.clone().or(imds.compute.vmss_name).ok_or_else(
            || {
                AzureDiscoveryError::ImdsError(
                    "current VM is not part of a VMSS".to_string(),
                )
            },
        )?;

        Ok((sub, rg, vmss))
    }

    /// Discover peer addresses from the VMSS.
    pub async fn discover_instances(&self) -> Result<Vec<String>, AzureDiscoveryError> {
        let (subscription_id, resource_group, vmss_name) =
            self.resolve_vmss_info().await?;

        let token = acquire_managed_identity_token(&self.client).await?;

        let url = format!(
            "https://management.azure.com/subscriptions/{subscription_id}\
             /resourceGroups/{resource_group}\
             /providers/Microsoft.Compute/virtualMachineScaleSets/{vmss_name}\
             /networkInterfaces?api-version={ARM_API_VERSION_NIC}"
        );

        let mut addresses = Vec::new();
        let mut next_url: Option<String> = Some(url);

        while let Some(page_url) = next_url.take() {
            let resp = self
                .client
                .get(&page_url)
                .bearer_auth(&token)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(AzureDiscoveryError::ArmApiError(format!(
                    "list NICs failed ({status}): {body}"
                )));
            }

            let page: ArmListResponse<ArmNetworkInterface> = resp
                .json()
                .await
                .map_err(|e| AzureDiscoveryError::ParseError(format!("NIC list: {e}")))?;

            for nic in &page.value {
                for ip_config in &nic.properties.ip_configurations {
                    if let Some(ip) = &ip_config.properties.private_ip_address {
                        addresses.push(format!("{ip}:{}", self.config.port));
                    }
                }
            }

            next_url = page.next_link;
        }

        tracing::debug!(count = addresses.len(), "VMSS discovery complete");
        Ok(addresses)
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for VmssDiscovery {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError> {
        self.discover_instances()
            .await
            .map_err(|e| DiscoveryError::new(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// VMSS Builder
// ---------------------------------------------------------------------------

/// Builder for [`VmssDiscovery`].
pub struct VmssDiscoveryBuilder {
    config: VmssDiscoveryConfig,
}

impl VmssDiscoveryBuilder {
    /// Set the port number (default: `9000`).
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Enable or disable IMDS auto-detection (default: `true`).
    pub fn use_imds(mut self, yes: bool) -> Self {
        self.config.use_imds = yes;
        self
    }

    /// Set an explicit subscription ID (overrides IMDS).
    pub fn subscription_id(mut self, id: &str) -> Self {
        self.config.subscription_id = Some(id.to_string());
        self
    }

    /// Set an explicit resource group (overrides IMDS).
    pub fn resource_group(mut self, rg: &str) -> Self {
        self.config.resource_group = Some(rg.to_string());
        self
    }

    /// Set an explicit VMSS name (overrides IMDS).
    pub fn vmss_name(mut self, name: &str) -> Self {
        self.config.vmss_name = Some(name.to_string());
        self
    }

    /// Build the [`VmssDiscovery`] instance.
    pub fn build(self) -> VmssDiscovery {
        VmssDiscovery {
            config: self.config,
            client: reqwest::Client::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Azure Tag Discovery configuration
// ---------------------------------------------------------------------------

/// Configuration for tag-based Azure VM discovery.
#[derive(Debug, Clone)]
pub struct AzureTagConfig {
    /// Tag key to filter on (e.g., `"dactor-cluster"`).
    pub tag_key: String,
    /// Tag value to match (e.g., `"production"`).
    pub tag_value: String,
    /// Port to append to each discovered IP address.
    pub port: u16,
    /// Azure subscription ID. When `None`, it is resolved from IMDS.
    pub subscription_id: Option<String>,
    /// Azure resource group. When `None`, all resource groups are searched.
    pub resource_group: Option<String>,
}

impl Default for AzureTagConfig {
    fn default() -> Self {
        Self {
            tag_key: String::new(),
            tag_value: String::new(),
            port: 9000,
            subscription_id: None,
            resource_group: None,
        }
    }
}

// ---------------------------------------------------------------------------
// AzureTagDiscovery
// ---------------------------------------------------------------------------

/// Discovers peer nodes by querying Azure VMs with matching tags.
///
/// Uses the Azure Resource Manager API to list VMs filtered by a tag
/// key/value pair and extracts their private IP addresses from the
/// associated network interfaces.
///
/// Requires a Managed Identity with **Reader** role on the target resources.
pub struct AzureTagDiscovery {
    config: AzureTagConfig,
    client: reqwest::Client,
}

impl AzureTagDiscovery {
    /// Returns a new builder with default configuration.
    pub fn builder() -> AzureTagDiscoveryBuilder {
        AzureTagDiscoveryBuilder {
            config: AzureTagConfig::default(),
        }
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &AzureTagConfig {
        &self.config
    }

    /// Resolve the subscription ID — from explicit config or IMDS.
    async fn resolve_subscription(&self) -> Result<String, AzureDiscoveryError> {
        if let Some(sub) = &self.config.subscription_id {
            return Ok(sub.clone());
        }

        let imds = query_imds(&self.client).await?;
        Ok(imds.compute.subscription_id)
    }

    /// Discover peer addresses by tag.
    pub async fn discover_by_tag(&self) -> Result<Vec<String>, AzureDiscoveryError> {
        if self.config.tag_key.is_empty() {
            return Err(AzureDiscoveryError::Config(
                "tag_key must not be empty".to_string(),
            ));
        }

        let subscription_id = self.resolve_subscription().await?;
        let token = acquire_managed_identity_token(&self.client).await?;

        // Build the VM list URL, optionally scoped to a resource group.
        let base_url = if let Some(rg) = &self.config.resource_group {
            format!(
                "https://management.azure.com/subscriptions/{subscription_id}\
                 /resourceGroups/{rg}\
                 /providers/Microsoft.Compute/virtualMachines\
                 ?api-version={ARM_API_VERSION_VM}"
            )
        } else {
            format!(
                "https://management.azure.com/subscriptions/{subscription_id}\
                 /providers/Microsoft.Compute/virtualMachines\
                 ?api-version={ARM_API_VERSION_VM}"
            )
        };

        let mut addresses = Vec::new();
        let mut next_url: Option<String> = Some(base_url);

        while let Some(page_url) = next_url.take() {
            let resp = self
                .client
                .get(&page_url)
                .bearer_auth(&token)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(AzureDiscoveryError::ArmApiError(format!(
                    "list VMs failed ({status}): {body}"
                )));
            }

            let page: ArmListResponse<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| AzureDiscoveryError::ParseError(format!("VM list: {e}")))?;

            for vm in &page.value {
                // Check the tag matches.
                let tags = vm.get("tags").and_then(|t| t.as_object());
                let matches = tags
                    .and_then(|t| t.get(&self.config.tag_key))
                    .and_then(|v| v.as_str())
                    .map(|v| v == self.config.tag_value)
                    .unwrap_or(false);

                if !matches {
                    continue;
                }

                // Extract NIC resource ID and fetch the NIC details.
                if let Some(nic_id) = vm
                    .pointer("/properties/networkProfile/networkInterfaces/0/id")
                    .and_then(|v| v.as_str())
                {
                    if let Ok(ip) = self.fetch_nic_private_ip(nic_id, &token).await {
                        addresses.push(format!("{ip}:{}", self.config.port));
                    }
                }
            }

            next_url = page.next_link;
        }

        tracing::debug!(count = addresses.len(), "Azure tag discovery complete");
        Ok(addresses)
    }

    /// Fetch the primary private IP of a NIC by its ARM resource ID.
    async fn fetch_nic_private_ip(
        &self,
        nic_id: &str,
        token: &str,
    ) -> Result<String, AzureDiscoveryError> {
        let url = format!(
            "https://management.azure.com{nic_id}?api-version={ARM_API_VERSION_NIC}"
        );

        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AzureDiscoveryError::ArmApiError(format!(
                "get NIC failed ({status}): {body}"
            )));
        }

        let nic: ArmNetworkInterface = resp
            .json()
            .await
            .map_err(|e| AzureDiscoveryError::ParseError(format!("NIC details: {e}")))?;

        nic.properties
            .ip_configurations
            .first()
            .and_then(|c| c.properties.private_ip_address.clone())
            .ok_or_else(|| {
                AzureDiscoveryError::ArmApiError(
                    "NIC has no private IP configuration".to_string(),
                )
            })
    }
}

#[async_trait::async_trait]
impl ClusterDiscovery for AzureTagDiscovery {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError> {
        self.discover_by_tag()
            .await
            .map_err(|e| DiscoveryError::new(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Azure Tag Builder
// ---------------------------------------------------------------------------

/// Builder for [`AzureTagDiscovery`].
pub struct AzureTagDiscoveryBuilder {
    config: AzureTagConfig,
}

impl AzureTagDiscoveryBuilder {
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

    /// Set an explicit subscription ID (overrides IMDS).
    pub fn subscription_id(mut self, id: &str) -> Self {
        self.config.subscription_id = Some(id.to_string());
        self
    }

    /// Set an explicit resource group.
    pub fn resource_group(mut self, rg: &str) -> Self {
        self.config.resource_group = Some(rg.to_string());
        self
    }

    /// Build the [`AzureTagDiscovery`] instance.
    pub fn build(self) -> AzureTagDiscovery {
        AzureTagDiscovery {
            config: self.config,
            client: reqwest::Client::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Environment helpers
// ---------------------------------------------------------------------------

/// Read the current VM's private IP from the `DACTOR_VM_IP` environment variable.
pub fn vm_private_ip() -> Option<String> {
    std::env::var("DACTOR_VM_IP").ok()
}

/// Read the Azure subscription ID from the `AZURE_SUBSCRIPTION_ID` environment variable.
pub fn subscription_id() -> Option<String> {
    std::env::var("AZURE_SUBSCRIPTION_ID").ok()
}

/// Read the Azure resource group from the `AZURE_RESOURCE_GROUP` environment variable.
pub fn resource_group() -> Option<String> {
    std::env::var("AZURE_RESOURCE_GROUP").ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- VMSS builder -------------------------------------------------------

    #[test]
    fn vmss_builder_creates_valid_config() {
        let discovery = VmssDiscovery::builder()
            .port(8080)
            .use_imds(false)
            .subscription_id("sub-123")
            .resource_group("my-rg")
            .vmss_name("my-vmss")
            .build();

        assert_eq!(discovery.config().port, 8080);
        assert!(!discovery.config().use_imds);
        assert_eq!(
            discovery.config().subscription_id.as_deref(),
            Some("sub-123")
        );
        assert_eq!(
            discovery.config().resource_group.as_deref(),
            Some("my-rg")
        );
        assert_eq!(
            discovery.config().vmss_name.as_deref(),
            Some("my-vmss")
        );
    }

    #[test]
    fn vmss_builder_default_values() {
        let discovery = VmssDiscovery::builder().build();

        assert_eq!(discovery.config().port, 9000);
        assert!(discovery.config().use_imds);
        assert!(discovery.config().subscription_id.is_none());
        assert!(discovery.config().resource_group.is_none());
        assert!(discovery.config().vmss_name.is_none());
    }

    #[test]
    fn vmss_default_config() {
        let cfg = VmssDiscoveryConfig::default();
        assert_eq!(cfg.port, 9000);
        assert!(cfg.use_imds);
        assert!(cfg.subscription_id.is_none());
        assert!(cfg.resource_group.is_none());
        assert!(cfg.vmss_name.is_none());
    }

    // -- Azure Tag builder --------------------------------------------------

    #[test]
    fn tag_builder_creates_valid_config() {
        let discovery = AzureTagDiscovery::builder()
            .tag_key("dactor-cluster")
            .tag_value("production")
            .port(7000)
            .subscription_id("sub-456")
            .resource_group("prod-rg")
            .build();

        assert_eq!(discovery.config().tag_key, "dactor-cluster");
        assert_eq!(discovery.config().tag_value, "production");
        assert_eq!(discovery.config().port, 7000);
        assert_eq!(
            discovery.config().subscription_id.as_deref(),
            Some("sub-456")
        );
        assert_eq!(
            discovery.config().resource_group.as_deref(),
            Some("prod-rg")
        );
    }

    #[test]
    fn tag_builder_default_values() {
        let discovery = AzureTagDiscovery::builder()
            .tag_key("cluster")
            .tag_value("dev")
            .build();

        assert_eq!(discovery.config().port, 9000);
        assert!(discovery.config().subscription_id.is_none());
        assert!(discovery.config().resource_group.is_none());
    }

    #[test]
    fn tag_default_config() {
        let cfg = AzureTagConfig::default();
        assert!(cfg.tag_key.is_empty());
        assert!(cfg.tag_value.is_empty());
        assert_eq!(cfg.port, 9000);
        assert!(cfg.subscription_id.is_none());
        assert!(cfg.resource_group.is_none());
    }

    // -- Environment helpers ------------------------------------------------

    #[test]
    fn vm_private_ip_returns_none_outside_azure() {
        std::env::remove_var("DACTOR_VM_IP");
        assert!(vm_private_ip().is_none());
    }

    #[test]
    fn subscription_id_returns_none_outside_azure() {
        std::env::remove_var("AZURE_SUBSCRIPTION_ID");
        assert!(subscription_id().is_none());
    }

    #[test]
    fn resource_group_returns_none_outside_azure() {
        std::env::remove_var("AZURE_RESOURCE_GROUP");
        assert!(resource_group().is_none());
    }

    // -- Error display ------------------------------------------------------

    #[test]
    fn error_display_imds() {
        let err = AzureDiscoveryError::ImdsError("timeout".to_string());
        assert_eq!(err.to_string(), "IMDS error: timeout");
    }

    #[test]
    fn error_display_arm_api() {
        let err = AzureDiscoveryError::ArmApiError("403 forbidden".to_string());
        assert_eq!(err.to_string(), "ARM API error: 403 forbidden");
    }

    #[test]
    fn error_display_parse() {
        let err = AzureDiscoveryError::ParseError("invalid json".to_string());
        assert_eq!(err.to_string(), "parse error: invalid json");
    }

    #[test]
    fn error_display_config() {
        let err = AzureDiscoveryError::Config("missing subscription".to_string());
        assert_eq!(err.to_string(), "configuration error: missing subscription");
    }

    // -- IMDS URL formatting ------------------------------------------------

    #[test]
    fn imds_url_contains_api_version() {
        let url = imds_instance_url();
        assert!(url.starts_with("http://169.254.169.254/metadata/instance"));
        assert!(url.contains("api-version=2021-02-01"));
    }
}
