use crate::opencode::types::{OpenAiInboundRequest, OpenAiRequest};
use serde::Serialize;

pub(crate) trait RetryableOpenAiRequest: Serialize + Clone {
    fn model(&self) -> &str;
    fn stream(&self) -> bool;
    fn set_model(&mut self, model: String);
    fn repair_missing_tool_reasoning(&mut self) -> bool;
    fn disable_reasoning_compatibility(&mut self) -> bool;
    fn strip_response_format(&mut self) -> bool;
}

impl RetryableOpenAiRequest for OpenAiRequest {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self) -> bool {
        self.stream
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn repair_missing_tool_reasoning(&mut self) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            let has_tool_calls = message
                .tool_calls
                .as_ref()
                .is_some_and(|tool_calls| !tool_calls.is_empty());
            let missing_reasoning = message
                .reasoning_content
                .as_deref()
                .is_none_or(str::is_empty);
            if message.role == "assistant" && has_tool_calls && missing_reasoning {
                message.reasoning_content = Some("Tool call continuation.".to_string());
                changed = true;
            }
        }
        changed
    }

    fn disable_reasoning_compatibility(&mut self) -> bool {
        let mut changed = false;
        if self
            .thinking
            .as_ref()
            .is_none_or(|thinking| thinking.thinking_type != "disabled")
        {
            self.thinking = Some(crate::opencode::types::OpenAiThinkingConfig {
                thinking_type: "disabled".to_string(),
            });
            changed = true;
        }
        changed |= self.reasoning_effort.take().is_some();
        if self.include_reasoning != Some(false) {
            self.include_reasoning = Some(false);
            changed = true;
        }
        for message in &mut self.messages {
            changed |= message.reasoning_content.take().is_some();
        }
        changed
    }

    fn strip_response_format(&mut self) -> bool {
        self.response_format.take().is_some()
    }
}

impl RetryableOpenAiRequest for OpenAiInboundRequest {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self) -> bool {
        self.stream
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn repair_missing_tool_reasoning(&mut self) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            let Some(object) = message.as_object_mut() else {
                continue;
            };
            let is_assistant =
                object.get("role").and_then(serde_json::Value::as_str) == Some("assistant");
            let has_tool_calls = object
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|tool_calls| !tool_calls.is_empty());
            let missing_reasoning = object
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty);
            if is_assistant && has_tool_calls && missing_reasoning {
                object.insert(
                    "reasoning_content".to_string(),
                    serde_json::Value::String("Tool call continuation.".to_string()),
                );
                changed = true;
            }
        }
        changed
    }

    fn disable_reasoning_compatibility(&mut self) -> bool {
        let mut changed = false;
        let disabled = serde_json::json!({"type":"disabled"});
        if self.extra.get("thinking") != Some(&disabled) {
            self.extra.insert("thinking".to_string(), disabled);
            changed = true;
        }
        changed |= self.extra.remove("reasoning_effort").is_some();
        changed |= self.extra.remove("include_reasoning").is_some();
        for message in &mut self.messages {
            if let Some(object) = message.as_object_mut() {
                for field in ["reasoning_content", "reasoning", "thinking"] {
                    changed |= object.remove(field).is_some();
                }
            }
        }
        changed
    }

    fn strip_response_format(&mut self) -> bool {
        self.extra.remove("response_format").is_some()
    }
}
