//! Local shell delegation protocol.
//!
//! The bridge never executes the command here. It emits a tool_use block for the
//! client and later echoes the matching tool_result back as an assistant response.

use super::prompt::{last_user_shell_cmd, local_shell_result};
use super::{AnthropicTool, MessagesRequest};
use crate::error::BridgeError;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::json;
use tracing::info;

const SHELL_TOOL_USE_ID: &str = "toolu_local_shell";

pub(super) async fn try_handle(
    state: &AppState,
    payload: &MessagesRequest,
    model: String,
) -> Result<Option<Response>, BridgeError> {
    if let Some(output) = local_shell_result(&payload.messages) {
        info!(
            length = output.len(),
            "received local shell result from client"
        );
        return Ok(Some(render_shell_result(output, model, payload.stream)));
    }

    let Some(command) = last_user_shell_cmd(&payload.messages) else {
        return Ok(None);
    };

    info!(%command, "delegating local shell command to client");
    state
        .config
        .shell_policy
        .check(&command)
        .map_err(|_| BridgeError::ShellDisabled)?;

    let target = resolve_shell_tool(payload.tools.as_deref());
    Ok(Some(render_shell_request(
        command,
        target,
        model,
        payload.stream,
    )))
}

#[derive(Debug)]
struct ShellToolTarget {
    name: String,
    parameter: String,
}

fn resolve_shell_tool(tools: Option<&[AnthropicTool]>) -> ShellToolTarget {
    let Some(tool) = tools.and_then(|tools| {
        tools.iter().find(|tool| {
            matches!(
                tool.name.to_ascii_lowercase().as_str(),
                "bash" | "execute_command" | "run_command"
            )
        })
    }) else {
        return ShellToolTarget {
            name: "bash".to_string(),
            parameter: "command".to_string(),
        };
    };

    let parameter = tool
        .input_schema
        .get("properties")
        .and_then(|value| value.as_object())
        .and_then(|properties| {
            if properties.contains_key("command") {
                Some("command".to_string())
            } else if properties.contains_key("cmd") {
                Some("cmd".to_string())
            } else {
                properties.keys().next().cloned()
            }
        })
        .unwrap_or_else(|| "command".to_string());

    ShellToolTarget {
        name: tool.name.clone(),
        parameter,
    }
}

fn render_shell_result(output: String, model: String, stream: bool) -> Response {
    let output_tokens = estimate_output_tokens(&output, 10);
    let builder = SseEventBuilder::new("msg_local_shell_result".to_string(), model);

    if !stream {
        return Json(builder.non_streaming_response(&output, 10, output_tokens)).into_response();
    }

    let (tx, rx) = tokio::sync::mpsc::channel(10);
    tokio::spawn(async move {
        for event in [
            builder.message_start(10),
            builder.content_block_start(),
            builder.text_delta(&output),
            builder.content_block_stop(),
            builder.message_delta(output_tokens),
            builder.message_stop(),
        ] {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    sse_response(rx)
}

fn render_shell_request(
    command: String,
    target: ShellToolTarget,
    model: String,
    stream: bool,
) -> Response {
    let output_tokens = estimate_output_tokens(&command, 15);
    let input = json!({ target.parameter.clone(): command.clone() });

    if !stream {
        return Json(json!({
            "id": "msg_local_shell",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{
                "type": "tool_use",
                "id": SHELL_TOOL_USE_ID,
                "name": target.name,
                "input": input
            }],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {"input_tokens": 50, "output_tokens": output_tokens}
        }))
        .into_response();
    }

    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let builder = SseEventBuilder::new("msg_local_shell".to_string(), model);
    tokio::spawn(async move {
        let events = [
            builder.message_start(50),
            json_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": SHELL_TOOL_USE_ID,
                        "name": target.name,
                        "input": {}
                    }
                }),
            ),
            json_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": input.to_string()
                    }
                }),
            ),
            json_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            json_event(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                    "usage": {"output_tokens": output_tokens}
                }),
            ),
            json_event("message_stop", json!({"type": "message_stop"})),
        ];

        for event in events {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    sse_response(rx)
}

fn json_event(name: &'static str, payload: serde_json::Value) -> Event {
    Event::default()
        .event(name)
        .json_data(payload)
        .unwrap_or_else(|_| Event::default().event(name).data("{}"))
}

fn sse_response(rx: tokio::sync::mpsc::Receiver<Event>) -> Response {
    let response = Sse::new(
        tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, std::convert::Infallible>),
    )
    .keep_alive(KeepAlive::default())
    .into_response();
    super::messages::disable_proxy_buffering(response)
}

fn estimate_output_tokens(text: &str, overhead: u32) -> u32 {
    (text.len() as f32 / 3.5).round() as u32 + overhead
}
