//! Dashboard status, safe configuration, diagnostics, and proxy actions.

use super::auth::{check_admin_mutation, check_admin_token};
use super::time::uptime_string;
use crate::audit::AuditOutcome;
use crate::management::{auth, service};
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// GET /api/dashboard/status — bridge status with uptime and proxy tier stats.
pub async fn handler_rest_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;
    let egress = service::egress_operational_snapshot(&state).await;
    let snapshot = &egress.proxy_pool;
    let uptime_secs = service::uptime_secs(&state);
    let auth_enabled = state.api_keys.read().await.configured();

    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "uptime": uptime_string(uptime_secs),
        "uptime_secs": uptime_secs,
        "model": state.config.model,
        "bridge_port": state.config.bridge_port,
        "auth_enabled": auth_enabled,
        "admin_token_configured": auth::dashboard_token(&state.config).is_some(),
        "shell_policy": state.config.shell_policy.kind(),
        "egress": {
            "mode": egress.mode,
            "ready": egress.gateway_ready,
            "active_route": egress.active_route,
            "minimum_unique_exit_ips": egress.minimum_unique_exit_ips,
            "unique_verified_exits": egress.unique_verified_exits,
            "proxy_subsystem": egress.proxy_subsystem,
        },
        "primary_proxies": {
            "total": snapshot.primary.total,
            "healthy": snapshot.primary.healthy,
            "degraded": snapshot.primary.degraded,
            "cooldown": snapshot.primary.cooldown,
            "recovering": snapshot.primary.recovering,
            "dead": snapshot.primary.dead,
        },
        "warm_standby": {
            "total": snapshot.warm_standby.total,
            "healthy": snapshot.warm_standby.healthy,
            "degraded": snapshot.warm_standby.degraded,
            "cooldown": snapshot.warm_standby.cooldown,
            "recovering": snapshot.warm_standby.recovering,
            "dead": snapshot.warm_standby.dead,
        },
    })))
}

/// GET /api/dashboard/proxies — detailed proxy node list.
pub async fn handler_proxies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;
    let snapshot = service::proxy_snapshot(&state).await;
    Ok(Json(snapshot.nodes))
}

/// GET /api/dashboard/config — active configuration with configured booleans instead of masked secrets.
pub async fn handler_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;
    let cfg = service::safe_config_snapshot(&state).await;
    Ok(Json(json!({
        "host": cfg.host,
        "bridge_port": cfg.bridge_port,
        "model": cfg.model,
        "shell_policy": cfg.shell_policy,
        "shell_policy_label": cfg.shell_policy_label,
        "tavily_api_key_configured": cfg.tavily_configured,
        "exa_api_key_configured": cfg.exa_configured,
        "serper_api_key_configured": cfg.serper_configured,
        "auth_tokens_configured": cfg.client_auth_configured,
        "searxng_url": state.config.searxng_url,
        "searxng_api_key_configured": cfg.searxng_api_key_configured,
        "shell_allowlist": cfg.shell_allowlist,
        "max_body_size": cfg.max_body_size,
        "stream_buffer_size": cfg.stream_buffer_size,
        "channel_capacity": cfg.channel_capacity,
        "max_search_loops": cfg.max_search_loops,
        "primary_proxies": cfg.primary_proxies,
        "warm_standby_proxies": cfg.warm_standby_proxies,
    })))
}

/// POST /api/dashboard/proxy/:port/restart — restart a managed proxy container.
pub async fn handler_proxy_restart(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Path(port): Path<u16>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_mutation(&state, &headers)?;
    let correlation = request_id.map(|Extension(value)| value.0);

    match service::restart_managed_proxy(&state, port).await {
        Ok(result) => {
            state.audit_log.record(
                "dashboard",
                "proxy_restart",
                format!("proxy:{port}"),
                AuditOutcome::Success,
                correlation,
                BTreeMap::new(),
            );
            Ok(Json(json!({
                "status": "ok",
                "port": result.port,
            })))
        }
        Err(error) => {
            state.audit_log.record(
                "dashboard",
                "proxy_restart",
                format!("proxy:{port}"),
                AuditOutcome::Failure,
                correlation,
                BTreeMap::from([("error_code".to_string(), error.code.to_string())]),
            );
            Ok(Json(json!({
                "status": "error",
                "message": error.message,
            })))
        }
    }
}

/// POST /api/dashboard/proxy/:port/drain — stop fresh routing while leases finish.
pub async fn handler_proxy_drain(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Path(port): Path<u16>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    set_proxy_drain(state, headers, request_id, port, true).await
}

/// POST /api/dashboard/proxy/:port/undrain — restore fresh routing eligibility.
pub async fn handler_proxy_undrain(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Path(port): Path<u16>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    set_proxy_drain(state, headers, request_id, port, false).await
}

async fn set_proxy_drain(
    state: AppState,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    port: u16,
    draining: bool,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_mutation(&state, &headers)?;
    let correlation = request_id.map(|Extension(value)| value.0);
    let action = if draining {
        "proxy_drain"
    } else {
        "proxy_undrain"
    };

    match service::set_managed_proxy_drain(&state, port, draining).await {
        Ok(result) => {
            state.audit_log.record(
                "dashboard",
                action,
                format!("proxy:{port}"),
                AuditOutcome::Success,
                correlation,
                BTreeMap::from([(
                    "active_requests".to_string(),
                    result.active_requests.to_string(),
                )]),
            );
            Ok(Json(json!({
                "status": "ok",
                "port": result.port,
                "draining": result.draining,
                "active_requests": result.active_requests,
            })))
        }
        Err(error) => {
            state.audit_log.record(
                "dashboard",
                action,
                format!("proxy:{port}"),
                AuditOutcome::Failure,
                correlation,
                BTreeMap::from([("error_code".to_string(), error.code.to_string())]),
            );
            Ok(Json(json!({
                "status": "error",
                "message": error.message,
            })))
        }
    }
}

/// GET /api/dashboard/diagnostics — Rich operational status for authenticated admin.
pub async fn handler_dashboard_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;

    let daemon_ok =
        crate::opencode::check_daemon(&state.http_client, state.config.opencode_port).await;
    let proxy_pool_stats = service::proxy_snapshot(&state).await;
    let auth_enabled = state.api_keys.read().await.configured();

    Ok(Json(json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "daemon": {
            "status": if daemon_ok { "connected" } else { "disconnected" },
            "port": state.config.opencode_port
        },
        "config": {
            "model": state.config.model.as_deref().unwrap_or("(default)"),
            "shell_policy": state.config.shell_policy.kind(),
            "auth_enabled": auth_enabled,
            "bridge_port": state.config.bridge_port
        },
        "proxy_pool": proxy_pool_stats
    })))
}
