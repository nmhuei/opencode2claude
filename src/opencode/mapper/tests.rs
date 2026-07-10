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
