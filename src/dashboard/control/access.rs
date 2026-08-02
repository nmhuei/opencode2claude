use super::dashboard_error;
use crate::application::{client_config, completion, integration};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn handler_doctor(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let report = crate::doctor::run_diagnostics().await;
    Ok(Json(json!({
        "exit_code": report.summary.exit_code(),
        "report": report
    })))
}

pub async fn handler_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    Ok(Json(json!({
        "metrics": state.metrics.snapshot(),
        "workers": state.workers.snapshot()
    })))
}

pub async fn handler_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    Ok(Json(json!({"events": state.audit_log.snapshot(100)})))
}

pub async fn handler_environment(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    Ok(Json(json!(integration::environment(&state.config))))
}

#[derive(Debug, Deserialize)]
pub struct ClientConfigRequest {
    format: String,
    #[serde(default)]
    key_source: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

pub async fn handler_client_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ClientConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let format = request
        .format
        .parse::<client_config::ClientConfigFormat>()
        .map_err(|error| dashboard_error(StatusCode::BAD_REQUEST, error))?;
    let mut environment = integration::environment(&state.config);
    if let Some(model) = request.model.filter(|value| !value.trim().is_empty()) {
        environment.model = Some(model.trim().to_string());
    }
    let source = if request.key_source.trim().is_empty() {
        "placeholder"
    } else {
        request.key_source.trim()
    };
    let (key, contains_secret) = match source {
        "placeholder" => ("sk-oc2-REPLACE_ME".to_string(), false),
        "active" => (environment.api_key.clone(), true),
        "latest" | "provided" => {
            let key = request
                .api_key
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    dashboard_error(
                        StatusCode::BAD_REQUEST,
                        "api_key is required for latest/provided source",
                    )
                })?;
            (key, true)
        }
        _ => {
            return Err(dashboard_error(
                StatusCode::BAD_REQUEST,
                "key_source must be placeholder, active, latest, or provided",
            ))
        }
    };
    Ok(Json(json!(client_config::generate(
        format,
        &environment,
        &key,
        contains_secret
    ))))
}

pub async fn handler_completion(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(shell): Path<String>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let content = completion::generate_completion(&shell)
        .map_err(|error| dashboard_error(StatusCode::BAD_REQUEST, error))?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        content,
    )
        .into_response())
}

pub async fn handler_config_template(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        crate::init::config_template(),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct InitConfigRequest {
    #[serde(default)]
    force: bool,
}

pub async fn handler_config_init(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InitConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let path = &state.config.management.config_path;
    if path.exists() && !request.force {
        return Err(dashboard_error(
            StatusCode::CONFLICT,
            "Configuration already exists; force=true is required to overwrite it",
        ));
    }
    state
        .file_store
        .atomic_write(path, crate::init::config_template().as_bytes(), true)
        .map_err(|error| dashboard_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({
        "status":"ok",
        "path":path.display().to_string(),
        "restart_required":true
    })))
}
