use super::*;

#[tokio::test]
async fn search_arguments_received_before_name_are_preserved_by_index() {
    let (tx, _rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_search".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "WebSearch".to_string(),
            description: "search the web".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{"query":{"type":"string"}}
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let arguments_first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":\"Claude "}}]},"finish_reason":null}]}"#;
    let name_later = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_search","function":{"name":"WebSearch","arguments":"Code security\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    assert!(
        !process_openai_sse_line(
            arguments_first,
            &mut ctx,
            &mut tracker,
            &tx,
            &builder,
            &payload,
        )
        .await
    );
    assert!(
        !process_openai_sse_line(name_later, &mut ctx, &mut tracker, &tx, &builder, &payload,)
            .await
    );

    assert!(ctx.intercepting_search);
    assert_eq!(ctx.search_tc_id, "call_search");
    assert_eq!(ctx.search_tc_name, "WebSearch");
    assert_eq!(ctx.search_tc_args, r#"{"query":"Claude Code security"}"#);
}

#[tokio::test]
async fn native_tool_name_and_id_fragments_are_reassembled_by_index() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_fragmented_identity".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run a command".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{"command":{"type":"string"}}
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_","function":{"name":"Ba","arguments":"{\"command\":\"printf frag"}}]},"finish_reason":null}]}"#;
    let second = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"bash","function":{"name":"sh","arguments":"mented\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    process_openai_sse_line(first, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    process_openai_sse_line(second, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }

    assert!(ctx.has_emitted_tool_use, "{joined}");
    assert!(!ctx.compat_retry_requested, "{joined}");
    assert!(joined.contains("call_bash"), "{joined}");
    assert!(joined.contains("Bash"), "{joined}");
    assert!(joined.contains("printf fragmented"), "{joined}");
}

#[tokio::test]
async fn native_tool_cumulative_name_and_id_snapshots_do_not_duplicate_prefixes() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_cumulative_identity".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run a command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_","function":{"name":"Ba","arguments":"{\"command\":\"printf cum"}}]},"finish_reason":null}]}"#;
    let second = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_bash","function":{"name":"Bash","arguments":"ulative\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    process_openai_sse_line(first, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    process_openai_sse_line(second, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }

    assert!(ctx.has_emitted_tool_use, "{joined}");
    assert!(!ctx.compat_retry_requested, "{joined}");
    assert!(joined.contains("call_bash"), "{joined}");
    assert!(!joined.contains("call_call_bash"), "{joined}");
    assert!(joined.contains("Bash"), "{joined}");
    assert!(!joined.contains("BaBash"), "{joined}");
    assert!(joined.contains("printf cumulative"), "{joined}");
}

#[tokio::test]
async fn native_tool_cumulative_argument_snapshots_replace_the_previous_prefix() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_cumulative_arguments".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run a command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_bash","function":{"name":"Bash","arguments":"{\"command\":\"printf "}}]},"finish_reason":null}]}"#;
    let second = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_bash","function":{"name":"Bash","arguments":"{\"command\":\"printf cumulative\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    process_openai_sse_line(first, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    process_openai_sse_line(second, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }

    assert!(ctx.has_emitted_tool_use, "{joined}");
    assert!(!ctx.compat_retry_requested, "{joined}");
    assert!(joined.contains("printf cumulative"), "{joined}");
    assert_eq!(joined.matches("printf cumulative").count(), 1, "{joined}");
}

#[tokio::test]
async fn interleaved_parallel_native_tool_identity_fragments_stay_isolated_by_index() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_interleaved_identity".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![
            AnthropicTool {
                name: "Bash".to_string(),
                description: "run a command".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "Read".to_string(),
                description: "read a file".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        ..empty_messages_request()
    };

    let first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_","function":{"name":"Ba","arguments":"{\"command\":\"printf one"}}]},"finish_reason":null}]}"#;
    let second = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"read_","function":{"name":"Re","arguments":"{\"file_path\":\"/tmp/fi"}}]},"finish_reason":null}]}"#;
    let final_chunk = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"bash","function":{"name":"sh","arguments":"\"}"}},{"index":1,"id":"one","function":{"name":"ad","arguments":"le\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    process_openai_sse_line(first, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    process_openai_sse_line(second, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    process_openai_sse_line(final_chunk, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }

    assert!(ctx.has_emitted_tool_use, "{joined}");
    assert!(!ctx.compat_retry_requested, "{joined}");
    assert!(joined.contains("call_bash"), "{joined}");
    assert!(joined.contains("read_one"), "{joined}");
    assert!(joined.contains("Bash"), "{joined}");
    assert!(joined.contains("Read"), "{joined}");
    assert!(joined.contains("printf one"), "{joined}");
    assert!(joined.contains("/tmp/file"), "{joined}");
}

#[tokio::test]
async fn native_mixed_batch_drops_search_and_emits_non_search_calls() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_parallel".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![
            AnthropicTool {
                name: "WebSearch".to_string(),
                description: "search".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "Read".to_string(),
                description: "read".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        ..empty_messages_request()
    };
    let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"s","function":{"name":"WebSearch","arguments":"{\"query\":\"security\"}"}},{"index":1,"id":"r","function":{"name":"Read","arguments":"{\"path\":\"secret.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.intercepting_search);
    assert!(ctx.search_tc_args.is_empty());
    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert!(joined.contains("secret.txt"), "{joined}");
    assert!(
        !joined.contains("security"),
        "dropped search leaked: {joined}"
    );
}

#[tokio::test]
async fn search_guard_finalization_emits_text_not_error_event() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let builder = SseEventBuilder::new("msg_guard".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);

    finalize_stream_with_text(
        "Search complete; synthesize existing results.",
        &tx,
        &builder,
        &mut tracker,
        &mut ctx,
    )
    .await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(format!("{:?}", event));
    }
    let joined = events.join("\n");
    assert!(joined.contains("content_block_start"));
    assert!(joined.contains("text_delta"));
    assert!(joined.contains("Search complete"));
    assert!(joined.contains("message_stop"));
    assert!(!joined.contains("api_error"));
}
#[tokio::test]
async fn unavailable_native_tool_does_not_report_tool_use_stop_reason() {
    let (tx, _rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_unavailable_native".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "WebSearch".to_string(),
            description: "search".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let thinking = r#"data: {"choices":[{"delta":{"reasoning_content":"I should run bash."},"finish_reason":null}]}"#;
    let unavailable = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_bash","function":{"name":"Bash","arguments":"{\"command\":\"echo test\"}"}}]},"finish_reason":"tool_calls"}]}"#;
    process_openai_sse_line(thinking, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    process_openai_sse_line(unavailable, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(!ctx.has_emitted_tool_use);
    assert!(!ctx.intercepting_search);
    assert_eq!(ctx.final_stop_reason, "end_turn");
    assert!(ctx.compat_retry_requested);
    assert_eq!(ctx.accumulated_thinking, "I should run bash.");
}
#[tokio::test]
async fn native_tool_call_preserves_visible_text_when_tool_call_shares_chunk() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new(
        "msg_native_clipped_preamble".to_string(),
        "model".to_string(),
    );
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run a shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let arguments = serde_json::json!({"command": "printf PRE_TOOL_OK"}).to_string();
    let tool_line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "Proxy up (200), env đủ. Copy tinyctfer sang tools/ và đọc code container conf",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_native_clipped_preamble",
                        "type": "function",
                        "function": {"name": "Bash", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
    );
    process_openai_sse_line(&tool_line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let body = serialize_sse_events(events).await;

    assert_eq!(
        ctx.accumulated_text,
        "Proxy up (200), env đủ. Copy tinyctfer sang tools/ và đọc code container conf"
    );
    assert!(
        body.contains(
            "Proxy up (200), env đủ. Copy tinyctfer sang tools/ và đọc code container conf"
        ),
        "{body}"
    );
    assert!(body.contains("tool_use"), "{body}");
    assert!(body.contains("Bash"), "{body}");
    assert!(body.contains("printf PRE_TOOL_OK"), "{body}");
}

#[tokio::test]
async fn native_agent_placeholder_prompt_requests_retry_without_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new(
        "msg_native_agent_placeholder".to_string(),
        "model".to_string(),
    );
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Agent".to_string(),
            description: "spawn an agent".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {"type": "string"},
                    "prompt": {"type": "string"}
                },
                "required": ["description", "prompt"]
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let arguments = serde_json::json!({
        "description": "Re-review Task 1",
        "prompt": "..."
    })
    .to_string();
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_native_agent_placeholder",
                        "type": "function",
                        "function": {"name": "Agent", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.compat_retry_requested);
    assert!(!ctx.has_emitted_tool_use);
    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(
        !joined.contains("call_native_agent_placeholder"),
        "{joined}"
    );
    assert!(!joined.contains("\\\"name\\\":\\\"Agent\\\""), "{joined}");
}
