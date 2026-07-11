//! Dashboard configuration inspection and typed atomic apply workflow.

use super::auth::{check_admin_mutation, check_admin_token};
use super::events::DashboardEvent;
use super::time::unix_timestamp;
use crate::audit::AuditOutcome;
use crate::management::config_apply;
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// GET /api/dashboard/config/raw — authenticated raw config inspection.
pub async fn handler_config_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;
    let raw = state
        .file_store
        .read(&state.config.management.config_path)
        .ok()
        .and_then(|content| String::from_utf8(content).ok())
        .unwrap_or_default();
    Ok(Json(json!({"raw": raw})))
}

/// POST /api/dashboard/config/save — validate, preview internally, atomically
/// apply, verify, and rollback on failure. Browser-cookie requests require CSRF.
pub async fn handler_config_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    check_admin_mutation(&state, &headers)?;
    let correlation = request_id.map(|Extension(value)| value.0);
    let content = body
        .as_ref()
        .and_then(|Json(payload)| payload.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status":"error",
                    "success":false,
                    "message":"Missing 'content' field in JSON body",
                })),
            )
        })?;

    match config_apply::apply_config(&state, content) {
        Ok(result) => {
            state.audit_log.record(
                "dashboard",
                "config_apply",
                "configuration",
                AuditOutcome::Success,
                correlation,
                BTreeMap::from([
                    (
                        "changed_key_count".to_string(),
                        result.changed_keys.len().to_string(),
                    ),
                    (
                        "restart_required".to_string(),
                        result.restart_required.to_string(),
                    ),
                    (
                        "rollback_performed".to_string(),
                        result.rollback_performed.to_string(),
                    ),
                ]),
            );
            let _ = state.event_tx.send(DashboardEvent::ConfigSaved {
                timestamp: unix_timestamp(),
            });
            Ok(Json(json!({
                "status":"ok",
                "success":true,
                "path":result.path,
                "changed_keys":result.changed_keys,
                "restart_required":result.restart_required,
                "rollback_performed":result.rollback_performed,
            })))
        }
        Err(error) => {
            state.audit_log.record(
                "dashboard",
                "config_apply",
                "configuration",
                AuditOutcome::Failure,
                correlation,
                BTreeMap::from([("error_code".to_string(), error.code.to_string())]),
            );
            Err((
                error.status,
                Json(json!({
                    "status":"error",
                    "success":false,
                    "code":error.code,
                    "message":error.message,
                })),
            ))
        }
    }
}
