use super::*;

#[tokio::test]
async fn websearch_capable_payload_streams_safe_text_immediately() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_rolling".to_string(), "model".to_string());
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

    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": "first streamed words"}, "finish_reason": null}]
        })
    );
    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert_eq!(ctx.accumulated_text, "first streamed words");
    let first = rx
        .try_recv()
        .expect("content_block_start should be emitted immediately");
    let second = rx
        .try_recv()
        .expect("text_delta should be emitted immediately");
    let joined = format!("{first:?}\n{second:?}");
    assert!(joined.contains("content_block_start"));
    assert!(joined.contains("text_delta"));
    assert!(joined.contains("first streamed words"));
}

#[tokio::test]
async fn split_websearch_marker_keeps_only_marker_prefix_buffered() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_split_search".to_string(), "model".to_string());
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

    for content in [
        "Visible before [Requesting Too",
        "l execution: 'WebSearch' with arguments: {\"query\":\"Claude Code security\"}]",
    ] {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }

    assert!(ctx.intercepting_search);
    assert_eq!(ctx.search_tc_name, "WebSearch");
    assert_eq!(ctx.search_tc_args, r#"{"query":"Claude Code security"}"#);
    assert_eq!(ctx.accumulated_text, "Visible before ");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("Visible before"));
    assert!(!joined.contains("Requesting Tool execution"));
}
#[tokio::test]
async fn first_encoded_candidate_activates_lazy_fallback_after_native_finalization() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_native_recovery_gate".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new_with_encoded_fallback(false, false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run shell command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker =
        r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf FIRST_GATE"}]"#;
    let line = format!(
        "data: {}",
        serde_json::json!({"choices":[{"delta":{"content":marker},"finish_reason":"stop"}]})
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.encoded_candidate_seen);
    assert!(!ctx.native_recovery_retry_requested);
    assert!(ctx.has_emitted_tool_use);
    assert_eq!(tracker.allocated_blocks(), 1);
    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(joined.contains("tool_use"), "{joined}");
    assert!(joined.contains("FIRST_GATE"), "{joined}");
    assert!(!joined.contains("Requesting Tool execution"), "{joined}");
}

#[tokio::test]
async fn encoded_candidate_after_native_recovery_may_execute_strict_fallback() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_encoded_fallback".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new_with_encoded_fallback(false, true);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run shell command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker =
        r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf FALLBACK_OK"}]"#;
    let line = format!(
        "data: {}",
        serde_json::json!({"choices":[{"delta":{"content":marker},"finish_reason":"stop"}]})
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.native_recovery_retry_requested);
    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(joined.contains("tool_use"), "{joined}");
    assert!(joined.contains("FALLBACK_OK"), "{joined}");
}

#[tokio::test]
async fn native_tool_call_executes_during_recovery_attempt_without_encoded_fallback() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new(
        "msg_native_recovery_success".to_string(),
        "model".to_string(),
    );
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new_with_encoded_fallback(false, true);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run shell command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices":[{
                "delta":{"tool_calls":[{
                    "index":0,
                    "id":"call_native_recovery",
                    "function":{"name":"Bash","arguments":"{\"command\":\"printf NATIVE_OK\"}"}
                }]},
                "finish_reason":"tool_calls"
            }]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.encoded_candidate_seen);
    assert!(!ctx.native_recovery_retry_requested);
    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(joined.contains("tool_use"), "{joined}");
    assert!(joined.contains("NATIVE_OK"), "{joined}");
}
#[tokio::test]
async fn reasoning_compat_search_marker_is_intercepted_without_leaking_marker() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_reasoning_search".to_string(), "model".to_string());
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
    let marker_name = "Requesting Tool execution";
    let reasoning = format!(
        "Let me continue.</thinking>\n[{marker_name}: 'WebSearch' with arguments: {{\"query\":\"Claude Code API security 2026\"}}]"
    );

    for chunk in reasoning.as_bytes().chunks(11) {
        let fragment = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"reasoning_content": fragment}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.intercepting_search);
    assert_eq!(ctx.search_tc_name, "WebSearch");
    assert_eq!(
        ctx.search_tc_args,
        r#"{"query":"Claude Code API security 2026"}"#
    );
    assert_eq!(ctx.accumulated_thinking.trim(), "Let me continue.");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("thinking_delta"));
    assert!(!joined.contains(marker_name));
    assert!(!joined.contains("</thinking>"));
}
