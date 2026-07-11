//! Public resolved configuration data structures.

use crate::shell::ShellPolicy;
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
}

impl EgressMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "direct" => Some(Self::Direct),
            "proxy" | "warp" => Some(Self::Proxy),
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
    pub max_provider_attempts: u32,
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

#[derive(Debug, Clone)]
pub struct ProtocolConfig {
    pub min_reasoning_stream_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub max_results: usize,
    pub max_snippet_chars: usize,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
    pub allow_private_searxng: bool,
    pub tavily_url: String,
    pub exa_url: String,
    pub serper_url: String,
    pub duckduckgo_url: String,
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
            max_search_loops: 5,
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
                max_network_attempts: 5,
                max_provider_attempts: 1,
                base_backoff: Duration::from_secs(2),
                max_backoff: Duration::from_secs(16),
            },
            egress: EgressConfig {
                mode: EgressMode::Direct,
                active_proxy_count: 3,
                require_verified_exit_ip: false,
                minimum_unique_exit_ips: 1,
                identity_endpoints: vec![
                    "https://cloudflare.com/cdn-cgi/trace".to_string(),
                    "https://api.ipify.org?format=json".to_string(),
                ],
                identity_ttl: Duration::from_secs(300),
                health_interval: Duration::from_secs(10),
                restart_interval: Duration::from_secs(2),
                max_restart_attempts: 3,
                allow_direct_fallback: false,
            },
            runtime: RuntimeConfig {
                runtime_dir: None,
                docker_binary: "docker".to_string(),
                warp_cli_binary: "warp-cli".to_string(),
                warp_image: "ghcr.io/mon-ius/docker-warp-socks:latest".to_string(),
                worker_shutdown_timeout: Duration::from_secs(10),
                server_shutdown_timeout: Duration::from_secs(15),
            },
            observability: ObservabilityConfig {
                max_concurrent_requests: None,
                metrics_enabled: true,
                request_id_header: "x-request-id".to_string(),
            },
            protocol: ProtocolConfig {
                min_reasoning_stream_tokens: 1024,
            },
            search: SearchConfig {
                max_results: 5,
                max_snippet_chars: 500,
                max_response_bytes: 1024 * 1024,
                request_timeout: Duration::from_secs(15),
                allow_private_searxng: false,
                tavily_url: "https://api.tavily.com/search".to_string(),
                exa_url: "https://api.exa.ai/search".to_string(),
                serper_url: "https://google.serper.dev/search".to_string(),
                duckduckgo_url: "https://html.duckduckgo.com/html/".to_string(),
            },
        }
    }
}
