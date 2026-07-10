//! Transport-agnostic management operations.

use crate::dashboard::DashboardEvent;
use crate::proxy_pool::{is_managed_proxy_port, is_protected_proxy_port, ProxyPoolStats};
use crate::state::AppState;
use axum::http::StatusCode;
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info};

#[derive(Debug, Clone, Serialize)]
pub struct SafeConfigSnapshot {
    pub host: String,
    pub bridge_port: u16,
    pub opencode_port: u16,
    pub model: Option<String>,
    pub shell_policy: String,
    pub shell_policy_label: String,
    pub shell_allowlist: Option<String>,
    pub max_body_size: usize,
    pub stream_buffer_size: usize,
    pub channel_capacity: usize,
    pub max_search_loops: u32,
    pub primary_proxies: Vec<String>,
    pub warm_standby_proxies: Vec<String>,
    pub client_auth_configured: bool,
    pub tavily_configured: bool,
    pub exa_configured: bool,
    pub serper_configured: bool,
    pub searxng_configured: bool,
    pub searxng_api_key_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyRestartResult {
    pub port: u16,
    pub action: &'static str,
}

#[derive(Debug)]
pub struct ManagementError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ManagementError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

pub fn uptime_secs(state: &AppState) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(state.started_at.load(Ordering::Relaxed))
}

pub async fn proxy_snapshot(state: &AppState) -> ProxyPoolStats {
    let mut pool = state.proxy_pool.write().await;
    pool.recover_expired_cooldowns();
    pool.snapshot()
}

pub fn safe_config_snapshot(state: &AppState) -> SafeConfigSnapshot {
    let cfg = &state.config;
    SafeConfigSnapshot {
        host: cfg.host.to_string(),
        bridge_port: cfg.bridge_port,
        opencode_port: cfg.opencode_port,
        model: cfg.model.clone(),
        shell_policy: cfg.shell_policy.kind().to_string(),
        shell_policy_label: cfg.shell_policy.description().to_string(),
        shell_allowlist: cfg.shell_policy.allowlist_string(),
        max_body_size: cfg.max_body_size,
        stream_buffer_size: cfg.stream_buffer_size,
        channel_capacity: cfg.channel_capacity,
        max_search_loops: cfg.max_search_loops,
        primary_proxies: redact_proxy_urls(cfg.primary_proxies.as_deref().unwrap_or_default()),
        warm_standby_proxies: redact_proxy_urls(
            cfg.warm_standby_proxies.as_deref().unwrap_or_default(),
        ),
        client_auth_configured: cfg
            .auth_tokens
            .as_ref()
            .is_some_and(|tokens| !tokens.is_empty()),
        tavily_configured: cfg.tavily_api_key.is_some(),
        exa_configured: cfg.exa_api_key.is_some(),
        serper_configured: cfg.serper_api_key.is_some(),
        searxng_configured: cfg.searxng_url.is_some(),
        searxng_api_key_configured: cfg.searxng_api_key.is_some(),
    }
}

pub async fn restart_managed_proxy(
    state: &AppState,
    port: u16,
) -> Result<ProxyRestartResult, ManagementError> {
    validate_restart_target(state, port).await?;

    crate::docker::create_container(port).await.map_err(|err| {
        error!(port, error = %err, "management API failed to restart proxy");
        ManagementError::new(
            StatusCode::BAD_GATEWAY,
            "proxy_restart_failed",
            format!("Failed to restart proxy port {port}: {err}"),
        )
    })?;

    info!(port, "management API restarted managed proxy");
    let _ = state.event_tx.send(DashboardEvent::ProxyStatus {
        port,
        status: "restarted".to_string(),
        timestamp: unix_timestamp(),
    });

    Ok(ProxyRestartResult {
        port,
        action: "restart",
    })
}

pub fn redact_proxy_url(value: &str) -> String {
    let Some(scheme_end) = value.find("://") else {
        return value.to_string();
    };
    let authority_start = scheme_end + 3;
    let Some(relative_at) = value[authority_start..].find('@') else {
        return value.to_string();
    };
    let at = authority_start + relative_at;
    format!("{}***{}", &value[..authority_start], &value[at..])
}

async fn validate_restart_target(state: &AppState, port: u16) -> Result<(), ManagementError> {
    if is_protected_proxy_port(port) {
        return Err(ManagementError::new(
            StatusCode::FORBIDDEN,
            "protected_proxy",
            format!("Proxy port {port} is protected and cannot be restarted"),
        ));
    }

    if !is_managed_proxy_port(port) {
        return Err(ManagementError::new(
            StatusCode::BAD_REQUEST,
            "invalid_proxy_port",
            format!(
                "Proxy port {port} is out of valid range for managed primary proxies (40001-40003)"
            ),
        ));
    }

    let configured = {
        let pool = state.proxy_pool.read().await;
        pool.proxies.iter().any(|entry| entry.port == port)
    };

    if !configured {
        return Err(ManagementError::new(
            StatusCode::NOT_FOUND,
            "proxy_not_found",
            format!("Proxy port {port} is not present in the active configuration"),
        ));
    }

    Ok(())
}

fn redact_proxy_urls(values: &[String]) -> Vec<String> {
    values.iter().map(|value| redact_proxy_url(value)).collect()
}

fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_proxy_credentials_only() {
        assert_eq!(
            redact_proxy_url("socks5://user:password@127.0.0.1:40001"),
            "socks5://***@127.0.0.1:40001"
        );
        assert_eq!(
            redact_proxy_url("socks5://127.0.0.1:40001"),
            "socks5://127.0.0.1:40001"
        );
    }
}
