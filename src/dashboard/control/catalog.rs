use super::dashboard_error;
use crate::application::models;
use crate::audit::AuditOutcome;
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub async fn handler_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    Ok(Json(json!({
        "groups": [
            {"id":"server","actions":["status","start_state","restart","stop","logs","config"]},
            {"id":"proxy","actions":["status","drain","undrain","plan_restart","restart","plan_purge","purge","logs"]},
            {"id":"dashboard","actions":["status","session","events"]},
            {"id":"integration","actions":["env","doctor","completion"]},
            {"id":"configuration","actions":["view","template","init","preview","apply","select_model"]},
            {"id":"access","actions":["list_api_keys","create_api_key","read_api_key","update_api_key","verify_api_key","rotate_api_key","disable_api_key","enable_api_key","revoke_api_key","generate_client_config","hot_reload_policy"]},
            {"id":"update","actions":["check","apply"]}
        ],
        "constraints": {
            "server_start": "Dashboard is reachable only while the server is already running; start is represented as idempotent running state.",
            "destructive_actions_require_confirmation": true,
            "protected_standby_mutation": false,
            "drain_preserves_active_leases": true
        }
    })))
}

pub async fn handler_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let catalog = models::free_models()
        .iter()
        .map(|model| {
            json!({
                "id": model.id,
                "label": model.label,
                "provider": model.provider,
                "protocol": model.protocol,
                "limited_time": model.limited_time,
                "privacy_notice": model.privacy_notice,
                "capabilities": crate::opencode::mapper::model_capabilities(model.id),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "selected": state.config.model,
        "models": catalog,
        "source": "OpenCode Zen official free catalog",
        "catalog_date": "2026-07-21"
    })))
}

#[derive(Debug, Deserialize)]
pub struct SelectModelRequest {
    model: String,
}

pub async fn handler_select_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<SelectModelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let model = request.model.trim();
    match models::select_free_model(&state, model) {
        Ok(result) => {
            state.audit_log.record(
                "dashboard",
                "model_select",
                model,
                AuditOutcome::Success,
                request_id.map(|Extension(value)| value.0),
                BTreeMap::from([("restart_required".to_string(), "true".to_string())]),
            );
            Ok(Json(json!({
                "status":"ok",
                "model":model,
                "changed_keys":result.changed_keys,
                "restart_required":true
            })))
        }
        Err(error) => Err(dashboard_error(error.status, error.message)),
    }
}
