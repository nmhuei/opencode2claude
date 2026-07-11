//! Security validation for resolved configuration.

use super::{BridgeConfig, EgressMode};
use crate::shell::ShellPolicy;

pub(super) fn validate(config: &BridgeConfig) -> Result<(), String> {
    if config.retry.upstream_base_url.trim().is_empty() {
        return Err("CONFIGURATION ERROR: upstream base URL cannot be empty".to_string());
    }
    if config.stream_buffer_size == 0 || config.channel_capacity == 0 {
        return Err(
            "CONFIGURATION ERROR: stream buffer size and channel capacity must be greater than zero"
                .to_string(),
        );
    }
    if config.retry.max_network_attempts == 0 {
        return Err("CONFIGURATION ERROR: max network attempts must be greater than zero".into());
    }
    if config.retry.base_backoff > config.retry.max_backoff {
        return Err(
            "CONFIGURATION ERROR: retry base backoff cannot exceed retry max backoff".into(),
        );
    }
    if config.search.max_results == 0
        || config.search.max_results > 20
        || config.search.max_snippet_chars == 0
        || config.search.max_response_bytes < 1024
        || config.search.request_timeout.is_zero()
    {
        return Err("CONFIGURATION ERROR: search limits must be positive, max_results <= 20, and max_response_bytes >= 1024".into());
    }
    for (name, endpoint) in [
        ("Tavily", config.search.tavily_url.as_str()),
        ("Exa", config.search.exa_url.as_str()),
        ("Serper", config.search.serper_url.as_str()),
        ("DuckDuckGo", config.search.duckduckgo_url.as_str()),
    ] {
        validate_http_url(name, endpoint)?;
    }
    if let Some(endpoint) = config.searxng_url.as_deref() {
        let parsed = validate_http_url("SearXNG", endpoint)?;
        if !config.search.allow_private_searxng && is_private_endpoint(&parsed) {
            return Err("SECURITY VIOLATION: private/loopback SearXNG endpoints require allow_private_searxng=true".into());
        }
    }

    if config.egress.mode == EgressMode::Proxy {
        let configured = config.primary_proxies.as_ref().map_or(0, Vec::len)
            + config.warm_standby_proxies.as_ref().map_or(0, Vec::len);
        if configured == 0 {
            return Err(
                "CONFIGURATION ERROR: proxy egress mode requires at least one configured proxy"
                    .into(),
            );
        }
        if config.egress.minimum_unique_exit_ips == 0 {
            return Err(
                "CONFIGURATION ERROR: minimum unique exit IPs must be greater than zero".into(),
            );
        }
        if config.egress.minimum_unique_exit_ips > configured {
            return Err(format!(
                "CONFIGURATION ERROR: minimum unique exit IPs ({}) exceeds configured proxy nodes ({})",
                config.egress.minimum_unique_exit_ips, configured
            ));
        }
        if config.egress.allow_direct_fallback {
            return Err(
                "SECURITY VIOLATION: direct fallback is forbidden while proxy egress mode is configured"
                    .into(),
            );
        }
    }

    if config.host.is_loopback() {
        return Ok(());
    }

    let dashboard_token = config.management.dashboard_token().unwrap_or_default();
    if dashboard_token.is_empty() {
        return Err(format!(
            "SECURITY VIOLATION: Binding to a non-loopback address without an explicit DASHBOARD_ADMIN_TOKEN.\n  Set DASHBOARD_ADMIN_TOKEN to a strong secret before binding publicly.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }
    if dashboard_token.len() < 12 {
        return Err(format!(
            "SECURITY VIOLATION: DASHBOARD_ADMIN_TOKEN is too weak (must be at least 12 characters) for non-loopback binding.\n  Configure a stronger DASHBOARD_ADMIN_TOKEN.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }
    if !config.auth_enabled() {
        return Err(format!(
            "SECURITY VIOLATION: Binding to a non-loopback address without authentication.\n  Set BRIDGE_AUTH_TOKEN to require authentication before binding publicly.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }
    if matches!(config.shell_policy, ShellPolicy::Unrestricted) {
        return Err(format!(
            "SECURITY VIOLATION: Binding to a non-loopback address with unrestricted shell policy.\n  Set BRIDGE_SHELL_POLICY=disabled or configure an allowlist.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }

    Ok(())
}

fn validate_http_url(name: &str, endpoint: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(endpoint)
        .map_err(|error| format!("CONFIGURATION ERROR: invalid {name} URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!(
            "CONFIGURATION ERROR: {name} URL must use http or https and include a host"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "SECURITY VIOLATION: {name} URL must not contain embedded credentials"
        ));
    }
    Ok(parsed)
}

fn is_private_endpoint(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    if normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
    {
        return true;
    }
    normalized
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => {
                ip.is_private()
                    || ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_unspecified()
                    || ip.is_broadcast()
                    || ip.octets()[0] == 0
            }
            std::net::IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip.is_unspecified()
                    || (ip.segments()[0] & 0xfe00) == 0xfc00
                    || (ip.segments()[0] & 0xffc0) == 0xfe80
            }
        })
}

#[cfg(test)]
mod search_security_tests {
    use super::*;

    #[test]
    fn rejects_private_searxng_without_explicit_opt_in() {
        let mut config = BridgeConfig {
            searxng_url: Some("http://127.0.0.1:8080".to_string()),
            ..Default::default()
        };
        assert!(validate(&config)
            .unwrap_err()
            .contains("allow_private_searxng"));
        config.search.allow_private_searxng = true;
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn rejects_embedded_credentials_in_provider_url() {
        let defaults = BridgeConfig::default();
        let config = BridgeConfig {
            search: super::super::SearchConfig {
                tavily_url: "https://user:secret@example.com/search".to_string(),
                ..defaults.search
            },
            ..defaults
        };
        assert!(validate(&config)
            .unwrap_err()
            .contains("embedded credentials"));
    }
}
