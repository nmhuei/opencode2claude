//! Mapping functions between Anthropic Messages API and OpenAI Chat Completions API.
//!
//! Converts Anthropic-style requests (with system prompts, content blocks, tool use,
//! tool results) into the OpenAI-compatible format used by the upstream API.

use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::types::*;
use std::collections::HashMap;

pub fn extract_system_prompt(system_val: &serde_json::Value) -> String {
    if let Some(s) = system_val.as_str() {
        return s.to_string();
    }
    if let Some(arr) = system_val.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(obj) = item.as_object() {
                if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                        parts.push(text);
                    }
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

pub fn is_web_search_tool(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower == "websearch"
        || name_lower == "web_search"
        || name_lower == "webfetch"
        || name_lower == "web_fetch"
}

/// Extract the search query from tool call arguments.
///
/// Parses the JSON tool arguments and looks for common query fields:
/// "query" or "q", falling back to the first string field found.
pub fn extract_search_query(tool_args: &str) -> String {
    let input_val: serde_json::Value = serde_json::from_str(tool_args)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    if let Some(obj) = input_val.as_object() {
        if let Some(q_val) = obj.get("query").and_then(|v| v.as_str()) {
            return q_val.to_string();
        }
        if let Some(q_val) = obj.get("q").and_then(|v| v.as_str()) {
            return q_val.to_string();
        }
        for (_, v) in obj {
            if let Some(s) = v.as_str() {
                return s.to_string();
            }
        }
    }
    String::new()
}

pub fn map_model_name(model: &str) -> String {
    let mut name = model.to_string();
    if name.starts_with("opencode/") {
        name = name["opencode/".len()..].to_string();
    }
    match name.as_str() {
        "deepseek-v4-flash" => "deepseek-v4-flash-free".to_string(),
        "nemotron-3-ultra" => "nemotron-3-ultra-free".to_string(),
        _ => name,
    }
}

pub fn tool_result_content_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let mut text_parts = Vec::new();
            for item in arr {
                if let Some(obj) = item.as_object() {
                    if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    } else {
                        text_parts.push(item.to_string());
                    }
                } else {
                    text_parts.push(item.to_string());
                }
            }
            text_parts.join("\n")
        }
        _ => val.to_string(),
    }
}

/// Map Anthropic request values into standard OpenAI payload.
pub fn map_anthropic_to_openai(payload: &MessagesRequest, model: String) -> OpenAiRequest {
    let mapped_model = map_model_name(&model);
    let mut openai_messages = Vec::new();

    // Build a map of tool_use_id -> name from previous assistant messages
    let mut tool_name_map = HashMap::new();
    for msg in &payload.messages {
        if msg.role == "assistant" {
            if let ContentVal::Multiple(blocks) = &msg.content {
                for block in blocks {
                    if block.content_type == "tool_use" {
                        if let (Some(id), Some(name)) = (&block.id, &block.name) {
                            tool_name_map.insert(id.clone(), name.clone());
                        }
                    }
                }
            }
        }
    }

    // 1. System Prompt
    if let Some(system_val) = &payload.system {
        let system = extract_system_prompt(system_val);
        if !system.is_empty() {
            openai_messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: Some(system),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // 2. Messages conversation turns
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => {
                openai_messages.push(OpenAiMessage {
                    role: msg.role.clone(),
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            ContentVal::Multiple(blocks) => {
                if msg.role == "user" {
                    let mut user_text = String::new();
                    for block in blocks {
                        match block.content_type.as_str() {
                            "text" => {
                                if let Some(t) = &block.text {
                                    if !user_text.is_empty() {
                                        user_text.push('\n');
                                    }
                                    user_text.push_str(t);
                                }
                            }
                            "tool_result" => {
                                let name = block.name.clone().or_else(|| {
                                    block
                                        .tool_use_id
                                        .as_ref()
                                        .and_then(|id| tool_name_map.get(id).cloned())
                                });
                                openai_messages.push(OpenAiMessage {
                                    role: "tool".to_string(),
                                    content: block
                                        .content
                                        .as_ref()
                                        .map(tool_result_content_to_string),
                                    tool_calls: None,
                                    tool_call_id: block.tool_use_id.clone(),
                                    name,
                                });
                            }
                            _ => {}
                        }
                    }
                    if !user_text.is_empty() {
                        openai_messages.push(OpenAiMessage {
                            role: "user".to_string(),
                            content: Some(user_text),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                    }
                } else if msg.role == "assistant" {
                    let mut assistant_text = String::new();
                    let mut tool_calls = Vec::new();
                    for block in blocks {
                        match block.content_type.as_str() {
                            "text" => {
                                if let Some(t) = &block.text {
                                    if !assistant_text.is_empty() {
                                        assistant_text.push('\n');
                                    }
                                    assistant_text.push_str(t);
                                }
                            }
                            "tool_use" => {
                                if let (Some(id), Some(name), Some(input)) =
                                    (&block.id, &block.name, &block.input)
                                {
                                    tool_calls.push(OpenAiToolCall {
                                        id: id.clone(),
                                        tool_type: "function".to_string(),
                                        function: OpenAiFunctionCall {
                                            name: name.clone(),
                                            arguments: serde_json::to_string(input)
                                                .unwrap_or_default(),
                                        },
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    openai_messages.push(OpenAiMessage {
                        role: "assistant".to_string(),
                        content: if assistant_text.is_empty() {
                            None
                        } else {
                            Some(assistant_text)
                        },
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
        }
    }

    // 3. Tools mapping
    let tools = payload.tools.as_ref().map(|t_list| {
        t_list
            .iter()
            .map(|t| OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect()
    });

    // 4. Tool Choice mapping
    let tool_choice = payload.tool_choice.as_ref().map(|tc| {
        if let Some(tc_str) = tc.as_str() {
            serde_json::Value::String(tc_str.to_string())
        } else if let Some(tc_obj) = tc.as_object() {
            if let Some(t_type) = tc_obj.get("type").and_then(|t| t.as_str()) {
                match t_type {
                    "auto" => serde_json::Value::String("auto".to_string()),
                    "any" => serde_json::Value::String("required".to_string()),
                    "tool" => {
                        if let Some(t_name) = tc_obj.get("name").and_then(|n| n.as_str()) {
                            serde_json::json!({
                                "type": "function",
                                "function": { "name": t_name }
                            })
                        } else {
                            serde_json::Value::String("auto".to_string())
                        }
                    }
                    _ => serde_json::Value::String("auto".to_string()),
                }
            } else {
                serde_json::Value::String("auto".to_string())
            }
        } else {
            serde_json::Value::String("auto".to_string())
        }
    });

    OpenAiRequest {
        model: mapped_model,
        messages: openai_messages,
        tools,
        tool_choice,
        stream: payload.stream,
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::{AnthropicTool, ContentVal, Message, MessageContent, MessagesRequest};

    #[test]
    fn test_is_web_search_tool() {
        assert!(is_web_search_tool("web_search"));
        assert!(is_web_search_tool("websearch"));
        assert!(is_web_search_tool("web_fetch"));
        assert!(is_web_search_tool("webfetch"));
        assert!(!is_web_search_tool("google_search"));
        assert!(!is_web_search_tool("some_other_tool"));
    }

    #[test]
    fn test_extract_search_query() {
        assert_eq!(
            extract_search_query(r#"{"query": "test query"}"#),
            "test query"
        );
        assert_eq!(
            extract_search_query(r#"{"q": "short query"}"#),
            "short query"
        );
        assert_eq!(
            extract_search_query(r#"{"other": "fallback"}"#),
            "fallback"
        );
        assert_eq!(extract_search_query(r#"{}"#), "");
        assert_eq!(extract_search_query(r#"invalid json"#), "");
    }

    #[test]
    fn test_tool_result_content_to_string() {
        // String variant
        let val_str = serde_json::Value::String("hello world".to_string());
        assert_eq!(tool_result_content_to_string(&val_str), "hello world");

        // Object array variant
        let val_arr = serde_json::json!([
            { "type": "text", "text": "line 1" },
            { "type": "text", "text": "line 2" }
        ]);
        assert_eq!(tool_result_content_to_string(&val_arr), "line 1\nline 2");

        // Non-standard array format
        let val_arr_fallback = serde_json::json!(["hello", 123]);
        assert_eq!(
            tool_result_content_to_string(&val_arr_fallback),
            "\"hello\"\n123"
        );

        // Number/Object fallback
        let val_num = serde_json::Value::Number(42.into());
        assert_eq!(tool_result_content_to_string(&val_num), "42");
    }

    #[test]
    fn test_map_anthropic_to_openai_plain() {
        let payload = MessagesRequest {
            model: Some("claude-3-5-sonnet".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single("hello".to_string()),
            }],
            system: Some(serde_json::json!("you are a helpful assistant")),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(1024),
        };

        let result = map_anthropic_to_openai(&payload, "claude-3-5-sonnet".to_string());
        assert_eq!(result.model, "claude-3-5-sonnet");
        assert_eq!(result.messages.len(), 2); // 1 system + 1 user
        assert_eq!(result.messages[0].role, "system");
        assert_eq!(
            result.messages[0].content.as_deref(),
            Some("you are a helpful assistant")
        );
        assert_eq!(result.messages[1].role, "user");
        assert_eq!(result.messages[1].content.as_deref(), Some("hello"));
    }

    #[test]
    fn test_map_anthropic_to_openai_tools_and_results() {
        let payload = MessagesRequest {
            model: None,
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: ContentVal::Single("run command".to_string()),
                },
                Message {
                    role: "assistant".to_string(),
                    content: ContentVal::Multiple(vec![
                        MessageContent {
                            content_type: "text".to_string(),
                            text: Some("Okay, running bash command...".to_string()),
                            ..Default::default()
                        },
                        MessageContent {
                            content_type: "tool_use".to_string(),
                            id: Some("call_123".to_string()),
                            name: Some("bash".to_string()),
                            input: Some(serde_json::json!({ "command": "echo test" })),
                            ..Default::default()
                        },
                    ]),
                },
                Message {
                    role: "user".to_string(),
                    content: ContentVal::Multiple(vec![MessageContent {
                        content_type: "tool_result".to_string(),
                        tool_use_id: Some("call_123".to_string()),
                        content: Some(serde_json::json!([
                            { "type": "text", "text": "test output" }
                        ])),
                        ..Default::default()
                    }]),
                },
            ],
            system: None,
            tools: Some(vec![AnthropicTool {
                name: "bash".to_string(),
                description: "run a command".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
            }]),
            tool_choice: Some(serde_json::json!({ "type": "any" })),
            stream: true,
            temperature: None,
            max_tokens: None,
        };

        let result = map_anthropic_to_openai(&payload, "deepseek-v4-flash".to_string());
        assert_eq!(result.model, "deepseek-v4-flash-free"); // Mapped model name
        assert_eq!(result.messages.len(), 3);

        // First user message
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content.as_deref(), Some("run command"));

        // Assistant message with tool_calls
        assert_eq!(result.messages[1].role, "assistant");
        assert_eq!(
            result.messages[1].content.as_deref(),
            Some("Okay, running bash command...")
        );
        let tc = result.messages[1].tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_123");
        assert_eq!(tc[0].function.name, "bash");
        assert_eq!(tc[0].function.arguments, "{\"command\":\"echo test\"}");

        // Tool result message mapped to OpenAI's tool role
        assert_eq!(result.messages[2].role, "tool");
        assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(result.messages[2].content.as_deref(), Some("test output"));
        // Name should be retrieved from history
        assert_eq!(result.messages[2].name.as_deref(), Some("bash"));

        // Verify tools and tool_choice mappings
        let res_tools = result.tools.unwrap();
        assert_eq!(res_tools.len(), 1);
        assert_eq!(res_tools[0].function.name, "bash");
        assert_eq!(
            result.tool_choice,
            Some(serde_json::Value::String("required".to_string()))
        );
    }

    #[test]
    fn test_map_model_name() {
        assert_eq!(map_model_name("deepseek-v4-flash"), "deepseek-v4-flash-free");
        assert_eq!(map_model_name("gpt-4"), "gpt-4");
        assert_eq!(map_model_name("opencode/gpt-4"), "gpt-4");
    }
}
