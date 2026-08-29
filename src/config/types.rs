//! Public resolved configuration data structures.

use crate::shell::ShellPolicy;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default)]
pub struct CliOverrides {
    pub bridge_port: Option<u16>,
    pub host: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub config_path: Option<String>,
    pub max_body_size: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub egress_mode: Option<String>,
}

impl fmt::Debug for CliOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CliOverrides")
            .field("bridge_port", &self.bridge_port)
            .field("host", &self.host)
            .field("model", &self.model)
            .field("shell_policy", &self.shell_policy)
            .field("config_path", &self.config_path)
            .field("max_body_size", &self.max_body_size)
            .field(
                "tavily_api_key",
                &self.tavily_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "exa_api_key",
                &self.exa_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "serper_api_key",
                &self.serper_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("searxng_url", &self.searxng_url)
            .field(
                "searxng_api_key",
                &self.searxng_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("egress_mode", &self.egress_mode)
            .finish()
    }
}

/// Secret value whose formatting never exposes the underlying bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMode {
    Direct,
    Proxy,
    Hybrid,
}

impl EgressMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "direct" => Some(Self::Direct),
            "proxy" | "warp" => Some(Self::Proxy),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagementConfig {
    pub dashboard_token: Option<SecretString>,
    pub rest_api_token: Option<SecretString>,
    pub config_path: PathBuf,
    pub csrf_enabled: bool,
}

impl ManagementConfig {
    pub fn rest_token(&self) -> Option<&str> {
        self.rest_api_token
            .as_ref()
            .or(self.dashboard_token.as_ref())
            .map(SecretString::expose)
    }

    pub fn dashboard_token(&self) -> Option<&str> {
        self.dashboard_token.as_ref().map(SecretString::expose)
    }
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub upstream_base_url: String,
    pub model_fallbacks: Vec<String>,
    pub default_fallbacks_enabled: bool,
    pub max_network_attempts: usize,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

#[derive(Debug, Clone)]
pub struct EgressConfig {
    pub mode: EgressMode,
    pub active_proxy_count: usize,
    pub require_verified_exit_ip: bool,
    pub minimum_unique_exit_ips: usize,
    pub identity_endpoints: Vec<String>,
    pub identity_ttl: Duration,
    pub health_interval: Duration,
    pub restart_interval: Duration,
    pub max_restart_attempts: u32,
    pub allow_direct_fallback: bool,
    /// True when at least one of `BRIDGE_PRIMARY_PROXIES`, legacy
    /// `BRIDGE_PROXIES`, TOML `primary_proxies`, or TOML `proxies` was
    /// explicitly provided by the operator AND parsed to at least one proxy.
    ///
    /// The loader always materializes a built-in WARP default pool when
    /// nothing is configured (other components depend on a non-empty pool);
    /// this flag lets security validation distinguish that silent
    /// inheritance from deliberate configuration without altering the
    /// resolved list itself. Sources that yield zero proxies after parsing
    /// (comma-only text, an empty array) count as unconfigured so that
    /// `egress_mode="proxy"` cannot be unlocked by a phantom setting.
    pub proxies_explicitly_configured: bool,
    pub bootstrap_timeout: Duration,
    pub verify_timeout: Duration,
    pub recovery_backoff_max: Duration,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub runtime_dir: Option<PathBuf>,
    pub docker_binary: String,
    pub warp_cli_binary: String,
    pub warp_image: String,
    pub worker_shutdown_timeout: Duration,
    pub server_shutdown_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    pub max_concurrent_requests: Option<usize>,
    pub metrics_enabled: bool,
    pub request_id_header: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryCaptureMode {
    Off,
    Metadata,
    Redacted,
    Full,
}

impl HistoryCaptureMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" => Some(Self::Off),
            "metadata" => Some(Self::Metadata),
            "redacted" => Some(Self::Redacted),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Metadata => "metadata",
            Self::Redacted => "redacted",
            Self::Full => "full",
        }
    }
}

impl fmt::Display for HistoryCaptureMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub capture_mode: HistoryCaptureMode,
    pub capture_inbound: bool,
    pub capture_effective: bool,
    pub capture_reasoning: bool,
    pub capture_response: bool,
    pub capture_tools: bool,
    pub capture_search_queries: bool,
    pub capture_search_results: bool,
    pub capture_shell_commands: bool,
    pub retention_days: u32,
    pub max_records: usize,
    pub max_database_bytes: u64,
    pub max_request_bytes: usize,
    pub max_reasoning_bytes: usize,
    pub max_response_bytes: usize,
    pub max_tool_payload_bytes: usize,
    pub max_record_bytes: usize,
    pub queue_capacity: usize,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ProtocolConfig {
    pub min_reasoning_stream_tokens: u32,
    pub max_sse_line_bytes: usize,
    pub max_sync_response_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub max_results: usize,
    pub max_snippet_chars: usize,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
    /// Wall-clock budget for one full provider fallback-chain walk, plumbed
    /// into `SearchPolicy.chain_budget` (see `opencode::search`). Must stay in
    /// lockstep with the loader's effective default of 25 seconds so both
    /// construction paths behave identically.
    pub chain_budget: Duration,
    pub allow_private_searxng: bool,
    pub tavily_url: String,
    pub exa_url: String,
    pub serper_url: String,
    pub duckduckgo_url: String,
    pub yahoo_url: String,
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub host: IpAddr,
    pub bridge_port: u16,
    pub opencode_port: u16,
    pub model: Option<String>,
    pub shell_policy: ShellPolicy,
    pub auth_tokens: Option<Vec<SecretString>>,
    pub max_body_size: usize,
    pub stream_buffer_size: usize,
    pub channel_capacity: usize,
    pub tavily_api_key: Option<SecretString>,
    pub exa_api_key: Option<SecretString>,
    pub serper_api_key: Option<SecretString>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<SecretString>,
    pub max_search_loops: u32,
    #[allow(dead_code)]
    pub proxies: Option<Vec<String>>,
    pub primary_proxies: Option<Vec<String>>,
    pub warm_standby_proxies: Option<Vec<String>>,
    pub management: ManagementConfig,
    pub retry: RetryConfig,
    pub egress: EgressConfig,
    pub runtime: RuntimeConfig,
    pub observability: ObservabilityConfig,
    pub history: HistoryConfig,
    pub protocol: ProtocolConfig,
    pub search: SearchConfig,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".parse().expect("hardcoded loopback is valid"),
            bridge_port: super::DEFAULT_BRIDGE_PORT,
            opencode_port: super::DEFAULT_OPENCODE_PORT,
            model: None,
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: super::DEFAULT_MAX_BODY_SIZE,
            stream_buffer_size: super::DEFAULT_STREAM_BUFFER_SIZE,
            channel_capacity: super::DEFAULT_CHANNEL_CAPACITY,
            tavily_api_key: None,
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 20,
            proxies: None,
            primary_proxies: None,
            warm_standby_proxies: None,
            management: ManagementConfig {
                dashboard_token: None,
                rest_api_token: None,
                config_path: PathBuf::from("opencode2api.toml"),
                csrf_enabled: true,
            },
            retry: RetryConfig {
                upstream_base_url: "https://opencode.ai/zen/v1".to_string(),
                model_fallbacks: Vec::new(),
                default_fallbacks_enabled: false,
                max_network_attempts: 8,
                base_backoff: Duration::from_secs(1),
                max_backoff: Duration::from_secs(30),
            },
            egress: EgressConfig {
                mode: EgressMode::Hybrid,
                active_proxy_count: 1,
                require_verified_exit_ip: true,
                minimum_unique_exit_ips: 1,
                identity_endpoints: vec![
                    "https://cloudflare.com/cdn-cgi/trace".to_string(),
                    "https://api.ipify.org?format=json".to_string(),
                ],
                identity_ttl: Duration::from_secs(300),
                health_interval: Duration::from_secs(10),
                restart_interval: Duration::from_secs(2),
                max_restart_attempts: 6,
                allow_direct_fallback: false,
                proxies_explicitly_configured: false,
                bootstrap_timeout: Duration::from_secs(30),
                verify_timeout: Duration::from_secs(10),
                recovery_backoff_max: Duration::from_secs(120),
            },
            runtime: RuntimeConfig {
                runtime_dir: None,
                docker_binary: "docker".to_string(),
                warp_cli_binary: "warp-cli".to_string(),
                warp_image: "ghcr.io/mon-ius/docker-warp-socks:latest".to_string(),
                worker_shutdown_timeout: Duration::from_secs(30),
                server_shutdown_timeout: Duration::from_secs(30),
            },
            observability: ObservabilityConfig {
                max_concurrent_requests: None,
                metrics_enabled: true,
                request_id_header: "x-request-id".to_string(),
            },
            history: HistoryConfig {
                enabled: false,
                capture_mode: HistoryCaptureMode::Redacted,
                capture_inbound: true,
                capture_effective: true,
                capture_reasoning: true,
                capture_response: true,
                capture_tools: true,
                capture_search_queries: true,
                capture_search_results: false,
                capture_shell_commands: false,
                retention_days: 30,
                max_records: 1_000_000,
                max_database_bytes: 16 * 1024 * 1024 * 1024,
                max_request_bytes: 8 * 1024 * 1024,
                max_reasoning_bytes: 16 * 1024 * 1024,
                // Kept in lockstep with the loader's effective default
                // (`BRIDGE_HISTORY_MAX_RESPONSE_BYTES` fallback) so both
                // construction paths behave identically.
                max_response_bytes: 2 * 1024 * 1024,
                max_tool_payload_bytes: 4 * 1024 * 1024,
                max_record_bytes: 48 * 1024 * 1024,
                queue_capacity: 8192,
                path: None,
            },
            protocol: ProtocolConfig {
                min_reasoning_stream_tokens: 1024,
                max_sse_line_bytes: 4 * 1024 * 1024,
                max_sync_response_bytes: 32 * 1024 * 1024,
            },
            search: SearchConfig {
                max_results: 20,
                max_snippet_chars: 2000,
                max_response_bytes: 8 * 1024 * 1024,
                request_timeout: Duration::from_secs(30),
                // Matches `SearchPolicy::default().chain_budget`: above a
                // single default provider timeout (15s) but far below the
                // unbounded serial walk.
                chain_budget: Duration::from_secs(25),
                allow_private_searxng: false,
                tavily_url: "https://api.tavily.com/search".to_string(),
                exa_url: "https://api.exa.ai/search".to_string(),
                serper_url: "https://google.serper.dev/search".to_string(),
                duckduckgo_url: "https://html.duckduckgo.com/html/".to_string(),
                yahoo_url: "https://search.yahoo.com/search".to_string(),
            },
        }
    }
}
