//! Security validation for resolved configuration.

use super::{BridgeConfig, EgressMode};
use crate::shell::ShellPolicy;

/// Minimum secret length for any token guarding a non-loopback bind. This is
/// the same strength convention already enforced for DASHBOARD_ADMIN_TOKEN
/// below; bridge auth tokens must not be weaker than the admin token.
const MIN_PUBLIC_BIND_TOKEN_LEN: usize = 12;

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
    if !(1024..=64 * 1024 * 1024).contains(&config.protocol.max_sse_line_bytes)
        || !(1024..=64 * 1024 * 1024).contains(&config.protocol.max_sync_response_bytes)
    {
        return Err(
            "CONFIGURATION ERROR: protocol response limits must be between 1024 bytes and 64 MiB"
                .to_string(),
        );
    }
    if config.retry.max_network_attempts == 0 {
        return Err("CONFIGURATION ERROR: max network attempts must be greater than zero".into());
    }
    if config.egress.bootstrap_timeout.is_zero()
        || config.egress.verify_timeout.is_zero()
        || config.egress.recovery_backoff_max.is_zero()
    {
        return Err(
            "CONFIGURATION ERROR: hybrid proxy timing values must be greater than zero".into(),
        );
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
        ("Yahoo", config.search.yahoo_url.as_str()),
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
        // Explicit-intent gate: the loader always materializes a built-in
        // WARP default pool when nothing is configured, so a bare
        // `egress_mode = "proxy"` used to silently inherit
        // socks5h://127.0.0.1:40001 and the old `configured == 0` check was
        // dead code. Require deliberate proxy configuration instead.
        if !config.egress.proxies_explicitly_configured {
            return Err(
                "CONFIGURATION ERROR: egress_mode='proxy' requires an explicitly configured proxy pool.\n  Set BRIDGE_PRIMARY_PROXIES (env) or primary_proxies (TOML) before selecting proxy egress.\n  Refusing to silently inherit the built-in WARP fallback pool."
                    .into(),
            );
        }
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
    if dashboard_token.len() < MIN_PUBLIC_BIND_TOKEN_LEN {
        return Err(format!(
            "SECURITY VIOLATION: DASHBOARD_ADMIN_TOKEN is too weak (must be at least 12 characters) for non-loopback binding.\n  Configure a stronger DASHBOARD_ADMIN_TOKEN.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }
    // Client-API authentication presence: only BRIDGE_AUTH_TOKEN qualifies.
    // REST_API_TOKEN (and DASHBOARD_ADMIN_TOKEN) gate management routes
    // exclusively and are never imported into the LLM-route admission
    // registry, so accepting them here would leave /v1/messages and
    // /v1/chat/completions wide open on a public interface.
    if !config.auth_enabled() {
        return Err(format!(
            "SECURITY VIOLATION: Binding to a non-loopback address without LLM-route authentication.\n  Set BRIDGE_AUTH_TOKEN to require authentication before binding publicly.\n  REST_API_TOKEN alone is not sufficient: it guards management routes only, not /v1/messages or /v1/chat/completions.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }
    if config.auth_tokens.as_ref().is_some_and(|tokens| {
        tokens
            .iter()
            .any(|token| token.expose().len() < MIN_PUBLIC_BIND_TOKEN_LEN)
    }) {
        return Err(format!(
            "SECURITY VIOLATION: BRIDGE_AUTH_TOKEN is too weak (every token must be at least 12 characters) for non-loopback binding.\n  Replace short tokens with stronger secrets.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
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
    // Url::host_str keeps IPv6 literals bracketed ("[::1]"), which
    // IpAddr::from_str rejects; strip them or every bracketed v6 target
    // would be misclassified as public.
    let unbracketed = normalized
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(&normalized);
    unbracketed
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => is_private_v4(ip),
            std::net::IpAddr::V6(ip) => {
                // IPv4-mapped IPv6 (::ffff:a.b.c.d) must be judged by its
                // embedded v4 address, or `http://[::ffff:169.254.169.254]`
                // would slip past the private-target guard.
                if let Some(mapped) = ip.to_ipv4_mapped() {
                    return is_private_v4(mapped);
                }
                ip.is_loopback()
                    || ip.is_unspecified()
                    || (ip.segments()[0] & 0xfe00) == 0xfc00
                    || (ip.segments()[0] & 0xffc0) == 0xfe80
            }
        })
}

fn is_private_v4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || octets[0] == 0
        // Carrier-grade NAT 100.64/10 is not covered by Ipv4Addr::is_private.
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
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

    #[test]
    fn rejects_ipv4_mapped_loopback_searxng_without_opt_in() {
        // ::ffff:127.0.0.1 is loopback in every meaningful sense; the guard
        // must see through the mapped representation.
        let config = BridgeConfig {
            searxng_url: Some("http://[::ffff:127.0.0.1]:8080".to_string()),
            ..Default::default()
        };
        assert!(validate(&config)
            .unwrap_err()
            .contains("allow_private_searxng"));
    }

    #[test]
    fn rejects_bracketed_ipv6_loopback_searxng_without_opt_in() {
        // Url::host_str keeps IPv6 literals bracketed; before the bracket
        // strip, http://[::1] was misclassified as public.
        let config = BridgeConfig {
            searxng_url: Some("http://[::1]:8080".to_string()),
            ..Default::default()
        };
        assert!(validate(&config)
            .unwrap_err()
            .contains("allow_private_searxng"));
    }

    #[test]
    fn rejects_mapped_and_cgnat_private_targets_without_opt_in() {
        for url in [
            "http://[::ffff:169.254.169.254]/search", // cloud metadata via mapped v4
            "http://100.64.0.10/search",              // CGNAT 100.64/10
            "http://[::ffff:100.64.0.10]/search",     // CGNAT via mapped v4
        ] {
            let config = BridgeConfig {
                searxng_url: Some(url.to_string()),
                ..Default::default()
            };
            assert!(
                validate(&config)
                    .unwrap_err()
                    .contains("allow_private_searxng"),
                "must reject private target {url} without explicit opt-in"
            );
        }
    }
}
