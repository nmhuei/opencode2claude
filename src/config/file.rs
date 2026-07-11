//! TOML file schema and parsing.

use serde::Deserialize;

/// Backward-compatible TOML value accepting either a comma-separated string
/// or an explicit string array.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum StringList {
    Csv(String),
    List(Vec<String>),
}

impl StringList {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::Csv(value) => value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            Self::List(values) => values
                .into_iter()
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct TomlConfig {
    pub schema_version: Option<u32>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub opencode_port: Option<u16>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub shell_allowlist: Option<String>,
    pub auth_tokens: Option<StringList>,
    pub max_body_size: Option<usize>,
    pub stream_buffer_size: Option<usize>,
    pub channel_capacity: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub max_search_loops: Option<u32>,
    pub search_max_results: Option<usize>,
    pub search_max_snippet_chars: Option<usize>,
    pub search_max_response_bytes: Option<usize>,
    pub search_timeout_secs: Option<u64>,
    pub allow_private_searxng: Option<bool>,
    pub tavily_url: Option<String>,
    pub exa_url: Option<String>,
    pub serper_url: Option<String>,
    pub duckduckgo_url: Option<String>,
    pub proxies: Option<Vec<String>>,
    pub primary_proxies: Option<Vec<String>>,
    pub warm_standby_proxies: Option<Vec<String>>,

    // Management/authentication.
    pub dashboard_admin_token: Option<String>,
    pub rest_api_token: Option<String>,
    pub csrf_enabled: Option<bool>,

    // Runtime policy.
    pub rate_limit: Option<usize>,
    pub min_reasoning_stream_tokens: Option<u32>,
    pub max_sse_line_bytes: Option<usize>,
    pub max_sync_response_bytes: Option<usize>,
    pub upstream_base_url: Option<String>,
    pub model_fallbacks: Option<Vec<String>>,
    pub enable_default_fallbacks: Option<bool>,
    pub max_network_attempts: Option<usize>,
    pub max_provider_attempts: Option<u32>,
    pub retry_base_backoff_ms: Option<u64>,
    pub retry_max_backoff_ms: Option<u64>,

    // Egress policy.
    pub egress_mode: Option<String>,
    pub active_proxy_count: Option<usize>,
    pub require_verified_exit_ip: Option<bool>,
    pub minimum_unique_exit_ips: Option<usize>,
    pub identity_endpoints: Option<Vec<String>>,
    pub identity_ttl_secs: Option<u64>,
    pub proxy_health_interval_secs: Option<u64>,
    pub proxy_restart_interval_secs: Option<u64>,
    pub max_proxy_restart_attempts: Option<u32>,
    pub allow_direct_fallback: Option<bool>,

    // Infrastructure and lifecycle.
    pub runtime_dir: Option<String>,
    pub docker_binary: Option<String>,
    pub warp_cli_binary: Option<String>,
    pub warp_image: Option<String>,
    pub worker_shutdown_timeout_secs: Option<u64>,
    pub server_shutdown_timeout_secs: Option<u64>,

    // Observability.
    pub metrics_enabled: Option<bool>,
    pub request_id_header: Option<String>,
}

impl TomlConfig {
    pub fn from_file(path: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let (migrated, _report) = super::migration::migrate_document(&content).ok()?;
        toml::from_str(&migrated).ok()
    }
}
