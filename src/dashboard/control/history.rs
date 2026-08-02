//! Authenticated request-history management endpoints.

use super::dashboard_error;
use crate::audit::AuditOutcome;
use crate::history::{
    HistoryExportRequest, HistoryPurgeRequest, HistoryQuery, HistorySettingsPatch,
};
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub async fn handler_history_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let history = state.history.clone();
    let page = tokio::task::spawn_blocking(move || history.list(query))
        .await
        .map_err(join_error)?
        .map_err(history_error)?;
    Ok(Json(json!(page)))
}

pub async fn handler_history_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let history = state.history.clone();
    let (stats, storage) = tokio::task::spawn_blocking(move || {
        let stats = history.stats()?;
        let storage = history.storage_status()?;
        Ok::<_, crate::history::HistoryError>((stats, storage))
    })
    .await
    .map_err(join_error)?
    .map_err(history_error)?;
    Ok(Json(json!({"stats":stats,"storage":storage})))
}

pub async fn handler_history_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let history = state.history.clone();
    let requested_id = id.clone();
    let detail = tokio::task::spawn_blocking(move || history.detail(&requested_id))
        .await
        .map_err(join_error)?
        .map_err(history_error)?
        .ok_or_else(|| dashboard_error(StatusCode::NOT_FOUND, "History request was not found"))?;
    state.audit_log.record(
        "dashboard",
        "history_view_detail",
        id,
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::new(),
    );
    Ok(Json(json!({"detail":detail})))
}

pub async fn handler_history_content(
    State(state): State<AppState>,
    Path((id, kind)): Path<(String, String)>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let history = state.history.clone();
    let lookup_id = id.clone();
    let lookup_kind = kind.clone();
    let content = tokio::task::spawn_blocking(move || history.content(&lookup_id, &lookup_kind))
        .await
        .map_err(join_error)?
        .map_err(history_error)?
        .ok_or_else(|| dashboard_error(StatusCode::NOT_FOUND, "History content was not found"))?;
    state.audit_log.record(
        "dashboard",
        "history_view_content",
        id,
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([("content_kind".to_string(), kind)]),
    );
    Ok(Json(json!({"content":content})))
}

pub async fn handler_history_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let settings = state.history.settings_view();
    let history = state.history.clone();
    let storage = tokio::task::spawn_blocking(move || history.storage_status())
        .await
        .map_err(join_error)?
        .map_err(history_error)?;
    Ok(Json(json!({"settings":settings,"storage":storage})))
}

pub async fn handler_history_settings_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(patch): Json<HistorySettingsPatch>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let history = state.history.clone();
    let settings = tokio::task::spawn_blocking(move || history.update_settings(patch))
        .await
        .map_err(join_error)?
        .map_err(history_error)?;
    state.audit_log.record(
        "dashboard",
        "history_settings_update",
        "request_history",
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([
            ("enabled".to_string(), settings.enabled.to_string()),
            (
                "capture_mode".to_string(),
                settings.capture_mode.to_string(),
            ),
            (
                "retention_days".to_string(),
                settings.retention_days.to_string(),
            ),
        ]),
    );
    Ok(Json(json!({"status":"ok","settings":settings})))
}

pub async fn handler_history_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let history = state.history.clone();
    let delete_id = id.clone();
    let deleted = tokio::task::spawn_blocking(move || history.delete(&delete_id))
        .await
        .map_err(join_error)?
        .map_err(history_error)?;
    if !deleted {
        return Err(dashboard_error(
            StatusCode::NOT_FOUND,
            "History request was not found",
        ));
    }
    state.audit_log.record(
        "dashboard",
        "history_delete",
        id,
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::new(),
    );
    Ok(Json(json!({"status":"ok","deleted":true})))
}

pub async fn handler_history_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<HistoryPurgeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let history = state.history.clone();
    let audit_filter = format!(
        "all={},before_ms={:?},status={:?}",
        request.all, request.before_ms, request.status
    );
    let deleted = tokio::task::spawn_blocking(move || history.purge(&request))
        .await
        .map_err(join_error)?
        .map_err(history_error)?;
    state.audit_log.record(
        "dashboard",
        "history_purge",
        "request_history",
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([
            ("deleted".to_string(), deleted.to_string()),
            ("filter".to_string(), audit_filter),
        ]),
    );
    Ok(Json(json!({"status":"ok","deleted":deleted})))
}

pub async fn handler_history_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<HistoryExportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let format = request.format.clone();
    let history = state.history.clone();
    let export = tokio::task::spawn_blocking(move || history.export(request))
        .await
        .map_err(join_error)?
        .map_err(history_error)?;
    state.audit_log.record(
        "dashboard",
        "history_export",
        "request_history",
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([
            ("format".to_string(), format),
            ("record_count".to_string(), export.records.len().to_string()),
        ]),
    );
    Ok(Json(json!(export)))
}

fn history_error(error: crate::history::HistoryError) -> (StatusCode, Json<Value>) {
    match error {
        crate::history::HistoryError::NotFound => {
            dashboard_error(StatusCode::NOT_FOUND, error.to_string())
        }
        crate::history::HistoryError::Invalid(_) => {
            dashboard_error(StatusCode::BAD_REQUEST, error.to_string())
        }
        crate::history::HistoryError::Unavailable => {
            dashboard_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
        }
        _ => dashboard_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

fn join_error(error: tokio::task::JoinError) -> (StatusCode, Json<Value>) {
    dashboard_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("history task failed: {error}"),
    )
}
