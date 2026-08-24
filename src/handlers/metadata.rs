//! Metadata, health, readiness, and token-count endpoints.

use super::MessagesRequest;
use crate::config::{EgressMode, DEFAULT_MODEL};
use crate::error::BridgeError;
use crate::opencode;
use crate::state::AppState;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;
use std::time::Duration;

pub async fn handle_count_tokens(
    payload: Result<Json<MessagesRequest>, JsonRejection>,
) -> Result<axum::response::Response, BridgeError> {
    let Json(payload) = payload
        .map_err(|error| BridgeError::InvalidRequest(format!("Invalid request body: {error}")))?;
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

/// Backward-compatible minimal health response.
pub async fn handle_health(State(_state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Process/event-loop liveness. This endpoint deliberately does not disclose topology.
pub async fn handle_liveness() -> impl IntoResponse {
    Json(json!({
        "status": "live",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Operational readiness: critical workers must be healthy and configured egress must be usable.
pub async fn handle_readiness(State(state): State<AppState>) -> impl IntoResponse {
    let heartbeat_budget = state
        .config
        .egress
        .health_interval
        .saturating_mul(3)
        .max(Duration::from_secs(90));
    let workers_ready = state.workers.critical_ready(heartbeat_budget);

    let (egress_ready, verified_unique_exit_ips) = match state.config.egress.mode {
        EgressMode::Direct | EgressMode::Hybrid => (true, 0),
        EgressMode::Proxy => {
            let pool = state.proxy_pool.read().await;
            (
                pool.egress_ready(
                    state.config.egress.minimum_unique_exit_ips,
                    state.config.egress.identity_ttl,
                ),
                pool.verified_unique_exit_count_fresh(state.config.egress.identity_ttl),
            )
        }
    };

    let ready = workers_ready && egress_ready;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(json!({
            "status": if ready { "ready" } else { "not_ready" },
            "checks": {
                "critical_workers": workers_ready,
                "egress": egress_ready,
            },
            "egress": {
                "mode": match state.config.egress.mode {
                    EgressMode::Direct => "direct",
                    EgressMode::Proxy => "proxy",
                    EgressMode::Hybrid => "hybrid",
                },
                "verified_unique_exit_ips": verified_unique_exit_ips,
                "minimum_unique_exit_ips": state.config.egress.minimum_unique_exit_ips,
            },
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}
