//! Wire types accepted by the Anthropic Messages-compatible endpoints.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct MessageContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Value>,
    /// Preserve newly introduced Claude content-block fields instead of silently dropping them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum ContentVal {
    Single(String),
    Multiple(Vec<MessageContent>),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: ContentVal,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_object")]
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ThinkingConfig {
    pub fn is_enabled(&self) -> Option<bool> {
        match self.thinking_type.as_str() {
            "enabled" | "adaptive" => Some(true),
            "disabled" => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct OutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct MessagesRequest {
    pub model: Option<String>,
    pub messages: Vec<Message>,
    pub system: Option<Value>,
    pub tools: Option<Vec<AnthropicTool>>,
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub stream: bool,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub thinking: Option<ThinkingConfig>,
    pub output_config: Option<OutputConfig>,
    pub context_management: Option<Value>,
    pub metadata: Option<Value>,
    pub service_tier: Option<String>,
    pub stop_sequences: Option<Vec<String>>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub container: Option<Value>,
    pub mcp_servers: Option<Vec<Value>>,
    /// Preserve future Claude Code request features and CLAUDE_CODE_EXTRA_BODY values.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl MessagesRequest {
    pub fn thinking_enabled(&self) -> Option<bool> {
        self.thinking.as_ref().and_then(ThinkingConfig::is_enabled)
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.output_config
            .as_ref()
            .and_then(|config| config.effort.as_deref())
            .or_else(|| self.extra.get("reasoning_effort").and_then(Value::as_str))
    }
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}
