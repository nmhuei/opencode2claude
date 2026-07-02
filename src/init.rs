//! Config initializer — generates a default `opencode2claude.toml`.
//!
//! The `init` subcommand writes a commented template to the specified path,
//! giving users a ready-to-edit starting point.

use anyhow::{Context, Result};
use std::path::Path;

/// Default config template content.
const CONFIG_TEMPLATE: &str = r##"# OpenCode2Claude configuration
# Uncomment and adjust values as needed.

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

# Bearer auth tokens (comma-separated). Required when host is not 127.0.0.1
# auth_tokens = ["sk-example-token"]

# Max concurrent requests (unset = no limit)
# rate_limit = 10

# Max web search tool-call loops
# max_search_loops = 5

# ── Proxy Pool (WARP SOCKS5) ──────────────────────────────────────────
# Primary proxies (for normal traffic)
# primary_proxies = [
#     "socks5://127.0.0.1:40001",
#     "socks5://127.0.0.1:40002",
#     "socks5://127.0.0.1:40003",
# ]

# Warm-standby proxies (failover only)
# warm_standby_proxies = [
#     "socks5://127.0.0.1:40004",
#     "socks5://127.0.0.1:40005",
# ]

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

    eprintln!("✓ Config template written to {}", path.display());
    eprintln!("  Edit it to match your setup, then run:");
    eprintln!("    opencode2claude server start -c {}", path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_template_is_non_empty() {
        assert!(!CONFIG_TEMPLATE.is_empty());
        assert!(CONFIG_TEMPLATE.contains("port = 4000"));
        assert!(CONFIG_TEMPLATE.contains("shell_policy"));
        assert!(CONFIG_TEMPLATE.contains("primary_proxies"));
    }

    #[test]
    fn test_config_template_contains_all_sections() {
        assert!(CONFIG_TEMPLATE.contains("OpenCode2Claude configuration"));
        assert!(CONFIG_TEMPLATE.contains("Web Search API Keys"));
        assert!(CONFIG_TEMPLATE.contains("Proxy Pool"));
    }
}
