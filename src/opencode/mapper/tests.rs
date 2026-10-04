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
fn folds_claude_code_2_1_220_system_message_into_system_prompt() {
    let payload = MessagesRequest {
        model: Some("opencode/deepseek-v4-flash-free".to_string()),
        system: Some(serde_json::json!([
            {"type":"text","text":"base system prompt"}
        ])),
        messages: vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "text".to_string(),
                    text: Some("hello".to_string()),
                    ..Default::default()
                }]),
            },
            Message {
                role: "system".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "text".to_string(),
                    text: Some(
                        "SessionStart hook additional context: use the required skill".to_string(),
                    ),
                    ..Default::default()
                }]),
            },
        ],
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let result = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());

    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].role, "system");
    assert_eq!(
        result.messages[0].content.as_deref(),
        Some("base system prompt\n\nSessionStart hook additional context: use the required skill")
    );
    assert_eq!(result.messages[1].role, "user");
    assert_eq!(result.messages[1].content.as_deref(), Some("hello"));
    assert!(result
        .messages
        .iter()
        .skip(1)
        .all(|message| message.role != "system"));
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
fn fanout_heuristic_does_not_trigger_on_negation() {
    let payload = MessagesRequest {
        model: None,
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "Do not fan out subagents, just answer directly.".to_string(),
            ),
        }],
        system: None,
        tools: Some(vec![AnthropicTool {
            name: "Agent".to_string(),
            description: "spawn an agent".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            ..Default::default()
        }]),
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: None,
        ..Default::default()
    };

    let result = map_anthropic_to_openai(&payload, "deepseek-chat".to_string());
    let system_text = result
        .messages
        .iter()
        .find(|m| m.role == "system")
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");
    assert!(
        !system_text.contains("fan-out of subagents"),
        "negated request must not inject the fan-out mandate: {system_text}"
    );
}

#[test]
fn user_image_block_is_not_silently_dropped() {
    let payload = MessagesRequest {
        model: None,
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Multiple(vec![
                MessageContent {
                    content_type: "image".to_string(),
                    source: Some(serde_json::json!({
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "abc123"
                    })),
                    ..Default::default()
                },
                MessageContent {
                    content_type: "text".to_string(),
                    text: Some("what is this?".to_string()),
                    ..Default::default()
                },
            ]),
        }],
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: None,
        max_tokens: None,
        ..Default::default()
    };

    let result = map_anthropic_to_openai(&payload, "deepseek-chat".to_string());
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role, "user");
    let text = result.messages[0].content.as_deref().unwrap();
    assert!(text.contains("what is this?"), "{text}");
    // The base64 payload must never be dumped into the prompt text.
    assert!(!text.contains("abc123"), "{text}");
    // The model must learn that the user attached an image instead of the
    // block silently vanishing from the conversation.
    assert!(text.contains("image"), "{text}");
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
fn preserves_claude_thinking_history_for_dflash_free() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "historical reasoning", "signature": "sig"},
                {"type": "text", "text": "calling tool"},
                {"type": "tool_use", "id": "call_1", "name": "Read", "input": {"path": "a"}}
            ]
        }],
        "thinking": {"type": "enabled", "budget_tokens": 64000}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());
    assert_eq!(
        mapped.messages[0].reasoning_content.as_deref(),
        Some("historical reasoning")
    );
    assert_eq!(mapped.messages[0].content.as_deref(), Some("calling tool"));
    assert_eq!(
        mapped.messages[0].tool_calls.as_ref().map(Vec::len),
        Some(1)
    );
}

#[test]
fn fills_missing_tool_reasoning_for_dflash_sync() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call_1",
                "name": "Read",
                "input": {"path": "a"}
            }]
        }],
        "stream": false,
        "thinking": {"type": "enabled", "budget_tokens": 64000}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());
    assert_eq!(
        mapped.messages[0].reasoning_content.as_deref(),
        Some("Tool call continuation.")
    );
}

#[test]
fn fills_missing_tool_reasoning_for_dflash_stream() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "messages": [{
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call_stream_1",
                "name": "Bash",
                "input": {"command": "true"}
            }]
        }],
        "stream": true,
        "thinking": {"type": "enabled", "budget_tokens": 64000}
    }))
    .unwrap();

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());
    let assistant = mapped
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("assistant history message");
    assert_eq!(
        assistant.reasoning_content.as_deref(),
        Some("Tool call continuation.")
    );
}

#[test]
fn omits_claude_structured_output_for_dflash_free() {
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
    assert!(mapped.response_format.is_none());
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
fn fanout_requirement_ignores_leading_system_reminder_agent_count() {
    let payload = MessagesRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                concat!(
                    "<system-reminder>Compatibility example: use exactly 5 Agent tool calls.</system-reminder>\n",
                    "Use exactly 4 Agent tool calls in parallel before making any implementation plan."
                )
                .to_string(),
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
    let system = mapped
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .unwrap_or_default();

    assert!(
        system.contains("exactly 4 additional Agent tool call"),
        "system prompt: {system}"
    );
    assert!(
        !system.contains("exactly 5 additional Agent tool call"),
        "system prompt: {system}"
    );
}

#[test]
fn fanout_requirement_ignores_leading_system_reminder_web_search_text() {
    let payload = MessagesRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                concat!(
                    "<system-reminder>Example policy: use web search for online research.</system-reminder>\n",
                    "Use exactly 4 Agent tool calls in parallel to inspect four local repository topics."
                )
                .to_string(),
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
    let system = mapped
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .unwrap_or_default();

    assert!(
        system.contains("The user did not explicitly request online/web research"),
        "system prompt: {system}"
    );
    assert!(
        !system.contains("The user explicitly requested online/web research"),
        "system prompt: {system}"
    );
}

#[test]
fn fanout_web_policy_respects_explicit_websearch_negation() {
    let payload = MessagesRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                concat!(
                    "Use exactly 4 Agent tool calls in parallel to inspect four local topics. ",
                    "Do not use WebSearch or WebFetch."
                )
                .to_string(),
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
    let system = mapped
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .unwrap_or_default();

    assert!(
        system.contains("The user did not explicitly request online/web research"),
        "system prompt: {system}"
    );
    assert!(
        !system.contains("The user explicitly requested online/web research"),
        "system prompt: {system}"
    );
}

#[test]
fn fanout_prompt_dispatch_four_agent_calls_injects_remaining_four_requirement() {
    let payload = MessagesRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "Use exactly 4 Agent tool calls in parallel. Each Agent must research one independent topic about building a safe study plan for learning web application security."
                    .to_string(),
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
    assert!(
        system.contains("fan-out of subagents"),
        "system prompt: {system}"
    );
    assert!(
        system.contains("exactly 4 additional Agent tool call"),
        "system prompt: {system}"
    );
    assert!(!system.contains("maximum of two Agent calls total"));
}

#[test]
fn explicit_agent_tool_prompts_a_b_c_inject_four_agent_compatibility_requirement() {
    let prompts = [
        "Use exactly 4 Agent tool calls in parallel. Each Agent must research one independent topic about building a safe study plan for learning web application security. The four topics are: HTTP basics, browser security concepts, authentication concepts, and secure coding concepts. Do not answer directly before the Agent calls finish. After all Agents return, synthesize their findings.",
        "Use exactly 4 Agent tool calls in parallel to research an authorized web application security assessment methodology in a lab or owned environment. Do not target any real website. Each Agent must cover one scope only: reconnaissance planning, attack-surface mapping, safe vulnerability testing workflow, and reporting/remediation. Do not answer directly before the Agent calls finish. After all Agents return, synthesize a high-level defensive methodology.",
        "You must use the Agent tool exactly 4 times before writing any final answer. Dispatch these 4 Agent tasks in parallel:\n1. Research safe reconnaissance planning for an authorized web application assessment.\n2. Research attack-surface mapping for an owned lab web app.\n3. Research safe vulnerability verification workflow without exploitation against real targets.\n4. Research reporting and remediation workflow.\n\nRules:\n- Do not answer directly until all 4 Agent results are available.\n- Do not reduce the number of Agent calls.\n- Do not merge tasks.\n- Do not call fewer than 4 Agents.\n- This is for a controlled lab/owned environment only.",
    ];

    for prompt in prompts {
        let payload = MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single(prompt.to_string()),
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
        let system = mapped
            .messages
            .iter()
            .find(|message| message.role == "system")
            .and_then(|message| message.content.as_deref())
            .unwrap_or_default();

        assert!(
            system.contains("exactly 4 additional Agent tool call"),
            "fan-out compatibility requirement missing for prompt: {prompt}\nMapped system: {system}"
        );
        assert!(
            system.contains("run_in_background=true"),
            "background Agent requirement missing for prompt: {prompt}\nMapped system: {system}"
        );
        assert!(
            system.contains("launch all requested Agent calls before calling TaskOutput"),
            "launch-before-collect requirement missing for prompt: {prompt}\nMapped system: {system}"
        );
    }
}

#[test]
fn explicit_fanout_prompt_honors_user_requested_four_agents_without_two_cap() {
    let payload = MessagesRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "Use the Task tool to dispatch at least four subagents for independent scopes"
                    .to_string(),
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
    assert!(system.contains("exactly 4 additional Agent tool call"));
    assert!(system.contains("same assistant turn"));
    assert!(system.contains("run_in_background=true"));
    assert!(system.contains("launch all requested Agent calls before calling TaskOutput"));
    assert!(system.contains("forbid that subagent from spawning Agent children"));
    assert!(!system.contains("maximum of two Agent calls total"));
    assert!(!system.contains("fewer than two total Agent calls is incomplete"));
}

#[test]
fn explicit_fanout_prompt_counts_down_from_user_requested_four_after_first_agent() {
    let payload = MessagesRequest {
        messages: vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Single(
                    "Use the Task tool to dispatch at least four subagents for independent scopes"
                        .to_string(),
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
    assert!(system.contains("exactly 3 additional Agent tool call"));
    assert!(!system.contains("maximum of two Agent calls total"));
}

#[test]
fn fanout_requirement_does_not_stop_after_two_when_user_requested_four() {
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
                    "Use the Task tool to dispatch at least four subagents for independent scopes"
                        .to_string(),
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
    assert!(system.contains("base system"));
    assert!(system.contains("exactly 2 additional Agent tool call"));
    assert!(!system.contains("maximum of two Agent calls total"));
}

#[test]
fn fanout_requirement_stops_after_user_requested_count() {
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
                    "Use the Task tool to dispatch at least four subagents for independent scopes"
                        .to_string(),
                ),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![
                    agent_block("agent-1"),
                    agent_block("agent-2"),
                    agent_block("agent-3"),
                    agent_block("agent-4"),
                ]),
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

#[test]
fn fanout_compatibility_does_not_force_websearch_when_user_did_not_request_it() {
    let payload = MessagesRequest {
        model: Some("claude-opus-5".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "Use exactly 4 Agent tool calls in parallel. Each Agent must research one independent topic about building a safe study plan for learning web application security. The four topics are: HTTP basics, browser security concepts, authentication concepts, and secure coding concepts. Do not answer directly before the Agent calls finish. After all Agents return, synthesize their findings."
                    .to_string(),
            ),
        }],
        tools: Some(vec![
            AnthropicTool {
                name: "Agent".to_string(),
                description: "launch an agent".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "WebSearch".to_string(),
                description: "search the web".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    let system = mapped
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .unwrap_or_default();

    assert!(
        !system.contains("require at most two WebSearch calls"),
        "compatibility instruction must not force WebSearch when the user did not ask for it: {system}"
    );
    assert!(
        system.contains("Do not call WebSearch or WebFetch"),
        "compatibility instruction should explicitly forbid web tools when the user did not request online research: {system}"
    );
}

#[test]
fn fanout_explicit_online_research_keeps_web_tools_available() {
    let payload = MessagesRequest {
        model: Some("claude-opus-5".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "Use exactly 4 Agent tool calls in parallel. Search the web for current sources about four independent safe web-security study topics."
                    .to_string(),
            ),
        }],
        tools: Some(vec![
            AnthropicTool {
                name: "Agent".to_string(),
                description: "launch an agent".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "WebSearch".to_string(),
                description: "search the web".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    let system = mapped
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .unwrap_or_default();

    assert!(system.contains("WebSearch or WebFetch may be used when needed"));
    assert!(!system.contains("Do not call WebSearch or WebFetch"));
}

#[test]
fn explicit_four_agent_fanout_keeps_parallel_tool_calls_enabled_with_websearch_available() {
    let payload = MessagesRequest {
        model: Some("claude-opus-5".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single(
                "Use exactly 4 Agent tool calls in parallel. Each Agent must research one independent topic about building a safe study plan for learning web application security. The four topics are: HTTP basics, browser security concepts, authentication concepts, and secure coding concepts. Do not answer directly before the Agent calls finish. After all Agents return, synthesize their findings."
                    .to_string(),
            ),
        }],
        tools: Some(vec![
            AnthropicTool {
                name: "Agent".to_string(),
                description: "launch an agent".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "WebSearch".to_string(),
                description: "search the web".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        stream: true,
        max_tokens: Some(1024),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".into());
    assert_eq!(
        mapped.parallel_tool_calls, None,
        "explicit Agent fan-out must not be serialized just because WebSearch is available"
    );
}

#[test]
fn parallel_tool_calls_false_is_serialized_when_tools_present() {
    let payload = MessagesRequest {
        model: Some("claude-sonnet-4-6".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single("hi".to_string()),
        }],
        system: None,
        tools: Some(vec![AnthropicTool {
            name: "WebSearch".to_string(),
            description: "search".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        }]),
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: Some(100),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "claude-sonnet-4-6".to_string());
    let serialized = serde_json::to_string(&mapped).unwrap();
    assert!(
        serialized.contains("\"parallel_tool_calls\":false"),
        "expected parallel_tool_calls=false in {serialized}"
    );
    let mapped_no_tools = map_anthropic_to_openai(
        &MessagesRequest {
            tools: None,
            ..payload
        },
        "claude-sonnet-4-6".to_string(),
    );
    let serialized_no_tools = serde_json::to_string(&mapped_no_tools).unwrap();
    assert!(
        !serialized_no_tools.contains("parallel_tool_calls"),
        "parallel_tool_calls should be omitted without tools: {serialized_no_tools}"
    );
}

#[test]
fn dflash_free_json_schema_reaches_the_system_prompt() {
    let payload = MessagesRequest {
        model: Some("opencode/deepseek-v4-flash-free".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single("list users".to_string()),
        }],
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: None,
        max_tokens: Some(100),
        output_config: Some(crate::handlers::OutputConfig {
            format: Some(serde_json::json!({
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {"name": {"type": "string"}}
                }
            })),
            ..Default::default()
        }),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "opencode/deepseek-v4-flash-free".to_string());
    assert_eq!(
        mapped.response_format, None,
        "free DFLASH must not receive an upstream grammar constraint"
    );
    let system = mapped
        .messages
        .iter()
        .find_map(|m| (m.role == "system").then_some(m.content.clone()).flatten())
        .expect("mapped request must carry a system message");
    assert!(
        system.contains("\"properties\""),
        "dropped json_schema must be preserved in the system prompt: {system}"
    );
}

#[test]
fn parallel_tool_calls_omitted_for_non_search_tool_sets() {
    let payload = MessagesRequest {
        model: Some("claude-sonnet-4-6".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single("hi".to_string()),
        }],
        system: None,
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run a command".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        }]),
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: Some(100),
        ..Default::default()
    };

    let mapped = map_anthropic_to_openai(&payload, "claude-sonnet-4-6".to_string());
    let serialized = serde_json::to_string(&mapped).unwrap();
    assert!(
        !serialized.contains("parallel_tool_calls"),
        "non-search tool sets must not force serial emission: {serialized}"
    );
}
