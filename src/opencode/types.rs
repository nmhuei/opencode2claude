//! Types for the OpenAI Chat Completions API format.
//!
//! These structs are used to serialize requests to and deserialize responses from
//! the upstream OpenAI-compatible API endpoint.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAiInboundRequest {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct OpenAiRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<OpenAiThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_reasoning: Option<bool>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct OpenAiThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OpenAiFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAiFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OpenAiFunction,
}

#[derive(Debug, Serialize, Clone)]
pub struct OpenAiFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponse {
    pub id: String,
    pub model: String,
    pub choices: Vec<OpenAiChoice>,
    pub usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiChoice {
    pub message: OpenAiResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponseMessage {
    pub content: Option<String>,
    #[serde(default, alias = "reasoning", alias = "thinking")]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<OpenAiResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponseToolCall {
    pub id: String,
    pub function: OpenAiResponseFunctionCall,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponseFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

// ── Streaming response structures ──

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamChunk {
    #[serde(default)]
    pub choices: Vec<OpenAiStreamChoice>,
    /// Present when the upstream ends the SSE stream with an OpenAI error
    /// payload (`{"error": {...}}`). Must be surfaced to the client as an
    /// Anthropic error event, not dropped.
    #[serde(default)]
    pub error: Option<OpenAiStreamError>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamError {
    pub message: Option<String>,
    #[serde(rename = "type")]
    pub error_type: Option<String>,
    pub code: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamChoice {
    pub delta: OpenAiStreamDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamDelta {
    pub content: Option<String>,
    #[serde(default, alias = "reasoning", alias = "thinking")]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<OpenAiStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamToolCall {
    pub index: usize,
    pub id: Option<String>,
    pub function: Option<OpenAiStreamFunctionCall>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamFunctionCall {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[cfg(test)]
mod golden_tests {
    //! Golden regression tests pinning the exact upstream wire shapes.
    //!
    //! The serialization shape of `OpenAiRequest` and the deserialization
    //! tolerance of the response/stream structs are part of the protected
    //! parse/protocol layer: any accidental field rename, dropped `skip_
    //! serializing_if`, or lost alias breaks Claude Code immediately.

    use super::*;

    #[test]
    fn openai_request_serializes_exact_wire_shape() {
        let request = OpenAiRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                OpenAiMessage {
                    role: "system".to_string(),
                    content: Some("sys".to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                OpenAiMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![OpenAiToolCall {
                        id: "call_1".to_string(),
                        tool_type: "function".to_string(),
                        function: OpenAiFunctionCall {
                            name: "bash".to_string(),
                            arguments: r#"{"command":"ls"}"#.to_string(),
                        },
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                OpenAiMessage {
                    role: "tool".to_string(),
                    content: Some("out".to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: Some("bash".to_string()),
                },
            ],
            tools: Some(vec![OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunction {
                    name: "bash".to_string(),
                    description: "run".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            }]),
            tool_choice: Some(serde_json::json!("required")),
            parallel_tool_calls: Some(false),
            stream: false,
            temperature: Some(0.5),
            top_p: None,
            stop: None,
            max_tokens: Some(128),
            thinking: Some(OpenAiThinkingConfig {
                thinking_type: "enabled".to_string(),
            }),
            reasoning_effort: Some("high".to_string()),
            response_format: None,
            include_reasoning: Some(true),
        };

        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "model": "gpt-4o",
                "messages": [
                    {"role": "system", "content": "sys"},
                    {"role": "assistant", "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
                    }]},
                    {"role": "tool", "content": "out", "tool_call_id": "call_1", "name": "bash"}
                ],
                "tools": [{"type": "function", "function": {
                    "name": "bash", "description": "run", "parameters": {"type": "object"}
                }}],
                "tool_choice": "required",
                "parallel_tool_calls": false,
                "stream": false,
                "temperature": 0.5,
                "max_tokens": 128,
                "thinking": {"type": "enabled"},
                "reasoning_effort": "high",
                "include_reasoning": true
            }),
            "upstream request wire shape changed; Claude Code depends on it"
        );

        // `None` fields must be omitted entirely, never serialized as null.
        let serialized = serde_json::to_string(&request).unwrap();
        for absent in ["top_p", "\"stop\"", "response_format", "reasoning_content"] {
            assert!(
                !serialized.contains(absent),
                "unexpected {absent} in {serialized}"
            );
        }
    }

    #[test]
    fn response_message_reads_provider_reasoning_aliases() {
        for alias in ["reasoning_content", "reasoning", "thinking"] {
            let payload = format!(r#"{{"content": "t", "{alias}": "why"}}"#);
            let message: OpenAiResponseMessage = serde_json::from_str(&payload).unwrap();
            assert_eq!(message.content.as_deref(), Some("t"), "alias {alias}");
            assert_eq!(
                message.reasoning_content.as_deref(),
                Some("why"),
                "alias {alias}"
            );
        }

        // Missing optional fields degrade to None instead of failing the parse;
        // strict providers omit them routinely.
        let message: OpenAiResponseMessage = serde_json::from_str("{}").unwrap();
        assert_eq!(message.content, None);
        assert_eq!(message.reasoning_content, None);
        assert!(message.tool_calls.is_none());
    }

    #[test]
    fn sync_usage_parses_both_token_counts() {
        let usage: OpenAiUsage =
            serde_json::from_str(r#"{"prompt_tokens": 11, "completion_tokens": 7}"#).unwrap();
        assert_eq!(usage.prompt_tokens, 11);
        assert_eq!(usage.completion_tokens, 7);
    }

    #[test]
    fn stream_chunk_parses_error_payload_without_choices() {
        let chunk: OpenAiStreamChunk = serde_json::from_str(
            r#"{"error": {"message": "upstream boom", "type": "server_error", "code": 502}}"#,
        )
        .unwrap();
        assert!(chunk.choices.is_empty());
        let error = chunk.error.expect("error payload must deserialize");
        assert_eq!(error.message.as_deref(), Some("upstream boom"));
        assert_eq!(error.error_type.as_deref(), Some("server_error"));
        assert_eq!(error.code, Some(serde_json::json!(502)));
    }

    #[test]
    fn stream_delta_defaults_and_reasoning_alias() {
        let chunk: OpenAiStreamChunk =
            serde_json::from_str(r#"{"choices": [{"delta": {}, "finish_reason": null}]}"#).unwrap();
        assert!(chunk.error.is_none());
        let delta = &chunk.choices[0].delta;
        assert_eq!(delta.content, None);
        assert_eq!(delta.reasoning_content, None);
        assert!(delta.tool_calls.is_none());

        let aliased: OpenAiStreamChunk = serde_json::from_str(
            r#"{"choices": [{"delta": {"thinking": "plan"}, "finish_reason": "stop"}]}"#,
        )
        .unwrap();
        assert_eq!(
            aliased.choices[0].delta.reasoning_content.as_deref(),
            Some("plan")
        );
        assert_eq!(aliased.choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn stream_tool_call_delta_accepts_partial_fields() {
        let chunk: OpenAiStreamChunk = serde_json::from_str(
            r#"{"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "bash"}}
            ]}, "finish_reason": null}]}"#,
        )
        .unwrap();
        let call = &chunk.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.index, 0);
        assert_eq!(call.id.as_deref(), Some("call_1"));
        assert_eq!(
            call.function
                .as_ref()
                .and_then(|function| function.name.as_deref()),
            Some("bash")
        );
        assert_eq!(
            call.function
                .as_ref()
                .and_then(|function| function.arguments.as_deref()),
            None
        );
    }
}
