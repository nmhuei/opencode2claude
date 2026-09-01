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
    pub search_chain_budget_secs: Option<u64>,
    pub allow_private_searxng: Option<bool>,
    pub tavily_url: Option<String>,
    pub exa_url: Option<String>,
    pub serper_url: Option<String>,
    pub duckduckgo_url: Option<String>,
    pub yahoo_url: Option<String>,
    pub proxies: Option<StringList>,
    pub primary_proxies: Option<StringList>,
    pub warm_standby_proxies: Option<StringList>,

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
    pub upstream_api_key: Option<String>,
    pub upstream_api_keys: Option<StringList>,
    pub model_fallbacks: Option<StringList>,
    pub enable_default_fallbacks: Option<bool>,
    pub max_network_attempts: Option<usize>,
    pub retry_base_backoff_ms: Option<u64>,
    pub retry_max_backoff_ms: Option<u64>,

    // Egress policy.
    pub egress_mode: Option<String>,
    pub active_proxy_count: Option<usize>,
    pub require_verified_exit_ip: Option<bool>,
    pub minimum_unique_exit_ips: Option<usize>,
    pub identity_endpoints: Option<StringList>,
    pub identity_ttl_secs: Option<u64>,
    pub proxy_health_interval_secs: Option<u64>,
    pub proxy_restart_interval_secs: Option<u64>,
    pub max_proxy_restart_attempts: Option<u32>,
    pub allow_direct_fallback: Option<bool>,
    pub proxy_bootstrap_timeout_secs: Option<u64>,
    pub proxy_verify_timeout_secs: Option<u64>,
    pub proxy_recovery_backoff_max_secs: Option<u64>,

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

    // Request history.
    pub history_enabled: Option<bool>,
    pub history_capture_mode: Option<String>,
    pub history_capture_inbound: Option<bool>,
    pub history_capture_effective: Option<bool>,
    pub history_capture_reasoning: Option<bool>,
    pub history_capture_response: Option<bool>,
    pub history_capture_tools: Option<bool>,
    pub history_capture_search_queries: Option<bool>,
    pub history_capture_search_results: Option<bool>,
    pub history_capture_shell_commands: Option<bool>,
    pub history_retention_days: Option<u32>,
    pub history_max_records: Option<usize>,
    pub history_max_database_bytes: Option<u64>,
    pub history_max_request_bytes: Option<usize>,
    pub history_max_reasoning_bytes: Option<usize>,
    pub history_max_response_bytes: Option<usize>,
    pub history_max_tool_payload_bytes: Option<usize>,
    pub history_max_record_bytes: Option<usize>,
    pub history_queue_capacity: Option<usize>,
    pub history_path: Option<String>,
}

impl TomlConfig {
    /// Load and migrate a TOML configuration document.
    ///
    /// A missing file is normal (configuration is optional), but a file that
    /// exists yet fails migration or parsing is rejected WHOLESALE — never
    /// partially applied. Because the caller treats `None` as "no overrides",
    /// every such rejection must be logged: otherwise a single typo silently
    /// discards auth tokens, proxy topology, and bind settings for the whole
    /// process lifetime.
    pub fn from_file(path: &str) -> Option<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!("ignoring unreadable config file '{path}': {error}");
                return None;
            }
        };
        let migrated = match super::migration::migrate_document(&content) {
            Ok((migrated, _report)) => migrated,
            Err(error) => {
                tracing::warn!("ignoring invalid config file '{path}': {error}");
                return None;
            }
        };
        match toml::from_str(&migrated) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                tracing::warn!("ignoring unparseable config file '{path}': {error}");
                None
            }
        }
    }
}
