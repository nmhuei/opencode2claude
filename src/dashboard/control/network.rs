use super::dashboard_error;
use crate::docker::ProxySpec;
use crate::management::service;
use crate::proxy_pool;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

pub async fn handler_proxy_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let action = query.get("action").map(String::as_str).unwrap_or("restart");
    if !matches!(action, "restart" | "purge") {
        return Err(dashboard_error(
            StatusCode::BAD_REQUEST,
            "action must be restart or purge",
        ));
    }
    Ok(Json(json!({
        "action":action,
        "ports":proxy_pool::get_primary_ports(),
        "protected_ports":proxy_pool::get_warm_standby_ports(),
        "steps":if action == "purge" { vec!["remove","create"] } else { vec!["recreate"] },
        "dry_run":true
    })))
}

#[derive(Debug, Deserialize)]
pub struct ConfirmRequest {
    confirm: bool,
}

pub async fn handler_proxy_restart_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ConfirmRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    if !request.confirm {
        return Err(dashboard_error(
            StatusCode::BAD_REQUEST,
            "confirm=true is required",
        ));
    }
    let mut results = Vec::new();
    for port in proxy_pool::get_primary_ports() {
        match service::restart_managed_proxy(&state, port).await {
            Ok(_) => results.push(json!({"port":port,"status":"ok"})),
            Err(error) => results.push(json!({
                "port":port,
                "status":"error",
                "code":error.code,
                "message":error.message
            })),
        }
    }
    Ok(Json(json!({"status":"ok","results":results})))
}

pub async fn handler_proxy_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ConfirmRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    if !request.confirm {
        return Err(dashboard_error(
            StatusCode::BAD_REQUEST,
            "confirm=true is required",
        ));
    }
    let ports = proxy_pool::get_primary_ports();
    {
        let pool = state.proxy_pool.read().await;
        for port in &ports {
            if let Some(index) = pool.proxies.iter().position(|node| node.port == *port) {
                pool.can_modify_node(index)
                    .map_err(|message| dashboard_error(StatusCode::CONFLICT, message))?;
            }
        }
    }

    let mut results = Vec::new();
    for port in ports {
        let spec = ProxySpec::new(port, state.config.runtime.warp_image.clone())
            .map_err(|error| dashboard_error(StatusCode::BAD_REQUEST, error.to_string()))?;
        match state.container_runtime.remove_managed(&spec).await {
            Ok(()) => match state.container_runtime.create_missing(&spec).await {
                Ok(()) => results.push(json!({
                    "port":port,
                    "remove":"ok",
                    "create":"ok"
                })),
                Err(error) => results.push(json!({
                    "port":port,
                    "remove":"ok",
                    "create":"error",
                    "message":error.to_string()
                })),
            },
            Err(error) => results.push(json!({
                "port":port,
                "remove":"error",
                "create":"skipped",
                "message":error.to_string()
            })),
        }
    }
    Ok(Json(json!({"status":"ok","results":results})))
}

pub async fn handler_proxy_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let tail = query
        .get("tail")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let mut logs = Vec::new();
    for port in proxy_pool::get_primary_ports() {
        let spec = ProxySpec::new(port, state.config.runtime.warp_image.clone())
            .map_err(|error| dashboard_error(StatusCode::BAD_REQUEST, error.to_string()))?;
        match state.container_runtime.logs(&spec, tail).await {
            Ok(content) => logs.push(json!({"port":port,"content":content})),
            Err(error) => logs.push(json!({"port":port,"error":error.to_string()})),
        }
    }
    Ok(Json(json!({"tail":tail,"logs":logs})))
}
