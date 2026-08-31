//! OpenAI response capture: history collection from sync and streaming SSE bodies.

use crate::history::HistoryCapture;
use axum::http::StatusCode;
use bytes::Bytes;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct OpenAiResponseCollector {
    capture: HistoryCapture,
    is_stream: bool,
    status: StatusCode,
    max_bytes: usize,
    buffer: Vec<u8>,
    finished: bool,
    first_chunk: bool,
}

#[derive(Default)]
struct ParsedOpenAiHistory {
    model: Option<String>,
    finish_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}

impl OpenAiResponseCollector {
    pub(super) fn new(
        capture: HistoryCapture,
        is_stream: bool,
        status: StatusCode,
        max_bytes: usize,
    ) -> Self {
        Self {
            capture,
            is_stream,
            status,
            max_bytes: max_bytes.max(1024),
            buffer: Vec::new(),
            finished: false,
            first_chunk: false,
        }
    }

    pub(super) fn push(&mut self, chunk: &Bytes) {
        if !self.first_chunk {
            self.capture.first_chunk();
            self.first_chunk = true;
        }
        if self.buffer.len() >= self.max_bytes {
            return;
        }
        let remaining = self.max_bytes - self.buffer.len();
        self.buffer
            .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }

    pub(super) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let parsed = self.parse_buffer();
        self.capture.usage(
            parsed.input_tokens,
            parsed.output_tokens,
            parsed.reasoning_tokens,
        );
        self.capture.response_model(parsed.model.as_deref());
        if self.status.is_success() {
            self.capture.attempt_finished(
                Some(self.status.as_u16()),
                "completed",
                parsed.finish_reason.as_deref(),
                None,
                None,
            );
            self.capture.finish_success(
                self.status.as_u16(),
                parsed.finish_reason.as_deref(),
                parsed.model.as_deref(),
            );
        } else {
            self.capture.attempt_finished(
                Some(self.status.as_u16()),
                "failed",
                parsed.finish_reason.as_deref(),
                Some("upstream_non_2xx"),
                Some(&format!("upstream returned status {}", self.status)),
            );
            self.capture.fail(
                Some(self.status.as_u16()),
                "upstream_non_2xx",
                &format!("upstream returned status {}", self.status),
            );
        }
    }

    pub(super) fn fail(&mut self, error_type: &str, message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = self.parse_buffer();
        self.capture.attempt_finished(
            Some(self.status.as_u16()),
            "failed",
            None,
            Some(error_type),
            Some(message),
        );
        self.capture
            .fail(Some(self.status.as_u16()), error_type, message);
    }

    fn parse_buffer(&self) -> ParsedOpenAiHistory {
        let raw = String::from_utf8_lossy(&self.buffer);
        self.capture.provider_raw_response(&raw);
        if self.is_stream {
            parse_openai_sse_history(&raw, &self.capture)
        } else {
            parse_openai_sync_history(&self.buffer, &self.capture)
        }
    }
}

impl Drop for OpenAiResponseCollector {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.parse_buffer();
        self.capture.attempt_finished(
            Some(self.status.as_u16()),
            "cancelled",
            None,
            Some("client_cancelled"),
            Some("client stopped reading the OpenAI response stream"),
        );
        self.capture.cancel();
        self.finished = true;
    }
}

fn parse_openai_sync_history(body: &[u8], capture: &HistoryCapture) -> ParsedOpenAiHistory {
    let Ok(root) = serde_json::from_slice::<Value>(body) else {
        return ParsedOpenAiHistory::default();
    };
    let mut parsed = ParsedOpenAiHistory {
        model: root
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        ..Default::default()
    };
    if let Some(usage) = root.get("usage") {
        parsed.input_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64);
        parsed.output_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64);
        parsed.reasoning_tokens = usage
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .or_else(|| usage.get("reasoning_tokens"))
            .and_then(Value::as_u64);
    }
    if let Some(choice) = root
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        parsed.finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if let Some(message) = choice.get("message") {
            if let Some(reasoning) = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .or_else(|| message.get("thinking"))
                .and_then(Value::as_str)
            {
                capture.append_reasoning(reasoning);
            }
            if let Some(content) = message.get("content") {
                append_openai_content(content, capture);
            }
            capture_openai_tool_calls(message.get("tool_calls"), capture);
        }
    }
    parsed
}

fn parse_openai_sse_history(raw: &str, capture: &HistoryCapture) -> ParsedOpenAiHistory {
    let mut parsed = ParsedOpenAiHistory::default();
    let mut tool_calls = BTreeMap::<usize, (String, String)>::new();
    for line in raw.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if parsed.model.is_none() {
            parsed.model = event
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        if let Some(usage) = event.get("usage") {
            parsed.input_tokens = usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .or(parsed.input_tokens);
            parsed.output_tokens = usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                .or(parsed.output_tokens);
            parsed.reasoning_tokens = usage
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .or_else(|| usage.get("reasoning_tokens"))
                .and_then(Value::as_u64)
                .or(parsed.reasoning_tokens);
        }
        for choice in event
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                parsed.finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .or_else(|| delta.get("thinking"))
                .and_then(Value::as_str)
            {
                capture.append_reasoning(reasoning);
            }
            if let Some(content) = delta.get("content") {
                append_openai_content(content, capture);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let entry = tool_calls.entry(index).or_default();
                    if let Some(function) = call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            entry.0.push_str(name);
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            entry.1.push_str(arguments);
                        }
                    }
                }
            }
        }
    }
    for (_, (name, arguments)) in tool_calls {
        capture.tool_call(
            if name.is_empty() { "tool_call" } else { &name },
            (!arguments.is_empty()).then_some(arguments.as_str()),
        );
    }
    parsed
}

fn append_openai_content(content: &Value, capture: &HistoryCapture) {
    match content {
        Value::String(text) => capture.append_response(text),
        Value::Array(items) => {
            for item in items {
                if let Some(text) = item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
                {
                    capture.append_response(text);
                }
            }
        }
        Value::Null => {}
        other => capture.append_response(&other.to_string()),
    }
}

fn capture_openai_tool_calls(calls: Option<&Value>, capture: &HistoryCapture) {
    let Some(calls) = calls.and_then(Value::as_array) else {
        return;
    };
    for call in calls {
        let function = call.get("function").unwrap_or(call);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool_call");
        let arguments = function.get("arguments").and_then(Value::as_str);
        capture.tool_call(name, arguments);
    }
}
