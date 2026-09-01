//! Config initializer — generates a default `opencode2api.toml`.
//!
//! The `init` subcommand writes a commented template to the specified path,
//! giving users a ready-to-edit starting point.

use anyhow::{Context, Result};
use std::path::Path;

/// Default config template content.
pub const CONFIG_TEMPLATE: &str = r##"# OpenCode2API configuration
# Uncomment and adjust values as needed.

# Config schema. The loader migrates supported legacy keys to this version.
schema_version = 1

# Bridge listen port
# port = 4000

# Bind address (use 0.0.0.0 with caution — requires auth tokens)
# host = "127.0.0.1"

# Default model to use (e.g. "openai/gpt-4o", "deepseek-v4-flash")
# model = "openai/gpt-4o"

# Shell execution policy: "disabled", "allowlist", or "unrestricted"
# shell_policy = "disabled"

# Allowed shell commands (comma-separated, only used when policy = "allowlist")
# shell_allowlist = "git,ls,pwd,echo,cat"

# Bearer auth tokens. A string array or comma-separated string is accepted.
# Required when host is not 127.0.0.1.
# auth_tokens = ["sk-example-token"]

# Browser dashboard and REST management authentication.
# dashboard_admin_token = "replace-with-a-long-random-token"
# rest_api_token = "replace-with-a-different-long-random-token"
# csrf_enabled = true

# OpenAI-compatible upstream base URL and authentication.
# upstream_base_url = "https://opencode.ai/zen/v1"
# upstream_api_key = "sk-example-upstream-key"
# Optional TOML-only pool (CSV or string array); requests round-robin and 429 retries the next key.
# upstream_api_keys = ["sk-primary", "sk-secondary"]
# model_fallbacks = ["opencode/deepseek-v4-flash-free"]
# enable_default_fallbacks = false
# max_network_attempts = 5
# retry_base_backoff_ms = 2000
# retry_max_backoff_ms = 16000

# Max concurrent requests (unset = no limit)
# rate_limit = 10
# min_reasoning_stream_tokens = 1024

# Upstream protocol response bounds. These limit data received from the LLM
# provider, not the incoming client request body.
# max_sse_line_bytes = 262144
# max_sync_response_bytes = 4194304

# Max web search tool-call loops
# max_search_loops = 5

# Search result and network bounds.
# search_max_results = 5
# search_max_snippet_chars = 500
# search_max_response_bytes = 1048576
# search_timeout_secs = 15

# Search provider endpoints. Override mainly for controlled tests or approved
# private deployments. Private SearXNG is blocked unless explicitly allowed.
# tavily_url = "https://api.tavily.com/search"
# exa_url = "https://api.exa.ai/search"
# serper_url = "https://google.serper.dev/search"
# duckduckgo_url = "https://html.duckduckgo.com/html/"
# yahoo_url = "https://search.yahoo.com/search"
# allow_private_searxng = false

# ── Proxy Pool (WARP SOCKS5) ──────────────────────────────────────────
# Egress mode: "direct" or "proxy". Proxy mode fails closed.
# egress_mode = "proxy"
# active_proxy_count = 3
# allow_direct_fallback = false

# Primary proxies (managed, normal traffic)
# primary_proxies = [
#     "socks5h://127.0.0.1:40001",
#     "socks5h://127.0.0.1:40002",
#     "socks5h://127.0.0.1:40003",
# ]

# Warm-standby proxies (protected failover only). The application never
# restarts, stops, purges, or recreates these nodes.
# warm_standby_proxies = [
#     "socks5h://127.0.0.1:40004",
#     "socks5h://127.0.0.1:40005",
# ]

# Exit identity verification policy.
# require_verified_exit_ip = true
# minimum_unique_exit_ips = 1
# identity_endpoints = [
#     "https://cloudflare.com/cdn-cgi/trace",
#     "https://api.ipify.org?format=json",
# ]
# identity_ttl_secs = 300

# Managed-primary health/restart policy.
# proxy_health_interval_secs = 10
# proxy_restart_interval_secs = 2
# max_proxy_restart_attempts = 3

# Docker/WARP runtime.
# docker_binary = "docker"
# warp_cli_binary = "warp-cli"
# warp_image = "ghcr.io/mon-ius/docker-warp-socks:latest"
# worker_shutdown_timeout_secs = 10
# server_shutdown_timeout_secs = 15

# ── Web Search API Keys ──────────────────────────────────────────────
# At least one is needed for web search tool support.
# Falls back in order: Tavily → Exa → Serper → SearXNG → DuckDuckGo

# Tavily search (fastest, recommended)
# tavily_api_key = "tvly-..."

# Exa search
# exa_api_key = "..."

# Serper.dev search
# serper_api_key = "..."

# Self-hosted SearXNG instance
# searxng_url = "http://searxng.local:8080"
# searxng_api_key = ""
"##;

pub fn config_template() -> &'static str {
    CONFIG_TEMPLATE
}

/// Generate the default config file at the given path.
///
/// Returns an error if the file already exists (unless `force` is true).
pub async fn generate_config(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists — use --force to overwrite",
            path.display()
        );
    }

    tokio::fs::write(path, CONFIG_TEMPLATE)
        .await
        .with_context(|| format!("failed to write config to {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_template_is_non_empty() {
        assert!(!CONFIG_TEMPLATE.is_empty());
        assert!(CONFIG_TEMPLATE.contains("schema_version = 1"));
        assert!(CONFIG_TEMPLATE.contains("port = 4000"));
        assert!(CONFIG_TEMPLATE.contains("shell_policy"));
        assert!(CONFIG_TEMPLATE.contains("primary_proxies"));
    }

    #[test]
    fn test_config_template_contains_all_sections() {
        assert!(CONFIG_TEMPLATE.contains("OpenCode2API configuration"));
        assert!(CONFIG_TEMPLATE.contains("Web Search API Keys"));
        assert!(CONFIG_TEMPLATE.contains("Proxy Pool"));
    }
}
