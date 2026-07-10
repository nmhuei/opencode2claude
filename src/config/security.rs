//! Security validation for resolved configuration.

use super::BridgeConfig;
use crate::management::auth;
use crate::shell::ShellPolicy;

pub(super) fn validate(config: &BridgeConfig) -> Result<(), String> {
    if config.host.is_loopback() {
        return Ok(());
    }

    let dashboard_token = auth::dashboard_token().unwrap_or_default();
    if dashboard_token.is_empty() {
        return Err(format!(
            "SECURITY VIOLATION: Binding to a non-loopback address without an explicit DASHBOARD_ADMIN_TOKEN.\n  Set DASHBOARD_ADMIN_TOKEN to a strong secret before binding publicly.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
            config.host
        ));
    }
    if dashboard_token.len() < 8 {
        return Err(format!(
            "SECURITY VIOLATION: DASHBOARD_ADMIN_TOKEN is too weak (must be at least 8 characters) for non-loopback binding.\n  Configure a stronger DASHBOARD_ADMIN_TOKEN.\n  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n  Current host: {}",
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
