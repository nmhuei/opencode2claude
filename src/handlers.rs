//! HTTP request handlers for the Anthropic-compatible API.

use crate::config::DEFAULT_MODEL;
use crate::error::BridgeError;
use crate::opencode;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use futures_util::StreamExt;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
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

pub async fn handle_messages(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<MessagesRequest>,
) -> Result<axum::response::Response, BridgeError> {
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("default-agent")
        .to_string();
    // Acquire rate limiter permit if configured — must live for the full handler
    let _rate_permit = match state.rate_limiter {
        Some(ref limiter) => Some(
            limiter
                .acquire()
                .await
                .map_err(|_| BridgeError::InvalidRequest("Rate limit exceeded".to_string()))?,
        ),
        None => None,
    };

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

    // Detect if we are in the second turn of a local shell execution (getting result back from Claude Code)
    let mut is_shell_result = false;
    let mut shell_result_text = String::new();

    if let Some(last_msg) = payload.messages.last() {
        if last_msg.role == "user" {
            if let ContentVal::Multiple(blocks) = &last_msg.content {
                for block in blocks {
                    if block.content_type == "tool_result"
                        && block.tool_use_id.as_deref() == Some("toolu_local_shell")
                    {
                        is_shell_result = true;
                        if let Some(ref content_val) = block.content {
                            shell_result_text =
                                opencode::mapper::tool_result_content_to_string(content_val);
                        }
                        break;
                    }
                }
            }
        }
    }

    if is_shell_result {
        info!(
            "Received local shell execution result from client (length: {})",
            shell_result_text.len()
        );
        if payload.stream {
            let (tx, rx) = tokio::sync::mpsc::channel(10);
            let builder = SseEventBuilder::new("msg_local_shell_result".to_string(), req_model);
            let output = shell_result_text;
            tokio::spawn(async move {
                let _ = tx.send(builder.message_start()).await;
                let _ = tx.send(builder.content_block_start()).await;
                let _ = tx.send(builder.text_delta(&output)).await;
                let _ = tx.send(builder.content_block_stop()).await;
                let _ = tx.send(builder.message_delta()).await;
                let _ = tx.send(builder.message_stop()).await;
            });
            let response = Sse::new(
                tokio_stream::wrappers::ReceiverStream::new(rx)
                    .map(Ok::<_, std::convert::Infallible>),
            )
            .keep_alive(KeepAlive::default())
            .into_response();
            let mut res = response;
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-accel-buffering"),
                axum::http::HeaderValue::from_static("no"),
            );
            Ok(res)
        } else {
            let builder = SseEventBuilder::new("msg_local_shell_result".to_string(), req_model);
            Ok(Json(builder.non_streaming_response(&shell_result_text)).into_response())
        }
    } else if !prompt.is_empty() && prompt.starts_with('!') {
        let shell_cmd = prompt.strip_prefix('!').unwrap().trim().to_string();
        info!(
            "Intercepted local shell command for delegation: '{}'",
            shell_cmd
        );

        // Enforce shell policy before delegating to client
        state
            .config
            .shell_policy
            .check(&shell_cmd)
            .map_err(|_| BridgeError::ShellDisabled)?;

        let mut shell_tool_name = "bash".to_string();
        let mut param_name = "command".to_string();

        if let Some(ref tools) = payload.tools {
            for tool in tools {
                let name_lower = tool.name.to_lowercase();
                if name_lower == "bash"
                    || name_lower == "execute_command"
                    || name_lower == "run_command"
                {
                    shell_tool_name = tool.name.clone();
                    if let Some(properties) = tool
                        .input_schema
                        .get("properties")
                        .and_then(|p| p.as_object())
                    {
                        if properties.contains_key("command") {
                            param_name = "command".to_string();
                        } else if properties.contains_key("cmd") {
                            param_name = "cmd".to_string();
                        } else if !properties.is_empty() {
                            param_name = properties.keys().next().unwrap().clone();
                        }
                    }
                    break;
                }
            }
        }

        let tool_use_id = "toolu_local_shell".to_string();

        if payload.stream {
            let (tx, rx) = tokio::sync::mpsc::channel(10);
            let builder = SseEventBuilder::new("msg_local_shell".to_string(), req_model);
            let tool_name = shell_tool_name;
            let p_name = param_name;
            let cmd = shell_cmd;
            let t_id = tool_use_id;

            tokio::spawn(async move {
                let _ = tx.send(builder.message_start()).await;

                let start_ev = Event::default()
                    .event("content_block_start")
                    .json_data(serde_json::json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "tool_use",
                            "id": t_id,
                            "name": tool_name,
                            "input": {}
                        }
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(start_ev).await;

                let args = serde_json::json!({ p_name: cmd }).to_string();
                let delta_ev = Event::default()
                    .event("content_block_delta")
                    .json_data(serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": args
                        }
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(delta_ev).await;

                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;

                let delta_ev = Event::default()
                    .event("message_delta")
                    .json_data(serde_json::json!({
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": "tool_use",
                            "stop_sequence": null
                        },
                        "usage": {"output_tokens": 0}
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(delta_ev).await;

                let stop_ev = Event::default()
                    .event("message_stop")
                    .json_data(serde_json::json!({
                        "type": "message_stop"
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            });

            let response = Sse::new(
                tokio_stream::wrappers::ReceiverStream::new(rx)
                    .map(Ok::<_, std::convert::Infallible>),
            )
            .keep_alive(KeepAlive::default())
            .into_response();
            let mut res = response;
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-accel-buffering"),
                axum::http::HeaderValue::from_static("no"),
            );
            Ok(res)
        } else {
            let resp_val = serde_json::json!({
                "id": "msg_local_shell",
                "type": "message",
                "role": "assistant",
                "model": req_model,
                "content": [
                    {
                        "type": "tool_use",
                        "id": tool_use_id,
                        "name": shell_tool_name,
                        "input": {
                            param_name: shell_cmd
                        }
                    }
                ],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            });
            Ok(Json(resp_val).into_response())
        }
    } else {
        // OpenCode path — forward directly to upstream API
        if payload.stream {
            let stream = opencode::forward_to_llm_stream(
                &state,
                api_key,
                payload,
                req_model,
                state.config.channel_capacity,
                state.search_client.clone(),
                state.config.max_search_loops,
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
                &state,
                api_key,
                payload,
                req_model,
                state.search_client.clone(),
                state.config.max_search_loops,
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

    let proxy_pool_stats = state.proxy_pool.read().await.snapshot();

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
        },
        "proxy_pool": proxy_pool_stats
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
