use super::policy::DEFAULT_MIN_REASONING_STREAM_TOKENS;
use super::*;
use crate::handlers::{AnthropicTool, ContentVal, Message, MessageContent, MessagesRequest};

#[test]
fn test_streaming_reasoning_model_raises_low_max_tokens() {
    let payload = MessagesRequest {
        model: Some("opencode/deepseek-r1-free".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single("hi".to_string()),
        }],
        system: None,
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: Some(32),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-r1-free".to_string());
    assert_eq!(mapped.max_tokens, Some(DEFAULT_MIN_REASONING_STREAM_TOKENS));
    assert_eq!(mapped.include_reasoning, Some(true));
}

#[test]
fn test_non_streaming_preserves_low_max_tokens() {
    let payload = MessagesRequest {
        model: Some("opencode/deepseek-r1-free".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single("hi".to_string()),
        }],
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: None,
        max_tokens: Some(32),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-r1-free".to_string());
    assert_eq!(mapped.max_tokens, Some(32));
    assert_eq!(mapped.include_reasoning, None);
}

#[test]
fn test_is_web_search_tool() {
    assert!(is_web_search_tool("web_search"));
    assert!(is_web_search_tool("websearch"));
    assert!(is_web_search_tool("web_fetch"));
    assert!(is_web_search_tool("webfetch"));
    assert!(!is_web_search_tool("google_search"));
    assert!(!is_web_search_tool("some_other_tool"));

    assert!(is_bridge_search_tool("WebSearch"));
    assert!(is_bridge_search_tool("web_search"));
    assert!(!is_bridge_search_tool("WebFetch"));
    assert!(!is_bridge_search_tool("web_fetch"));
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
    assert_eq!(extract_search_query(r#"{"other": "fallback"}"#), "fallback");
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
        ..Default::default()
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
            ..Default::default()
        }]),
        tool_choice: Some(serde_json::json!({ "type": "any" })),
        stream: true,
        temperature: None,
        max_tokens: None,
        ..Default::default()
    };

    let result = map_anthropic_to_openai(&payload, "deepseek-chat".to_string());
    assert_eq!(result.model, "deepseek-chat"); // Mapped model name
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
    assert_eq!(
        map_model_name("deepseek-v4-flash"),
        "deepseek-v4-flash-free"
    );
    assert_eq!(map_model_name("gpt-4"), "gpt-4");
    assert_eq!(map_model_name("opencode/gpt-4"), "gpt-4");
}

#[test]
fn test_map_model_name_free_mapping() {
    assert_eq!(map_model_name("nemotron-3-ultra"), "nemotron-3-ultra-free");
}

#[test]
fn test_extract_system_prompt_string() {
    let val = serde_json::json!("you are a helpful assistant");
    assert_eq!(extract_system_prompt(&val), "you are a helpful assistant");
}

#[test]
fn test_extract_system_prompt_array() {
    let val = serde_json::json!([
        {"type": "text", "text": "Be concise."},
        {"type": "text", "text": "Use markdown."}
    ]);
    assert_eq!(extract_system_prompt(&val), "Be concise.\nUse markdown.");
}

#[test]
fn test_extract_system_prompt_mixed() {
    let val = serde_json::json!([
        {"type": "text", "text": "Hello"},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}},
        {"type": "text", "text": "World"}
    ]);
    assert_eq!(extract_system_prompt(&val), "Hello\nWorld");
}

#[test]
fn test_extract_system_prompt_empty() {
    let val = serde_json::json!("");
    assert_eq!(extract_system_prompt(&val), "");
}

#[test]
fn test_extract_system_prompt_null() {
    let val = serde_json::json!(null);
    assert_eq!(extract_system_prompt(&val), "");
}

#[test]
fn free_model_tool_history_uses_native_openai_messages() {
    let payload = MessagesRequest {
        model: Some("claude-sonnet-5".to_string()),
        messages: vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Single("run command".to_string()),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "tool_use".to_string(),
                    id: Some("call_123".to_string()),
                    name: Some("Bash".to_string()),
                    input: Some(serde_json::json!({ "command": "printf TOOL_XML_OK" })),
                    ..Default::default()
                }]),
            },
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "tool_result".to_string(),
                    tool_use_id: Some("call_123".to_string()),
                    content: Some(serde_json::json!("TOOL_XML_OK")),
                    ..Default::default()
                }]),
            },
        ],
        system: None,
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: None,
        ..Default::default()
    };

    let result = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());

    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[1].role, "assistant");
    assert!(result.messages[1].content.is_none());
    let calls = result.messages[1]
        .tool_calls
        .as_ref()
        .expect("assistant native tool_calls missing");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_123");
    assert_eq!(calls[0].function.name, "Bash");
    assert!(calls[0].function.arguments.contains("TOOL_XML_OK"));
    assert_eq!(result.messages[2].role, "tool");
    assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_123"));
    assert_eq!(result.messages[2].name.as_deref(), Some("Bash"));
    assert_eq!(result.messages[2].content.as_deref(), Some("TOOL_XML_OK"));
}

#[test]
fn free_model_preserves_multiple_native_tool_results_in_conversation_order() {
    let payload = MessagesRequest {
        model: Some("claude-sonnet-5".to_string()),
        messages: vec![
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![
                    MessageContent {
                        content_type: "tool_use".to_string(),
                        id: Some("call_a".to_string()),
                        name: Some("Read".to_string()),
                        input: Some(serde_json::json!({"file_path": "a.txt"})),
                        ..Default::default()
                    },
                    MessageContent {
                        content_type: "tool_use".to_string(),
                        id: Some("call_b".to_string()),
                        name: Some("Read".to_string()),
                        input: Some(serde_json::json!({"file_path": "b.txt"})),
                        ..Default::default()
                    },
                ]),
            },
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![
                    MessageContent {
                        content_type: "tool_result".to_string(),
                        tool_use_id: Some("call_a".to_string()),
                        content: Some(serde_json::json!("A")),
                        ..Default::default()
                    },
                    MessageContent {
                        content_type: "tool_result".to_string(),
                        tool_use_id: Some("call_b".to_string()),
                        content: Some(serde_json::json!("B")),
                        ..Default::default()
                    },
                ]),
            },
        ],
        ..Default::default()
    };

    let result = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());
    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role, "assistant");
    let calls = result.messages[0]
        .tool_calls
        .as_ref()
        .expect("native tool calls missing");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert!(calls[0].function.arguments.contains("a.txt"));
    assert_eq!(calls[1].id, "call_b");
    assert!(calls[1].function.arguments.contains("b.txt"));
    assert_eq!(result.messages[1].role, "tool");
    assert_eq!(result.messages[1].tool_call_id.as_deref(), Some("call_a"));
    assert_eq!(result.messages[1].content.as_deref(), Some("A"));
    assert_eq!(result.messages[2].role, "tool");
    assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_b"));
    assert_eq!(result.messages[2].content.as_deref(), Some("B"));
}

#[test]
fn deepseek_v4_free_streaming_thinking_adds_reasoning_hygiene() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{"role": "user", "content": "continue the workflow"}],
        "stream": true,
        "thinking": {"type": "enabled", "budget_tokens": 4096}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    let system = mapped
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .unwrap_or_default();

    assert!(system.contains("never restart, restate, or repeat the same plan"));
    assert!(system.contains("perform it immediately"));
}

#[test]
fn deepseek_v4_free_without_streaming_thinking_skips_reasoning_hygiene() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{"role": "user", "content": "fast answer"}],
        "stream": true,
        "thinking": {"type": "disabled"}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    assert!(mapped.messages.iter().all(|message| {
        message
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("never restart, restate, or repeat"))
    }));
}

#[test]
fn maps_claude_adaptive_thinking_and_max_effort_for_deepseek_v4() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "solve"}],
        "stream": true,
        "temperature": 0.7,
        "top_p": 0.9,
        "max_tokens": 128000,
        "tool_choice": {"type": "auto"},
        "thinking": {"type": "adaptive", "display": "omitted"},
        "output_config": {"effort": "max"}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());

    assert_eq!(
        mapped
            .thinking
            .as_ref()
            .map(|value| value.thinking_type.as_str()),
        Some("enabled")
    );
    assert_eq!(mapped.reasoning_effort.as_deref(), Some("max"));
    assert_eq!(mapped.include_reasoning, Some(true));
    assert!(mapped.temperature.is_none());
    assert!(mapped.top_p.is_none());
    assert!(mapped.tool_choice.is_none());
}

#[test]
fn maps_absent_claude_thinking_to_disabled_for_deepseek_v4() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{"role": "user", "content": "fast answer"}],
        "stream": true,
        "temperature": 0.2,
        "output_config": {"effort": "max"}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());

    assert_eq!(
        mapped
            .thinking
            .as_ref()
            .map(|value| value.thinking_type.as_str()),
        Some("disabled")
    );
    assert!(mapped.reasoning_effort.is_none());
    assert_eq!(mapped.include_reasoning, Some(false));
    assert_eq!(mapped.temperature, Some(0.2));
}

#[test]
fn normalizes_claude_effort_levels_for_deepseek_v4() {
    for (claude_effort, deepseek_effort) in [
        ("low", "high"),
        ("medium", "high"),
        ("high", "high"),
        ("xhigh", "max"),
        ("max", "max"),
    ] {
        let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
            "messages": [{"role": "user", "content": "solve"}],
            "stream": true,
            "thinking": {"type": "enabled", "budget_tokens": 64000},
            "output_config": {"effort": claude_effort}
        }))
        .unwrap();
        let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-pro".to_string());
        assert_eq!(mapped.reasoning_effort.as_deref(), Some(deepseek_effort));
    }
}

#[test]
fn preserves_claude_thinking_history_as_reasoning_content() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "tool reasoning", "signature": "sig"},
                {"type": "text", "text": "calling tool"},
                {"type": "tool_use", "id": "call_1", "name": "Read", "input": {"path": "a"}}
            ]
        }],
        "thinking": {"type": "enabled", "budget_tokens": 64000},
        "output_config": {"effort": "high"}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-pro".to_string());
    assert_eq!(
        mapped.messages[0].reasoning_content.as_deref(),
        Some("tool reasoning")
    );
    assert_eq!(mapped.messages[0].content.as_deref(), Some("calling tool"));
    assert_eq!(
        mapped.messages[0].tool_calls.as_ref().map(Vec::len),
        Some(1)
    );
}

#[test]
fn maps_claude_structured_output_for_deepseek_json_mode() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{"role": "user", "content": "return json"}],
        "thinking": {"type": "disabled"},
        "output_config": {
            "effort": "high",
            "format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {"title": {"type": "string"}},
                    "required": ["title"]
                }
            }
        }
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());
    assert_eq!(
        mapped.response_format,
        Some(serde_json::json!({"type": "json_object"}))
    );
    assert_eq!(
        mapped
            .thinking
            .as_ref()
            .map(|value| value.thinking_type.as_str()),
        Some("disabled")
    );
}

#[test]
fn extracts_search_query_from_nested_and_non_json_arguments() {
    assert_eq!(
        extract_search_query(r#"{"input":{"search_query":"Claude Code security skills"}}"#),
        "Claude Code security skills"
    );
    assert_eq!(
        extract_search_query(r#"{"queries":[{"text":"MCP security servers"}]}"#),
        "MCP security servers"
    );
    assert_eq!(extract_search_query("{}"), "");
}

#[test]
fn explicit_fanout_prompt_requires_two_agent_calls() {
    let payload = MessagesRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "fan sub agent search security skills for Claude Code".to_string(),
            ),
        }],
        tools: Some(vec![AnthropicTool {
            name: "Agent".to_string(),
            description: "launch an agent".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    let system = mapped.messages[0].content.as_deref().unwrap_or_default();
    assert!(system.contains("fan-out of subagents"));
    assert!(system.contains("exactly 2 additional Agent tool call"));
    assert!(system.contains("same assistant turn"));
    assert!(system.contains("run_in_background=false"));
    assert!(system.contains("at most two WebSearch calls"));
    assert!(system.contains("forbid that subagent from spawning Agent children"));
    assert!(system.contains("fewer than two total Agent calls is incomplete"));
}

#[test]
fn explicit_fanout_prompt_requires_one_more_after_first_agent() {
    let payload = MessagesRequest {
        messages: vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Single(
                    "fan sub agent search security skills for Claude Code".to_string(),
                ),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "tool_use".to_string(),
                    id: Some("agent-1".to_string()),
                    name: Some("Agent".to_string()),
                    input: Some(serde_json::json!({"prompt":"scope one"})),
                    ..Default::default()
                }]),
            },
        ],
        tools: Some(vec![AnthropicTool {
            name: "Agent".to_string(),
            description: "launch an agent".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    let system = mapped.messages[0].content.as_deref().unwrap_or_default();
    assert!(system.contains("exactly 1 additional Agent tool call"));
}

#[test]
fn fanout_requirement_stops_after_two_agent_calls() {
    let agent_block = |id: &str| MessageContent {
        content_type: "tool_use".to_string(),
        id: Some(id.to_string()),
        name: Some("Agent".to_string()),
        input: Some(serde_json::json!({"prompt":id})),
        ..Default::default()
    };
    let payload = MessagesRequest {
        system: Some(serde_json::json!("base system")),
        messages: vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Single(
                    "fan sub agent search security skills for Claude Code".to_string(),
                ),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![agent_block("agent-1"), agent_block("agent-2")]),
            },
        ],
        tools: Some(vec![AnthropicTool {
            name: "Agent".to_string(),
            description: "launch an agent".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    let system = mapped.messages[0].content.as_deref().unwrap_or_default();
    assert_eq!(system, "base system");
}
