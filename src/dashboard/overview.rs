//! Dashboard status, safe configuration, diagnostics, and proxy actions.

use super::auth::{check_admin_mutation, check_admin_token};
use super::time::uptime_string;
use crate::management::{auth, service};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

/// GET /api/dashboard/status — bridge status with uptime and proxy tier stats.
pub async fn handler_rest_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;
    let snapshot = service::proxy_snapshot(&state).await;
    let uptime_secs = service::uptime_secs(&state);

    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime": uptime_string(uptime_secs),
        "uptime_secs": uptime_secs,
        "model": state.config.model,
        "bridge_port": state.config.bridge_port,
        "auth_enabled": state.config.auth_enabled(),
        "admin_token_configured": auth::dashboard_token(&state.config).is_some(),
        "shell_policy": state.config.shell_policy.kind(),
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
    let cfg = service::safe_config_snapshot(&state);
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
    Path(port): Path<u16>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_mutation(&state, &headers)?;

    match service::restart_managed_proxy(&state, port).await {
        Ok(result) => Ok(Json(json!({
            "status": "ok",
            "port": result.port,
        }))),
        Err(error) => Ok(Json(json!({
            "status": "error",
            "message": error.message,
        }))),
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
            "auth_enabled": state.config.auth_enabled(),
            "bridge_port": state.config.bridge_port
        },
        "proxy_pool": proxy_pool_stats
    })))
}
