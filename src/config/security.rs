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
