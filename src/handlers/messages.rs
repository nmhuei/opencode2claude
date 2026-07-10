//! POST /v1/messages orchestration.

use super::prompt::extract_prompt;
use super::shell;
use super::MessagesRequest;
use crate::config::DEFAULT_MODEL;
use crate::error::BridgeError;
use crate::opencode;
use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use tracing::info;

pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<MessagesRequest>,
) -> Result<Response, BridgeError> {
    let _rate_permit = acquire_rate_permit(&state).await?;
    if payload.messages.is_empty() {
        return Err(BridgeError::InvalidRequest("No messages found".to_string()));
    }

    let model = state
        .config
        .model
        .clone()
        .or_else(|| payload.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    log_request(&payload, &model);

    if let Some(response) = shell::try_handle(&state, &payload, model.clone()).await? {
        return Ok(response);
    }

    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("default-agent")
        .to_string();

    if payload.stream {
        let stream = opencode::forward_to_llm_stream(
            &state,
            api_key,
            payload,
            model,
            state.config.channel_capacity,
            state.search_client.clone(),
            state.config.max_search_loops,
        )
        .await?;
        return Ok(disable_proxy_buffering(
            Sse::new(stream)
                .keep_alive(KeepAlive::default())
                .into_response(),
        ));
    }

    let response = opencode::forward_to_llm_sync(
        &state,
        api_key,
        payload,
        model,
        state.search_client.clone(),
        state.config.max_search_loops,
    )
    .await?;
    Ok(Json(response).into_response())
}

pub(super) fn disable_proxy_buffering(mut response: Response) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

async fn acquire_rate_permit(
    state: &AppState,
) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, BridgeError> {
    match &state.rate_limiter {
        Some(limiter) => limiter
            .acquire()
            .await
            .map(Some)
            .map_err(|_| BridgeError::InvalidRequest("Rate limiter is unavailable".to_string())),
        None => Ok(None),
    }
}

fn log_request(payload: &MessagesRequest, model: &str) {
    let prompt = extract_prompt(&payload.messages);
    info!(
        message_count = payload.messages.len(),
        prompt_chars = prompt.len(),
        %model,
        "incoming messages request"
    );
    if let Some(tools) = &payload.tools {
        info!(
            tools = ?tools.iter().map(|tool| &tool.name).collect::<Vec<_>>(),
            "client tools available"
        );
    }
}
