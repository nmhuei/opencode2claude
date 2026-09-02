//! Heuristic token estimation for usage accounting.

use crate::handlers::{ContentVal, MessagesRequest};

pub fn estimate_string_tokens(text: &str) -> u32 {
    let mut tokens: f32 = 0.0;
    let mut in_word = false;

    for c in text.chars() {
        if c.is_whitespace() {
            tokens += 0.25;
            in_word = false;
        } else if c.is_ascii_alphanumeric() {
            if !in_word {
                tokens += 1.0;
                in_word = true;
            } else {
                tokens += 0.22;
            }
        } else {
            tokens += 0.5;
            in_word = false;
        }
    }
    tokens.round() as u32
}

pub fn estimate_input_tokens(payload: &MessagesRequest) -> u32 {
    let mut total_tokens = 0;
    if let Some(ref sys) = payload.system {
        total_tokens += estimate_string_tokens(&sys.to_string());
    }
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => total_tokens += estimate_string_tokens(text),
            ContentVal::Multiple(blocks) => {
                for b in blocks {
                    if let Some(ref text) = b.text {
                        total_tokens += estimate_string_tokens(text);
                    }
                    if let Some(ref input) = b.input {
                        total_tokens += estimate_string_tokens(&input.to_string());
                    }
                    if let Some(ref content) = b.content {
                        total_tokens += estimate_string_tokens(&content.to_string());
                    }
                }
            }
        }
    }
    if total_tokens == 0 {
        100
    } else {
        total_tokens
    }
}

/// Estimate tokens for an OpenAI-compatible messages JSON value. Walking the
/// complete value intentionally includes tool calls, tool results, and nested
/// arguments, which the older typed Anthropic helper cannot see.
pub fn estimate_provider_request_tokens(messages: &serde_json::Value) -> u64 {
    let encoded = serde_json::to_string(messages).unwrap_or_default();
    let estimate = estimate_string_tokens(&encoded) as u64;
    if estimate == 0 {
        100
    } else {
        estimate
    }
}

#[cfg(test)]
mod tests {
    use super::estimate_provider_request_tokens;

    #[test]
    fn provider_estimator_includes_nested_tool_arguments() {
        let plain = estimate_provider_request_tokens(&serde_json::json!([
            {"role": "user", "content": "hello"}
        ]));
        let with_tool = estimate_provider_request_tokens(&serde_json::json!([
            {"role": "user", "content": "hello", "tool_calls": [{"arguments": {"path": "/very/long/path"}}]}
        ]));
        assert!(with_tool > plain);
    }
}
