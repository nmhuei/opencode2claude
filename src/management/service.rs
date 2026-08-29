//! Transport-agnostic management operations.

use crate::dashboard::DashboardEvent;
use crate::docker::ProxySpec;
use crate::proxy_pool::{
    is_managed_proxy_port, is_protected_proxy_port, ProxyPool, ProxyPoolStats,
    ProxySubsystemSnapshot,
};
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
pub struct EgressOperationalSnapshot {
    pub mode: &'static str,
    pub gateway_ready: bool,
    pub active_route: &'static str,
    pub minimum_unique_exit_ips: usize,
    pub unique_verified_exits: usize,
    pub proxy_subsystem: ProxySubsystemSnapshot,
    pub proxy_pool: ProxyPoolStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyRestartResult {
    pub port: u16,
    pub action: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyDrainResult {
    pub port: u16,
    pub action: &'static str,
    pub draining: bool,
    pub active_requests: usize,
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

pub async fn egress_operational_snapshot(state: &AppState) -> EgressOperationalSnapshot {
    // Keep lock ordering aligned with readiness/routing: subsystem first, then
    // pool, never nested across await points.
    let proxy_subsystem = state.proxy_subsystem.read().await.snapshot();
    let mut pool = state.proxy_pool.write().await;
    pool.recover_expired_cooldowns();
    let unique_verified_exits =
        pool.verified_unique_exit_count_fresh(state.config.egress.identity_ttl);
    let proxy_routable = pool.egress_ready(
        state.config.egress.minimum_unique_exit_ips,
        state.config.egress.identity_ttl,
    );
    let proxy_pool = pool.snapshot();
    drop(pool);

    let (mode, gateway_ready, active_route) = match state.config.egress.mode {
        crate::config::EgressMode::Direct => ("direct", true, "direct"),
        crate::config::EgressMode::Proxy => ("proxy", proxy_routable, "proxy"),
        crate::config::EgressMode::Hybrid => (
            "hybrid",
            true,
            if proxy_subsystem.ready && proxy_routable {
                "proxy"
            } else {
                "direct"
            },
        ),
    };

    EgressOperationalSnapshot {
        mode,
        gateway_ready,
        active_route,
        minimum_unique_exit_ips: state.config.egress.minimum_unique_exit_ips,
        unique_verified_exits,
        proxy_subsystem,
        proxy_pool,
    }
}

pub async fn safe_config_snapshot(state: &AppState) -> SafeConfigSnapshot {
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
        client_auth_configured: state.api_keys.read().await.configured(),
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
    let spec = ProxySpec::new(port, state.config.runtime.warp_image.clone()).map_err(|err| {
        ManagementError::new(
            StatusCode::BAD_REQUEST,
            "invalid_proxy_spec",
            err.to_string(),
        )
    })?;
    let index = prepare_restart_target(state, port).await?;

    state.metrics.record_proxy_restart_attempt();
    if let Err(err) = state.container_runtime.recreate_managed(&spec).await {
        state
            .proxy_pool
            .write()
            .await
            .mark_manual_restart_failed(index);
        state.metrics.record_proxy_restart_failure();
        error!(port, error = %err, "management API failed to restart proxy");
        return Err(ManagementError::new(
            StatusCode::BAD_GATEWAY,
            "proxy_restart_failed",
            format!("Failed to restart proxy port {port}: {err}"),
        ));
    }
    state.metrics.record_proxy_restart_success();

    info!(port, "management API restarted managed proxy");
    let _ = state.event_tx.send(DashboardEvent::ProxyStatus {
        port,
        status: "recovering".to_string(),
        timestamp: unix_timestamp(),
    });

    Ok(ProxyRestartResult {
        port,
        action: "restart",
    })
}

pub async fn set_managed_proxy_drain(
    state: &AppState,
    port: u16,
    draining: bool,
) -> Result<ProxyDrainResult, ManagementError> {
    let mut pool = state.proxy_pool.write().await;
    let index = managed_proxy_index(&pool, port)?;
    let active_requests = if draining {
        pool.begin_drain(index).map_err(|message| {
            ManagementError::new(StatusCode::CONFLICT, "proxy_drain_failed", message)
        })?
    } else {
        pool.cancel_drain(index).map_err(|message| {
            ManagementError::new(StatusCode::CONFLICT, "proxy_undrain_failed", message)
        })?;
        pool.proxies[index].active_request_count()
    };
    drop(pool);

    let status = if draining {
        "draining"
    } else {
        "drain_cancelled"
    };
    let _ = state.event_tx.send(DashboardEvent::ProxyStatus {
        port,
        status: status.to_string(),
        timestamp: unix_timestamp(),
    });
    info!(port, active_requests, draining, "proxy drain state changed");

    Ok(ProxyDrainResult {
        port,
        action: if draining { "drain" } else { "undrain" },
        draining,
        active_requests,
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

fn managed_proxy_index(pool: &ProxyPool, port: u16) -> Result<usize, ManagementError> {
    if is_protected_proxy_port(port) {
        return Err(ManagementError::new(
            StatusCode::FORBIDDEN,
            "protected_proxy",
            format!("Proxy port {port} is protected and cannot be modified"),
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

    pool.proxies
        .iter()
        .position(|entry| entry.port == port)
        .ok_or_else(|| {
            ManagementError::new(
                StatusCode::NOT_FOUND,
                "proxy_not_found",
                format!("Proxy port {port} is not present in the active configuration"),
            )
        })
}

async fn prepare_restart_target(state: &AppState, port: u16) -> Result<usize, ManagementError> {
    let mut pool = state.proxy_pool.write().await;
    let index = managed_proxy_index(&pool, port)?;
    pool.begin_manual_restart(index)
        .map_err(|message| ManagementError::new(StatusCode::CONFLICT, "proxy_busy", message))?;

    Ok(index)
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

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use crate::config::{BridgeConfig, EgressMode};
    use crate::docker::{
        ContainerRuntime, ContainerState, ContainerSummary, DockerResult, ProxySpec,
    };
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeRuntime {
        recreated: Mutex<Vec<u16>>,
    }

    #[async_trait]
    impl ContainerRuntime for FakeRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".into())
        }
        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
            Ok(ContainerState {
                exists: true,
                running: true,
                has_expected_volume: true,
            })
        }
        async fn create_missing(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn recreate_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.recreated.lock().unwrap().push(spec.port);
            Ok(())
        }
        async fn remove_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn restart_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn stop_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn start_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn logs(&self, _spec: &ProxySpec, _tail: usize) -> DockerResult<String> {
            Ok(String::new())
        }
        async fn list(&self, _specs: &[ProxySpec]) -> DockerResult<Vec<ContainerSummary>> {
            Ok(Vec::new())
        }
    }

    fn state(runtime: Arc<FakeRuntime>) -> AppState {
        let mut config = BridgeConfig::default();
        config.egress.mode = EgressMode::Proxy;
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        config.warm_standby_proxies = None;
        config.egress.identity_endpoints.clear();
        AppState::new_with_container_runtime(config, runtime)
    }

    #[tokio::test]
    async fn management_restart_delegates_to_injected_runtime() {
        let runtime = Arc::new(FakeRuntime::default());
        let state = state(runtime.clone());
        let result = restart_managed_proxy(&state, 40001)
            .await
            .expect("restart result");
        assert_eq!(result.port, 40001);
        assert_eq!(*runtime.recreated.lock().unwrap(), vec![40001]);
        let pool = state.proxy_pool.read().await;
        assert_eq!(
            pool.proxies[0].health,
            crate::proxy_pool::HealthState::Recovering
        );
        assert_eq!(
            pool.proxies[0].circuit,
            crate::proxy_pool::CircuitState::HalfOpen
        );
        assert!(pool.restart_queue.is_empty());
        drop(pool);
        let metrics = state.metrics.snapshot();
        assert_eq!(metrics.proxy_restart_attempts, 1);
        assert_eq!(metrics.proxy_restart_successes, 1);
        assert_eq!(metrics.proxy_restart_failures, 0);
    }

    #[tokio::test]
    async fn management_restart_rejects_active_request_lease() {
        let runtime = Arc::new(FakeRuntime::default());
        let state = state(runtime.clone());
        let lease = state.proxy_pool.read().await.begin_lease(0).expect("lease");
        let error = restart_managed_proxy(&state, 40001)
            .await
            .expect_err("busy node must fail");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(runtime.recreated.lock().unwrap().is_empty());
        drop(lease);
    }

    #[tokio::test]
    async fn management_drain_preserves_leases_and_is_reversible() {
        let runtime = Arc::new(FakeRuntime::default());
        let state = state(runtime.clone());
        let lease = state.proxy_pool.read().await.begin_lease(0).expect("lease");

        let drained = set_managed_proxy_drain(&state, 40001, true)
            .await
            .expect("drain result");
        assert!(drained.draining);
        assert_eq!(drained.active_requests, 1);
        {
            let pool = state.proxy_pool.read().await;
            assert!(pool.proxies[0].draining);
            assert_eq!(pool.proxies[0].active_request_count(), 1);
        }
        assert!(runtime.recreated.lock().unwrap().is_empty());

        let restored = set_managed_proxy_drain(&state, 40001, false)
            .await
            .expect("undrain result");
        assert!(!restored.draining);
        assert_eq!(restored.active_requests, 1);
        assert!(!state.proxy_pool.read().await.proxies[0].draining);
        drop(lease);
    }

    #[tokio::test]
    async fn management_drain_rejects_protected_standby() {
        let runtime = Arc::new(FakeRuntime::default());
        let mut config = BridgeConfig::default();
        config.egress.mode = EgressMode::Proxy;
        config.primary_proxies = None;
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);
        config.egress.identity_endpoints.clear();
        let state = AppState::new_with_container_runtime(config, runtime);
        let error = set_managed_proxy_drain(&state, 40004, true)
            .await
            .expect_err("protected node must fail");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn management_restart_rejects_protected_standby_before_runtime_call() {
        let runtime = Arc::new(FakeRuntime::default());
        let mut config = BridgeConfig::default();
        config.egress.mode = EgressMode::Proxy;
        config.primary_proxies = None;
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);
        config.egress.identity_endpoints.clear();
        let state = AppState::new_with_container_runtime(config, runtime.clone());
        let error = restart_managed_proxy(&state, 40004)
            .await
            .expect_err("protected node must fail");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert!(runtime.recreated.lock().unwrap().is_empty());
    }
}
