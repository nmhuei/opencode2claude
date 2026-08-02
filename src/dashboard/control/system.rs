use super::dashboard_error;
use crate::application::lifecycle;
use crate::runtime::RuntimePaths;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

pub async fn handler_server_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let tail = query
        .get("tail")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 5000);
    let path = RuntimePaths::from_config(&state.config).bridge_log();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines = text.lines().rev().take(tail).collect::<Vec<_>>();
    let content = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
    Ok(Json(json!({
        "path":path.display().to_string(),
        "tail":tail,
        "content":content
    })))
}

pub async fn handler_server_restart(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let task = lifecycle::schedule_server_action(&state, lifecycle::ServerAction::Restart)
        .map_err(|error| dashboard_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"status":"accepted","action":"restart","task":task})),
    ))
}

pub async fn handler_server_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let task = lifecycle::schedule_server_action(&state, lifecycle::ServerAction::Stop)
        .map_err(|error| dashboard_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"status":"accepted","action":"stop","task":task})),
    ))
}

pub async fn handler_update_check(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let release = crate::update::fetch_latest_release(&state.http_client)
        .await
        .map_err(|error| dashboard_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
    let available = crate::update::has_update(crate::update::current_version(), &release);
    Ok(Json(json!({
        "current":crate::update::current_version(),
        "latest":release.version,
        "tag":release.tag,
        "available":available,
        "asset_available":crate::update::find_matching_asset(&release).is_some(),
        "notes":release.body
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    confirm: bool,
    #[serde(default)]
    force: bool,
}

pub async fn handler_update_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    if !request.confirm {
        return Err(dashboard_error(
            StatusCode::BAD_REQUEST,
            "confirm=true is required",
        ));
    }
    let mut args = vec!["update".to_string()];
    if request.force {
        args.push("--force".to_string());
    }
    let task = lifecycle::schedule_cli_command(&state, "self-update", args)
        .map_err(|error| dashboard_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"status":"accepted","task":task,"restart_required":true})),
    ))
}
