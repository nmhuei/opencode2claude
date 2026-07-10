//! Small metadata and token-count endpoints.

use super::MessagesRequest;
use crate::config::DEFAULT_MODEL;
use crate::error::BridgeError;
use crate::opencode;
use crate::state::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

pub async fn handle_count_tokens(
    Json(payload): Json<MessagesRequest>,
) -> Result<axum::response::Response, BridgeError> {
    Ok(Json(json!({
        "input_tokens": opencode::estimate_input_tokens(&payload)
    }))
    .into_response())
}

pub async fn handle_models(State(state): State<AppState>) -> impl IntoResponse {
    let model_id = state
        .config
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    Json(json!({
        "object": "list",
        "data": [{
            "id": model_id,
            "object": "model",
            "created": 0
        }]
    }))
}

pub async fn handle_health(State(_state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
