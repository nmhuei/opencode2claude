//! HTTP request handlers for the Anthropic-compatible API.

use crate::config::{DEFAULT_MODEL, MSG_ID_SHELL};
use crate::error::BridgeError;
use crate::opencode;
use crate::shell;
use crate::sse::SseEventBuilder;
use crate::state::AppState;

use axum::extract::State;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;

// ── Request types (Anthropic Messages API) ──

/// A single content block within a message, following the Anthropic Messages API format.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct MessageContent {
    /// Content type discriminator (e.g. "text", "image", "tool_use", "tool_result").
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content when `content_type` is "text".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Unique identifier for tool use block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Name of the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Input parameters passed to the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// ID of the tool use block this result corresponds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Result content of the tool execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
}

/// Message content can be either a plain string or a structured array of content blocks.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum ContentVal {
    /// Plain-text message body.
    Single(String),
    /// Structured array of typed content blocks.
    Multiple(Vec<MessageContent>),
}

/// A single message in the Anthropic Messages API conversation array.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Message {
    /// Message role: "user" or "assistant".
    pub role: String,
    /// Message content (plain string or structured blocks).
    pub content: ContentVal,
}

/// A tool definition in Anthropic format.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Request body for POST /v1/messages, matching the Anthropic Messages API schema.
#[derive(Debug, Deserialize, Serialize)]
pub struct MessagesRequest {
    /// Optional model override (falls back to DEFAULT_MODEL when absent).
    pub model: Option<String>,
    /// Ordered conversation turns.
    pub messages: Vec<Message>,
    /// Optional system prompt.
    pub system: Option<serde_json::Value>,
    /// Optional list of tools available to the model.
    pub tools: Option<Vec<AnthropicTool>>,
    /// Optional tool choice policy.
    pub tool_choice: Option<serde_json::Value>,
    /// Whether to stream the response via SSE (default: false).
    #[serde(default)]
    pub stream: bool,
    /// Temperature for model response.
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
}

// ── Helper functions ──

/// Extract the combined user prompt text from the messages array.
pub fn extract_prompt(messages: &[Message]) -> String {
    let mut prompt = String::new();
    for msg in messages {
        if msg.role == "user" {
            match &msg.content {
                ContentVal::Single(text) => {
                    if !prompt.is_empty() {
                        prompt.push('\n');
                    }
                    prompt.push_str(text);
                }
                ContentVal::Multiple(parts) => {
                    for part in parts {
                        if part.content_type == "text" {
                            if let Some(ref t) = part.text {
                                if !prompt.is_empty() {
                                    prompt.push('\n');
                                }
                                prompt.push_str(t);
                            }
                        }
                    }
                }
            }
        }
    }
    prompt.trim().to_string()
}

// ── Handlers ──

/// POST /v1/messages — Main message handler (Anthropic API compatible).
pub async fn handle_messages(
    State(state): State<AppState>,
    Json(payload): Json<MessagesRequest>,
) -> Result<axum::response::Response, BridgeError> {
    // Acquire rate limiter permit if configured
    if let Some(ref limiter) = state.rate_limiter {
        let _permit = limiter.acquire().await.map_err(|_| {
            BridgeError::InvalidRequest("Rate limit exceeded".to_string())
        })?;
    }

    if payload.messages.is_empty() {
        return Err(BridgeError::InvalidRequest("No messages found".to_string()));
    }

    let prompt = extract_prompt(&payload.messages);
    let req_model = state
        .config
        .model
        .clone()
        .or_else(|| payload.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    info!(
        "Incoming request ({} messages) [Model: {}]",
        payload.messages.len(),
        req_model
    );

    if let Some(ref tools) = payload.tools {
        info!(
            "Available tools from client: {:?}",
            tools.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    // Shell command interception: prompts starting with '!' run locally
    if !prompt.is_empty() && prompt.starts_with('!') {
        let shell_cmd = prompt.strip_prefix('!').unwrap().trim().to_string();
        info!("Intercepted local shell command: '{}'", shell_cmd);

        // Check shell policy
        if let Err(reason) = state.config.shell_policy.check(&shell_cmd) {
            return Err(BridgeError::ShellBlocked {
                command: shell_cmd,
                allowed: reason,
            });
        }

        if payload.stream {
            let stream = shell::run_shell_stream(
                shell_cmd,
                req_model,
                state.config.stream_buffer_size,
                state.config.channel_capacity,
            );
            let response = Sse::new(stream)
                .keep_alive(KeepAlive::default())
                .into_response();
            let mut res = response;
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-accel-buffering"),
                axum::http::HeaderValue::from_static("no"),
            );
            Ok(res)
        } else {
            let output = shell::run_shell_sync(&shell_cmd).await;
            let builder = SseEventBuilder::new(MSG_ID_SHELL.to_string(), req_model);
            Ok(Json(builder.non_streaming_response(&output)).into_response())
        }
    } else {
        // OpenCode path — forward directly to upstream API
        if payload.stream {
            let stream = opencode::forward_to_llm_stream(
                &state.http_client,
                payload,
                req_model,
                state.config.channel_capacity,
                state.config.clone(),
            )
            .await?;
            let response = Sse::new(stream)
                .keep_alive(KeepAlive::default())
                .into_response();
            let mut res = response;
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-accel-buffering"),
                axum::http::HeaderValue::from_static("no"),
            );
            Ok(res)
        } else {
            let response = opencode::forward_to_llm_sync(
                &state.http_client,
                payload,
                req_model,
                state.config.clone(),
            )
            .await?;
            Ok(Json(response).into_response())
        }
    }
}

/// GET /v1/models — List available models (Anthropic-compatible format).
pub async fn handle_models(State(state): State<AppState>) -> impl IntoResponse {
    let model_id = state
        .config
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    Json(json!({
        "object": "list",
        "data": [
            {
                "id": model_id,
                "object": "model",
                "created": 0
            }
        ]
    }))
}

/// GET /health — Health check endpoint for monitoring and Docker.
pub async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    let daemon_ok = opencode::check_daemon(&state.http_client, state.config.opencode_port).await;

    Json(json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "daemon": {
            "status": if daemon_ok { "connected" } else { "disconnected" },
            "port": state.config.opencode_port
        },
        "config": {
            "model": state.config.model.as_deref().unwrap_or("(default)"),
            "shell_policy": state.config.shell_policy.description(),
            "auth_enabled": state.config.auth_enabled(),
            "bridge_port": state.config.bridge_port
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: ContentVal) -> Message {
        Message {
            role: role.to_string(),
            content,
        }
    }

    #[test]
    fn test_extract_prompt_single_string() {
        let msgs = vec![make_msg("user", ContentVal::Single("hello world".into()))];
        assert_eq!(extract_prompt(&msgs), "hello world");
    }

    #[test]
    fn test_extract_prompt_multiple_content_blocks() {
        let msgs = vec![make_msg(
            "user",
            ContentVal::Multiple(vec![
                MessageContent {
                    content_type: "text".into(),
                    text: Some("part1".into()),
                    ..Default::default()
                },
                MessageContent {
                    content_type: "image".into(),
                    text: None,
                    ..Default::default()
                },
                MessageContent {
                    content_type: "text".into(),
                    text: Some("part2".into()),
                    ..Default::default()
                },
            ]),
        )];
        assert_eq!(extract_prompt(&msgs), "part1\npart2");
    }

    #[test]
    fn test_extract_prompt_ignores_assistant() {
        let msgs = vec![
            make_msg("assistant", ContentVal::Single("I am AI".into())),
            make_msg("user", ContentVal::Single("hello".into())),
        ];
        assert_eq!(extract_prompt(&msgs), "hello");
    }

    #[test]
    fn test_extract_prompt_empty() {
        let msgs: Vec<Message> = vec![];
        assert_eq!(extract_prompt(&msgs), "");
    }

    #[test]
    fn test_extract_prompt_whitespace_trim() {
        let msgs = vec![make_msg("user", ContentVal::Single("  spaced  ".into()))];
        assert_eq!(extract_prompt(&msgs), "spaced");
    }

    #[test]
    fn test_extract_prompt_multiple_user_messages() {
        let msgs = vec![
            make_msg("user", ContentVal::Single("first".into())),
            make_msg("assistant", ContentVal::Single("reply".into())),
            make_msg("user", ContentVal::Single("second".into())),
        ];
        assert_eq!(extract_prompt(&msgs), "first\nsecond");
    }
}
