use super::context::{
    finalize_stream_with_text, process_openai_sse_line, split_pending_text, StreamContext,
};
use crate::handlers::{AnthropicTool, MessagesRequest};
use crate::opencode::forward::common::{
    extract_compat_tool_requests, extract_compat_tool_requests_detailed, get_correct_tool_name,
    parse_compat_tool_request, parse_compat_tool_request_at_eof,
};
use crate::opencode::sanitize::{extract_and_clean_dsml_detailed, strip_system_tags};
use crate::sse::SseEventBuilder;
use crate::stream_tracker::SseBlockTracker;

#[test]
fn test_split_pending_text() {
    assert_eq!(
        split_pending_text("hello<"),
        ("hello".to_string(), "<".to_string())
    );
    assert_eq!(
        split_pending_text("hello<｜"),
        ("hello".to_string(), "<｜".to_string())
    );
    assert_eq!(
        split_pending_text("hello<｜DSML｜tool_calls>"),
        ("hello".to_string(), "<｜DSML｜tool_calls>".to_string())
    );
    assert_eq!(
        split_pending_text("hello"),
        ("hello".to_string(), "".to_string())
    );
}

fn empty_messages_request() -> MessagesRequest {
    MessagesRequest {
        model: Some("model".to_string()),
        messages: vec![],
        system: None,
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: Some(100),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_process_openai_sse_line_keeps_final_partial_delta() {
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let line = r#"data: {"choices":[{"delta":{"content":"final words without newline"},"finish_reason":null}]}"#;

    let done = process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert!(!done);
    assert_eq!(ctx.accumulated_text, "final words without newline");
    assert!(tracker.has_any_blocks_ever_opened());
}

#[tokio::test]
async fn test_process_openai_sse_line_emits_thinking_delta() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking step 1"},"finish_reason":null}]}"#;

    let done = process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert!(!done);
    assert_eq!(ctx.accumulated_thinking, "thinking step 1");
    assert_eq!(tracker.thinking_idx(), Some(0));
    assert!(tracker.has_any_blocks_ever_opened());

    let start = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for thinking block start")
        .expect("thinking block start event missing");
    let delta = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for thinking delta")
        .expect("thinking delta event missing");

    let start_dbg = format!("{:?}", start);
    let delta_dbg = format!("{:?}", delta);
    assert!(
        start_dbg.contains("content_block_start") && start_dbg.contains("thinking"),
        "expected thinking content_block_start, got: {}",
        start_dbg
    );
    assert!(
        delta_dbg.contains("thinking_delta") && delta_dbg.contains("thinking step 1"),
        "expected thinking_delta event, got: {}",
        delta_dbg
    );
}

#[tokio::test]
async fn long_reasoning_is_segmented_for_interactive_rendering() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let builder = SseEventBuilder::new("msg_segmented_thinking".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let first = "a".repeat(400);

    for reasoning in [&first, "tail"] {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"reasoning_content": reasoning}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(
        joined.matches("event: content_block_start").count(),
        2,
        "{joined}"
    );
    assert_eq!(joined.matches("thinking_delta").count(), 2, "{joined}");
    assert_eq!(
        joined.matches("event: content_block_stop").count(),
        1,
        "{joined}"
    );
    assert_eq!(ctx.accumulated_thinking, format!("{first}tail"));
    assert_eq!(tracker.thinking_idx(), Some(1));
}

#[tokio::test]
async fn test_process_openai_sse_line_streams_thinking_then_text() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();

    let reasoning_line =
        r#"data: {"choices":[{"delta":{"reasoning_content":"think"},"finish_reason":null}]}"#;
    let text_line = r#"data: {"choices":[{"delta":{"content":"answer"},"finish_reason":null}]}"#;

    assert!(
        !process_openai_sse_line(
            reasoning_line,
            &mut ctx,
            &mut tracker,
            &tx,
            &builder,
            &payload,
        )
        .await
    );
    assert!(
        !process_openai_sse_line(text_line, &mut ctx, &mut tracker, &tx, &builder, &payload).await
    );

    let mut events = Vec::new();
    for _ in 0..5 {
        events.push(
            tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out waiting for SSE event")
                .expect("SSE event missing"),
        );
    }
    let joined = events
        .iter()
        .map(|e| format!("{:?}", e))
        .collect::<Vec<_>>()
        .join("\n---\n");

    assert!(joined.contains("content_block_start") && joined.contains("thinking"));
    assert!(joined.contains("thinking_delta") && joined.contains("think"));
    assert!(joined.contains("content_block_stop"), "events: {}", joined);
    assert!(joined.contains("content_block_start") && joined.contains("text"));
    assert!(joined.contains("text_delta") && joined.contains("answer"));
    assert_eq!(ctx.accumulated_thinking, "think");
    assert_eq!(ctx.accumulated_text, "answer");
}

#[tokio::test]
async fn reasoning_success_phrase_is_emitted_before_final_text() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new(
        "msg_reasoning_success_before_text".to_string(),
        "model".to_string(),
    );
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Bash"]);

    let reasoning = "The command executed successfully and returned RAW_BOUNDARY_BASH_OK.";
    let final_text = "RAW_BOUNDARY_BASH_DONE";
    let reasoning_line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"reasoning_content": reasoning},
                "finish_reason": null
            }]
        })
    );
    let text_line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"content": final_text},
                "finish_reason": "stop"
            }]
        })
    );

    process_openai_sse_line(
        &reasoning_line,
        &mut ctx,
        &mut tracker,
        &tx,
        &builder,
        &payload,
    )
    .await;
    process_openai_sse_line(&text_line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    let reasoning_pos = joined
        .find(reasoning)
        .expect("reasoning delta must be emitted");
    let text_pos = joined
        .find(final_text)
        .expect("final text delta must be emitted");

    assert!(
        reasoning_pos < text_pos,
        "reasoning was reordered after final text: {joined}"
    );
    assert_eq!(ctx.accumulated_thinking, reasoning);
    assert_eq!(ctx.accumulated_text, final_text);
}

#[tokio::test]
async fn test_process_openai_sse_line_accepts_reasoning_aliases() {
    for (field, expected) in [("reasoning", "alias-think"), ("thinking", "alias-think-2")] {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = empty_messages_request();
        let line = format!(
            r#"data: {{"choices":[{{"delta":{{"{}":"{}"}},"finish_reason":null}}]}}"#,
            field, expected
        );

        let done =
            process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

        assert!(!done);
        let start = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for thinking block start")
            .expect("thinking block start event missing");
        let delta = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for thinking delta")
            .expect("thinking delta event missing");
        let joined = format!("{:?}\n{:?}", start, delta);
        assert!(joined.contains("thinking"), "events: {}", joined);
        assert!(joined.contains("thinking_delta"), "events: {}", joined);
        assert!(joined.contains(expected), "events: {}", joined);
    }
}

#[tokio::test]
async fn test_process_openai_sse_line_done_marker_stops_outer_stream() {
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    let payload = empty_messages_request();

    let done = process_openai_sse_line(
        "data: [DONE]",
        &mut ctx,
        &mut tracker,
        &tx,
        &builder,
        &payload,
    )
    .await;

    assert!(done);
}

#[test]
fn test_get_correct_tool_name() {
    let req = MessagesRequest {
        model: Some("model".to_string()),
        messages: vec![],
        system: None,
        tools: Some(vec![AnthropicTool {
            name: "Skill".to_string(),
            description: "Skill tool".to_string(),
            input_schema: serde_json::json!({}),
            ..Default::default()
        }]),
        tool_choice: None,
        stream: false,
        temperature: None,
        max_tokens: Some(100),
        ..Default::default()
    };
    assert_eq!(get_correct_tool_name("skill", &req), "Skill");
    assert_eq!(get_correct_tool_name("Skill", &req), "Skill");
    assert_eq!(get_correct_tool_name("other", &req), "other");
}

#[tokio::test]
async fn dsml_tags_split_across_sse_chunks_emit_tool_use_and_trailing_text() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_split".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "execute a shell command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    let chunks = [
        "prefix <",
        "｜DSML｜tool_",
        "calls><｜DSML｜invoke name=\"Bash\"><｜DSML｜parameter name=\"command\">printf SPLIT_OK</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_",
        "calls> suffix",
    ];

    for chunk in chunks {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{
                    "delta": {"content": chunk},
                    "finish_reason": serde_json::Value::Null
                }]
            })
        );
        assert!(
            !process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload,).await
        );
    }

    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(format!("{:?}", event));
    }
    let joined = events.join("\n");

    assert!(joined.contains("tool_use"), "events: {joined}");
    assert!(joined.contains("Bash"), "events: {joined}");
    assert!(joined.contains("printf SPLIT_OK"), "events: {joined}");
    assert!(joined.contains("prefix "), "events: {joined}");
    assert!(joined.contains(" suffix"), "events: {joined}");
    assert!(ctx.has_emitted_tool_use);
    assert_eq!(ctx.accumulated_text, "prefix  suffix");
}

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
async fn native_search_batch_is_rejected_without_silent_drop() {
    let (tx, _rx) = tokio::sync::mpsc::channel(32);
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
    assert!(!ctx.intercepting_search);
    assert!(ctx.search_tc_args.is_empty());
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
}

#[tokio::test]
async fn search_guard_finalization_emits_text_not_error_event() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let builder = SseEventBuilder::new("msg_guard".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();

    finalize_stream_with_text(
        "Search complete; synthesize existing results.",
        &tx,
        &builder,
        &mut tracker,
        false,
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

#[test]
fn parses_free_model_compatibility_tool_marker() {
    let marker = "preface [Requesting Tool execution: 'WebSearch' with arguments: {\"query\":\"Claude Code security\"}]";
    let (name, arguments, prefix) = parse_compat_tool_request(marker).expect("marker should parse");
    assert_eq!(name, "WebSearch");
    assert_eq!(arguments, r#"{"query":"Claude Code security"}"#);
    assert_eq!(prefix, "preface");
}

#[test]
fn compat_parser_accepts_real_lowercase_marker_and_repairs_glob_quotes() {
    let text = r#"[Requesting tool execution: 'Bash' with arguments: {"command":"find /home/light/GitHub/ANSER -type f -name "*.py" | sort","description":"List all Python files"}]
[Requesting tool execution: 'Bash' with arguments: {"command":"ls -la /home/light/GitHub/ANSER/","description":"List root directory"}]"#;

    let (cleaned, calls) = extract_compat_tool_requests(text);
    assert!(cleaned.trim().is_empty(), "marker leaked: {cleaned:?}");
    assert_eq!(calls.len(), 2, "calls: {calls:?}");
    for (name, arguments) in &calls {
        assert_eq!(name, "Bash");
        serde_json::from_str::<serde_json::Value>(arguments)
            .expect("repaired arguments must be valid JSON");
    }
    let first: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
    assert_eq!(
        first["command"],
        r#"find /home/light/GitHub/ANSER -type f -name "*.py" | sort"#
    );
    assert_eq!(first["description"], "List all Python files");
}

#[test]
fn compat_parser_at_eof_accepts_complete_json_without_marker_bracket() {
    let marker = r#"[Requesting tool execution: 'Write' with arguments: {"file_path":"Makefile","content":"all:\n\t@echo ok\n"}"#;
    assert!(
        parse_compat_tool_request(marker).is_none(),
        "stream parser must wait for the closing marker bracket"
    );
    let (name, arguments, prefix, consumed) = parse_compat_tool_request_at_eof(marker)
        .expect("EOF parser should recover wrapper omission");
    assert_eq!(name, "Write");
    assert!(prefix.is_empty());
    assert_eq!(consumed, marker.len());
    let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();
    assert_eq!(parsed["file_path"], "Makefile");
    assert_eq!(parsed["content"], "all:\n\t@echo ok\n");
}

#[tokio::test]
async fn complete_json_without_marker_bracket_becomes_tool_use_at_eof() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_eof_wrapper".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Write".to_string(),
            description: "write file".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "file_path":{"type":"string"},
                    "content":{"type":"string"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = r#"[Requesting tool execution: 'Write' with arguments: {"file_path":"Makefile","content":"all:\n\t@echo ok\n"}"#;
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );
    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    assert!(!ctx.has_emitted_tool_use, "must wait until EOF");
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    assert!(ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.is_empty());
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("tool_use"), "{joined}");
    assert!(joined.contains("Makefile"), "{joined}");
    assert!(
        !joined.contains("Incomplete tool request omitted"),
        "{joined}"
    );
}

#[test]
fn compat_parser_accepts_missing_tool_word_case_and_spacing_variants() {
    let variants = [
        r#"[Requesting Execution: 'Bash' with arguments: {"command":"printf SOC_OK"}]"#,
        r#"[  REQUESTING   tool   EXECUTION : "Bash" WITH ARGS : {"command":"printf SOC_OK"}]"#,
    ];

    for marker in variants {
        let (name, arguments, prefix) =
            parse_compat_tool_request(marker).expect("variant should parse");
        assert_eq!(name, "Bash");
        assert!(prefix.is_empty());
        let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();
        assert_eq!(parsed["command"], "printf SOC_OK");
    }
}

#[tokio::test]
async fn lowercase_compat_marker_split_across_chunks_becomes_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_lowercase_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run shell".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "command":{"type":"string"},
                    "description":{"type":"string"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = r#"[Requesting tool execution: 'Bash' with arguments: {"command":"find . -name "*.py" | sort","description":"List Python"}]"#;

    for chunk in marker.as_bytes().chunks(7) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(
        ctx.accumulated_text.is_empty(),
        "leaked: {:?}",
        ctx.accumulated_text
    );
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("tool_use"), "{joined}");
    assert!(joined.contains("find . -name"), "{joined}");
    assert!(!joined.contains("Requesting tool execution"), "{joined}");
}

#[tokio::test]
async fn compat_search_marker_is_intercepted_without_streaming_marker_text() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_compat".to_string(), "model".to_string());
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
    let marker = "[Requesting Tool execution: 'WebSearch' with arguments: {\"query\":\"MCP security servers\"}]";
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"content": marker},
                "finish_reason": "stop"
            }]
        })
    );

    assert!(
        !process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload,).await
    );
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.intercepting_search);
    assert_eq!(ctx.search_tc_name, "WebSearch");
    assert_eq!(ctx.search_tc_args, r#"{"query":"MCP security servers"}"#);
    assert!(ctx.accumulated_text.is_empty());
    assert!(
        rx.try_recv().is_err(),
        "compat marker must not leak to client"
    );
}

#[test]
fn compat_marker_parser_handles_array_closing_brackets() {
    let marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf ']'; echo ok","items":["a","b"]}] trailing"#;
    let (name, arguments, prefix) =
        parse_compat_tool_request(marker).expect("complete marker should parse");
    assert_eq!(name, "Bash");
    assert_eq!(prefix, "");
    let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();
    assert_eq!(parsed["items"], serde_json::json!(["a", "b"]));
}

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
async fn bash_compat_marker_becomes_tool_use_without_leaking_marker() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_bash_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run shell command".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "command":{"type":"string"},
                    "description":{"type":"string"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };

    for content in [
        "[Requesting Tool exec",
        "ution: 'Bash' with arguments: {\"command\":\"ls -la /home/light/.local/share/claude/\",\"description\":\"Check full install structure\"}]",
    ] {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(!ctx.intercepting_search);
    assert!(ctx.has_emitted_tool_use);
    assert_eq!(ctx.final_stop_reason, "tool_use");
    assert!(ctx.accumulated_text.is_empty());

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("tool_use"));
    assert!(joined.contains("Bash"));
    assert!(joined.contains("ls -la /home/light/.local/share/claude/"));
    assert!(!joined.contains("Requesting Tool execution"));
}

#[test]
fn extracts_multiple_compat_tool_markers_in_order() {
    let marker = "Requesting Tool execution";
    let text = format!(
        "before [{marker}: 'Read' with arguments: {{\"file_path\":\"/tmp/a\",\"limit\":50}}]\n[{marker}: 'Read' with arguments: {{\"file_path\":\"/tmp/b\",\"limit\":100}}] after"
    );
    let (cleaned, calls) = extract_compat_tool_requests(&text);
    assert_eq!(cleaned, "before \n after");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "Read");
    assert_eq!(calls[1].0, "Read");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls[0].1).unwrap()["file_path"],
        "/tmp/a"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls[1].1).unwrap()["limit"],
        100
    );
}

#[test]
fn shorthand_taskupdate_batch_and_read_markers_are_extracted() {
    let text = concat!(
        "[Requesting TaskUpdate with arguments: ",
        "{\"status\":\"completed\",\"taskId\":\"10\"}, ",
        "{\"status\":\"completed\",\"taskId\":\"12\"}]\n",
        "[Requesting Read with arguments: ",
        "{\"file_path\":\"/home/light/GitHub/CTF/skills/ctf-skills/solve-challenge/SKILL.md\"}]"
    );

    let (cleaned, calls) = extract_compat_tool_requests(text);
    assert!(cleaned.trim().is_empty(), "marker leaked: {cleaned:?}");
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, "TaskUpdate");
    assert_eq!(calls[1].0, "TaskUpdate");
    assert_eq!(calls[2].0, "Read");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls[0].1).unwrap()["taskId"],
        "10"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls[1].1).unwrap()["taskId"],
        "12"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls[2].1).unwrap()["file_path"],
        "/home/light/GitHub/CTF/skills/ctf-skills/solve-challenge/SKILL.md"
    );
}

#[tokio::test]
async fn shorthand_taskupdate_batch_becomes_two_tool_use_blocks_without_leaking() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_taskupdate_batch".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "TaskUpdate".to_string(),
            description: "update task".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "status":{"type":"string"},
                    "taskId":{"type":"string"}
                },
                "required":["status","taskId"]
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "[Requesting TaskUpdate with arguments: ",
        "{\"status\":\"completed\",\"taskId\":\"10\"}, ",
        "{\"status\":\"completed\",\"taskId\":\"12\"}]"
    );

    for chunk in marker.as_bytes().chunks(11) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert_eq!(ctx.final_stop_reason, "tool_use");
    assert!(ctx.accumulated_text.trim().is_empty());
    assert!(!ctx.compat_retry_requested);

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(joined.matches("tool_use").count(), 2);
    assert!(joined.contains("TaskUpdate"));
    assert!(joined.contains("10"));
    assert!(joined.contains("12"));
    assert!(!joined.contains("Requesting TaskUpdate"));
}

#[tokio::test]
async fn shorthand_read_marker_becomes_tool_use_without_leaking() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_read_shorthand".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Read".to_string(),
            description: "read file".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{"file_path":{"type":"string"}},
                "required":["file_path"]
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = "[Requesting Read with arguments: {\"file_path\":\"/tmp/SKILL.md\"}]";

    for chunk in marker.as_bytes().chunks(7) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(joined.matches("tool_use").count(), 1);
    assert!(joined.contains("Read"));
    assert!(joined.contains("/tmp/SKILL.md"));
    assert!(!joined.contains("Requesting Read"));
    assert!(!ctx.compat_retry_requested);
}

#[tokio::test]
async fn consecutive_read_compat_markers_become_two_tool_use_blocks() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_read_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Read".to_string(),
            description: "read file".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "file_path":{"type":"string"},
                    "limit":{"type":"integer"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = "Requesting Tool execution";
    let content = format!(
        "[{marker}: 'Read' with arguments: {{\"file_path\":\"/tmp/ANSER/app.py\",\"limit\":50}}]\n[{marker}: 'Read' with arguments: {{\"file_path\":\"/tmp/ANSER/CLAUDE.md\",\"limit\":100}}]"
    );

    for chunk in content.as_bytes().chunks(17) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.trim().is_empty());
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(joined.matches("tool_use").count(), 2);
    assert!(joined.contains("/tmp/ANSER/app.py"));
    assert!(joined.contains("/tmp/ANSER/CLAUDE.md"));
    assert!(!joined.contains(marker));
}

#[test]
fn compat_parser_recovers_detached_duplicate_fields() {
    let marker = r#"[Requesting Tool execution: 'Bash' with arguments:{"command":"curl -s http://127.0.0.1:5002/health","description":"Check health"}, "description": null}]"#;
    let (name, arguments, prefix) =
        parse_compat_tool_request(marker).expect("detached field shape should recover");
    let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();

    assert_eq!(name, "Bash");
    assert!(prefix.is_empty());
    assert_eq!(parsed["command"], "curl -s http://127.0.0.1:5002/health");
    assert_eq!(parsed["description"], "Check health");
}

#[test]
fn compat_parser_repairs_unescaped_json_example_inside_write_content() {
    let marker = r#"[Requesting Tool execution: 'Write' with arguments:{"content":"Proof: curl returned {"success":true,"warehouses":[]}. Keep this exact text.","file_path":"/tmp/report.md"}]"#;
    let (name, arguments, _) =
        parse_compat_tool_request(marker).expect("embedded JSON quote shape should recover");
    let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();

    assert_eq!(name, "Write");
    assert_eq!(parsed["file_path"], "/tmp/report.md");
    assert_eq!(
        parsed["content"],
        r#"Proof: curl returned {"success":true,"warehouses":[]}. Keep this exact text."#
    );
}

#[test]
fn parses_exact_bash_and_read_markers_without_space_after_arguments_colon() {
    let text = r#"[Requesting Tool execution: 'Bash' with arguments:{"command":"grep -rn 'rq\|RQ\|Queue\|enqueue\|redis' /tmp/ANSER/core/ --include='*.py' 2>/dev/null | grep -v '.pyc' | head -40","description":"Search for RQ/Redis usage in core directory"}]
[Requesting Tool execution: 'Read' with arguments:{"file_path":"/tmp/ANSER/core/automation_engine.py"}]"#;

    let (cleaned, calls) = extract_compat_tool_requests(text);
    assert!(cleaned.trim().is_empty(), "cleaned text: {cleaned:?}");
    assert_eq!(calls.len(), 2, "calls: {calls:?}");
    assert_eq!(calls[0].0, "Bash");
    assert_eq!(calls[1].0, "Read");
    let bash: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
    let read: serde_json::Value = serde_json::from_str(&calls[1].1).unwrap();
    assert_eq!(
        bash["description"],
        "Search for RQ/Redis usage in core directory"
    );
    assert!(bash["command"]
        .as_str()
        .unwrap_or_default()
        .contains("grep -rn 'rq\\|RQ\\|Queue\\|enqueue\\|redis'"));
    assert_eq!(read["file_path"], "/tmp/ANSER/core/automation_engine.py");
}

#[tokio::test]
async fn exact_bash_then_read_markers_become_two_tool_uses_without_leaking_text() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_exact_bash_read".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![
            AnthropicTool {
                name: "Bash".to_string(),
                description: "execute shell command".to_string(),
                input_schema: serde_json::json!({
                    "type":"object",
                    "properties":{
                        "command":{"type":"string"},
                        "description":{"type":"string"}
                    }
                }),
                ..Default::default()
            },
            AnthropicTool {
                name: "Read".to_string(),
                description: "read file".to_string(),
                input_schema: serde_json::json!({
                    "type":"object",
                    "properties":{"file_path":{"type":"string"}}
                }),
                ..Default::default()
            },
        ]),
        ..empty_messages_request()
    };
    let content = r#"[Requesting Tool execution: 'Bash' with arguments:{"command":"grep -rn 'rq\|RQ\|Queue\|enqueue\|redis' /tmp/ANSER/core/ --include='*.py' 2>/dev/null | grep -v '.pyc' | head -40","description":"Search for RQ/Redis usage in core directory"}]
[Requesting Tool execution: 'Read' with arguments:{"file_path":"/tmp/ANSER/core/automation_engine.py"}]"#;

    for chunk in content.as_bytes().chunks(11) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.trim().is_empty());
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(joined.matches("tool_use").count(), 2, "events: {joined}");
    assert!(joined.contains("Bash"));
    assert!(joined.contains("Read"));
    assert!(joined.contains("rq"));
    assert!(joined.contains("redis"));
    assert!(joined.contains("/tmp/ANSER/core/automation_engine.py"));
    assert!(!joined.contains("Requesting Tool execution"));
}

#[tokio::test]
async fn historical_bash_read_markers_survive_every_two_chunk_split() {
    let payload = MessagesRequest {
        tools: Some(vec![
            AnthropicTool {
                name: "Bash".to_string(),
                description: "execute shell command".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "Read".to_string(),
                description: "read file".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        ..empty_messages_request()
    };
    let content = r#"[Requesting Tool execution: 'Bash' with arguments:{"command":"grep -rn 'rq\|RQ\|Queue\|enqueue\|redis' /tmp/ANSER/core/ --include='*.py' 2>/dev/null | grep -v '.pyc' | head -40","description":"Search for RQ/Redis usage in core directory"}]
[Requesting Tool execution: 'Read' with arguments:{"file_path":"/tmp/ANSER/core/automation_engine.py"}]"#;

    for split in 0..=content.len() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(128);
        let builder = SseEventBuilder::new(format!("msg_split_{split}"), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;

        for part in [&content[..split], &content[split..]] {
            let line = format!(
                "data: {}",
                serde_json::json!({
                    "choices": [{"delta": {"content": part}, "finish_reason": null}]
                })
            );
            process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        }
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(format!("{event:?}"));
        }
        let joined = events.join("\n");
        assert_eq!(
            joined.matches("tool_use").count(),
            2,
            "split={split}, events={joined}"
        );
        assert!(
            !joined.contains("Requesting Tool execution"),
            "split={split}, events={joined}"
        );
    }
}

#[tokio::test]
async fn incomplete_compat_marker_requests_retry_at_eof() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_incomplete_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "execute shell command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = r#"safe prefix [Requesting Tool execution: 'Bash' with arguments:{"command":"echo unfinished""#;
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("safe prefix"));
    assert!(!joined.contains("Incomplete tool request omitted"));
    assert!(!joined.contains("Requesting Tool execution"));
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
}

#[tokio::test]
async fn prose_tool_invocation_is_buffered_and_requests_retry() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_prose_invocation".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Write".to_string(),
            description: "write file".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = "safe [Requesting Tool invocation: Write file at /tmp/golden.sv]";
    for chunk in marker.as_bytes().chunks(9) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("safe"), "{joined}");
    assert!(!joined.contains("Requesting Tool invocation"), "{joined}");
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
}

#[tokio::test]
async fn plural_prose_tool_calls_are_buffered_and_request_retry() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_plural_tool_calls".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![
            AnthropicTool {
                name: "Read".to_string(),
                description: "read file".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "Bash".to_string(),
                description: "execute shell command".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "Now let me read the key tests.\n\n",
        "[Requesting tool calls: Read(/tmp/test_diff.py), Read(/tmp/test_parser.py)]\n",
        "[Requesting tool calls: Bash(description=\"List files\", command=\"find /tmp -name \\\"*.json\\\" | head\")]"
    );
    for chunk in marker.as_bytes().chunks(7) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(
        ctx.accumulated_text
            .contains("Now let me read the key tests"),
        "{}",
        ctx.accumulated_text
    );
    assert!(!joined.contains("Requesting tool calls"), "{joined}");
    assert!(!joined.contains("test_diff.py"), "{joined}");
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
}

#[tokio::test]
async fn tool_call_for_prose_variants_are_buffered_and_request_retry() {
    let payload = MessagesRequest {
        tools: Some(vec![
            AnthropicTool {
                name: "Glob".to_string(),
                description: "find files".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "Bash".to_string(),
                description: "execute shell command".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]),
        ..empty_messages_request()
    };
    let cases = [
        concat!(
            "safe glob prefix\n",
            "[Requesting tool calls for 'Glob' with pattern \"tests/golden/**/*\"]:"
        ),
        concat!(
            "safe bash prefix\n",
            "[Requesting tool call for Bash with parameters: ",
            "{\"command\": \"find /tmp -name \\\"*.json\\\" | head\",",
            "\"description\": \"List files\"}]"
        ),
    ];

    for (case_index, marker) in cases.into_iter().enumerate() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let builder = SseEventBuilder::new(
            format!("msg_tool_call_for_{case_index}"),
            "model".to_string(),
        );
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;

        for chunk in marker.as_bytes().chunks(5) {
            let content = std::str::from_utf8(chunk).unwrap();
            let line = format!(
                "data: {}",
                serde_json::json!({
                    "choices": [{"delta": {"content": content}, "finish_reason": null}]
                })
            );
            process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        }
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(format!("{event:?}"));
        }
        let joined = events.join("\n");
        assert!(ctx.accumulated_text.contains("safe "), "case={case_index}");
        assert!(!joined.contains("Requesting tool call"), "{joined}");
        assert!(!joined.contains("tests/golden"), "{joined}");
        assert!(!joined.contains("find /tmp"), "{joined}");
        assert!(!ctx.has_emitted_tool_use);
        assert!(ctx.compat_retry_requested, "case={case_index}");
    }
}

#[tokio::test]
async fn oversized_compat_marker_is_fail_closed_without_leaking_remainder() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_oversized_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Write".to_string(),
            description: "write file".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let oversized = format!(
        "[Requesting Tool execution: 'Write' with arguments:{{\"content\":\"{}",
        "x".repeat(65 * 1024)
    );

    for content in [&oversized, "SECRET_MARKER_REMAINDER"] {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("Oversized tool request omitted"));
    assert!(!joined.contains("Requesting Tool execution"));
    assert!(!joined.contains("SECRET_MARKER_REMAINDER"));
    assert!(!ctx.has_emitted_tool_use);
}

#[tokio::test]
async fn webfetch_compat_marker_is_forwarded_to_claude_code() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_webfetch_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "WebFetch".to_string(),
            description: "fetch a URL".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string"},
                    "prompt":{"type":"string"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker_name = "Requesting Tool execution";
    let marker = format!(
        "[{marker_name}: 'WebFetch' with arguments: {{\"url\":\"https://example.com\",\"prompt\":\"Return the heading\"}}]"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );
    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(!ctx.intercepting_search);
    assert!(ctx.has_emitted_tool_use);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("tool_use"));
    assert!(joined.contains("WebFetch"));
    assert!(joined.contains("https://example.com"));
    assert!(!joined.contains(marker_name));
}

#[tokio::test]
async fn unavailable_compat_tool_is_not_emitted_as_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_unknown_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Edit".to_string(),
            description: "edit".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker_name = "Requesting Tool execution";
    let marker = format!("[{marker_name}: 'Read' with arguments: {{\"file_path\":\"/tmp/a\"}}]");
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );
    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(!ctx.has_emitted_tool_use);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(!joined.contains("tool_use"));
    assert!(!joined.contains("Unavailable tool requested: Read"));
    assert!(ctx.compat_retry_requested);
    assert!(!joined.contains(marker_name));
}

#[test]
fn compat_marker_parser_accepts_raw_multiline_json_strings() {
    let marker_name = "Requesting Tool execution";
    let marker = format!(
        "[{marker_name}: 'Agent' with arguments: {{\"description\":\"security search\",\"prompt\":\"line one\nline two\twith tab\",\"run_in_background\":true}}]"
    );
    let (name, arguments, prefix) =
        parse_compat_tool_request(&marker).expect("multiline marker should parse");
    assert_eq!(name, "Agent");
    assert_eq!(prefix, "");
    let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();
    assert_eq!(parsed["prompt"], "line one\nline two\twith tab");
    assert_eq!(parsed["run_in_background"], true);
}

#[test]
fn compat_marker_parser_repairs_unescaped_shell_quotes() {
    let marker_name = "Requesting Tool execution";
    let marker = format!(
        r#"[{marker_name}: 'Bash' with arguments: {{"command":"source /tmp/venv/bin/activate && echo "=== SQL Injection Tests ==="

curl -s "http://127.0.0.1:5002/api/warehouses" 2>&1 | head -3
printf "Done\n"","description":"Run comprehensive security test suite across all modules","timeout":30000}}]"#
    );

    let (name, arguments, prefix) =
        parse_compat_tool_request(&marker).expect("malformed Bash marker should be repaired");
    let parsed: serde_json::Value = serde_json::from_str(&arguments).unwrap();

    assert_eq!(name, "Bash");
    assert_eq!(prefix, "");
    assert_eq!(
        parsed["description"],
        "Run comprehensive security test suite across all modules"
    );
    assert_eq!(parsed["timeout"], 30000);
    let command = parsed["command"].as_str().unwrap_or_default();
    assert!(command.contains("echo \"=== SQL Injection Tests ===\""));
    assert!(command.contains("curl -s \"http://127.0.0.1:5002/api/warehouses\""));
    assert!(
        command.contains("printf \"Done\n\""),
        "parsed command: {command:?}"
    );
}

#[tokio::test]
async fn malformed_bash_compat_marker_becomes_tool_use_without_leaking_marker() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_bash_quotes".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "execute a shell command".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker_name = "Requesting Tool execution";
    let marker = format!(
        r#"[{marker_name}: 'Bash' with arguments: {{"command":"echo "=== SQL Injection Tests ==="
curl -s "http://127.0.0.1:5002/api/warehouses"","description":"security tests","timeout":30000}}]"#
    );

    for chunk in marker.as_bytes().chunks(17) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.is_empty());
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("tool_use"));
    assert!(joined.contains("Bash"));
    assert!(joined.contains("SQL Injection Tests"));
    assert!(joined.contains("api/warehouses"));
    assert!(!joined.contains(marker_name));
}

#[tokio::test]
async fn multiline_agent_compat_marker_becomes_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_agent_multiline".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Agent".to_string(),
            description: "launch subagent".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "description":{"type":"string"},
                    "prompt":{"type":"string"},
                    "run_in_background":{"type":"boolean"},
                    "subagent_type":{"type":"string"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker_name = "Requesting Tool execution";
    let marker = format!(
        "[{marker_name}: 'Agent' with arguments: {{\"description\":\"Search details\",\"prompt\":\"first line\nsecond line\",\"run_in_background\":true,\"subagent_type\":\"general-purpose\"}}]"
    );
    for chunk in marker.as_bytes().chunks(13) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.is_empty());
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("tool_use"));
    assert!(joined.contains("Agent"));
    assert!(joined.contains("first line"));
    assert!(joined.contains("second line"));
    assert!(!joined.contains(marker_name));
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

#[test]
fn legitimate_array_tool_input_remains_one_compat_call() {
    let marker = r#"[Requesting BatchRead with arguments: [{"path":"a"},{"path":"b"}]]"#;
    let (cleaned, calls) = extract_compat_tool_requests(marker);

    assert_eq!(cleaned, "");
    assert_eq!(
        calls.len(),
        1,
        "array input must not be used as a batch sentinel"
    );
    assert_eq!(calls[0].0, "BatchRead");
    let parsed: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
    assert_eq!(parsed, serde_json::json!([{"path":"a"},{"path":"b"}]));
}

#[test]
fn fenced_compat_marker_is_inert_example_text() {
    let text = concat!(
        "Example output:\n```text\n",
        "[Requesting Read with arguments: {\"file_path\":\"secret\"}]\n",
        "```\nDone."
    );
    let (cleaned, calls) = extract_compat_tool_requests(text);

    assert!(
        calls.is_empty(),
        "marker inside a fenced code block must never execute"
    );
    assert_eq!(cleaned, text);
}

#[test]
fn inline_code_compat_marker_is_inert_example_text() {
    let text = r#"Use `[Requesting Read with arguments: {"file_path":"secret"}]` as an example."#;
    let (cleaned, calls) = extract_compat_tool_requests(text);

    assert!(
        calls.is_empty(),
        "marker inside inline code must never execute"
    );
    assert_eq!(cleaned, text);
}

#[test]
fn malformed_batch_is_fail_closed_without_partial_calls() {
    let marker = concat!(
        "[Requesting TaskUpdate with arguments: ",
        "{\"status\":\"completed\",\"taskId\":\"10\"},",
        "{\"status\":\"completed\",\"taskId\":}]"
    );
    let (_cleaned, calls) = extract_compat_tool_requests(marker);

    assert!(
        calls.is_empty(),
        "a malformed batch must not emit its valid prefix"
    );
}

fn payload_with_tools(names: &[&str]) -> MessagesRequest {
    MessagesRequest {
        tools: Some(
            names
                .iter()
                .map(|name| AnthropicTool {
                    name: (*name).to_string(),
                    description: format!("{name} tool"),
                    input_schema: serde_json::json!({"type":"object"}),
                    ..Default::default()
                })
                .collect(),
        ),
        ..empty_messages_request()
    }
}

#[tokio::test]
async fn fenced_marker_split_across_chunks_never_executes() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_fenced_marker".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let chunks = [
        "Example:\n```text\n",
        r#"[Requesting Read with arguments: {"file_path":"secret"}]"#,
        "\n```\nDone.",
    ];

    for (index, content) in chunks.iter().enumerate() {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{
                    "delta": {"content": content},
                    "finish_reason": if index + 1 == chunks.len() { Some("stop") } else { None::<&str> }
                }]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert!(!joined.contains("type: \"tool_use\""), "{joined}");
    assert!(ctx.accumulated_text.contains("Requesting Read"));
}

#[tokio::test]
async fn malformed_then_valid_marker_resynchronizes_without_leaking() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_resync".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let content = concat!(
        "[Requesting Read with arguments: {\"file_path\":\"/tmp/bad\"\n",
        "[Requesting Read with arguments: {\"file_path\":\"/tmp/good\"}]"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": content}, "finish_reason": "stop"}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert_eq!(
        joined.matches("\\\"type\\\":\\\"tool_use\\\"").count(),
        1,
        "{joined}"
    );
    assert!(joined.contains("/tmp/good"), "{joined}");
    assert!(!joined.contains("/tmp/bad"), "{joined}");
    assert!(!joined.contains("Requesting Read"), "{joined}");
}

#[tokio::test]
async fn emitted_tool_then_malformed_marker_never_requests_replay() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_no_replay".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let content = concat!(
        "[Requesting Read with arguments: {\"file_path\":\"/tmp/once\"}]",
        "[Requesting Read with arguments: {\"file_path\":]"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": content}, "finish_reason": "stop"}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(ctx.has_emitted_tool_use);
    assert!(
        !ctx.compat_retry_requested,
        "an emitted side effect must never be replayed"
    );
    assert_eq!(
        joined.matches("\\\"type\\\":\\\"tool_use\\\"").count(),
        1,
        "{joined}"
    );
    assert!(joined.contains("/tmp/once"), "{joined}");
}

#[tokio::test]
async fn search_batch_is_rejected_instead_of_silently_dropping_calls() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_search_batch".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["WebSearch"]);
    let marker = concat!(
        "[Requesting WebSearch with arguments: ",
        "{\"query\":\"alpha\"},{\"query\":\"beta\"}]"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.intercepting_search);
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
    assert!(!joined.contains("alpha"), "{joined}");
    assert!(!joined.contains("beta"), "{joined}");
}

#[test]
fn quoted_json_and_escaped_markers_are_inert() {
    let cases = [
        r#"> [Requesting Read with arguments: {"file_path":"secret"}]"#,
        r#"{"example":"[Requesting Read with arguments: {\"file_path\":\"secret\"}]"}"#,
        r#"\[Requesting Read with arguments: {"file_path":"secret"}]"#,
    ];
    for text in cases {
        let (cleaned, calls) = extract_compat_tool_requests(text);
        assert!(calls.is_empty(), "{text}");
        assert_eq!(cleaned, text);
    }
}

#[test]
fn compatibility_fixture_matrix_matches_expected_semantics() {
    for relative_path in [
        "/tests/fixtures/compat_markers.json",
        "/tests/fixtures/claude_tool_markers.json",
    ] {
        let fixture_path = format!("{}{}", env!("CARGO_MANIFEST_DIR"), relative_path);
        let fixtures: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&fixture_path).expect("compat fixture file should exist"),
        )
        .expect("compat fixture JSON should parse");

        for fixture in fixtures.as_array().expect("fixture root must be an array") {
            let name = fixture["name"].as_str().expect("fixture name");
            let input = fixture["input"].as_str().expect("fixture input");
            let extraction = extract_compat_tool_requests_detailed(input);
            assert_eq!(
                extraction.cleaned_text,
                fixture["expected_cleaned"].as_str().unwrap(),
                "file={relative_path} fixture={name}"
            );
            assert_eq!(
                extraction.malformed_intent,
                fixture["expected_malformed"].as_bool().unwrap(),
                "file={relative_path} fixture={name}"
            );
            let actual_calls = extraction
                .calls
                .into_iter()
                .map(|(call_name, arguments)| {
                    serde_json::json!({
                        "name": call_name,
                        "arguments": serde_json::from_str::<serde_json::Value>(&arguments).unwrap()
                    })
                })
                .collect::<Vec<_>>();
            assert_eq!(
                actual_calls,
                fixture["expected_calls"].as_array().unwrap().clone(),
                "file={relative_path} fixture={name}"
            );
        }
    }
}

#[tokio::test]
async fn ordinary_requesting_prose_is_streamed_without_pending_retry() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let builder = SseEventBuilder::new("msg_requesting_prose".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let prose = "[Requesting approval from user before continuing]";
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": prose}, "finish_reason": null}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert_eq!(ctx.accumulated_text, prose);
    assert!(ctx.text_stream_buffer.is_empty());
    assert!(!ctx.compat_retry_requested);
    assert!(joined.contains("Requesting approval"), "{joined}");
}

#[tokio::test]
async fn shorthand_batch_survives_every_utf8_chunk_boundary() {
    let marker = concat!(
        "[Requesting TaskUpdate with arguments: ",
        "{\"status\":\"completed\",\"taskId\":\"10\"},",
        "{\"status\":\"completed\",\"taskId\":\"12\"}]"
    );
    let payload = payload_with_tools(&["TaskUpdate"]);

    for split in marker
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(marker.len()))
        .filter(|index| *index > 0 && *index < marker.len())
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let builder = SseEventBuilder::new(format!("msg_boundary_{split}"), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;

        for content in [&marker[..split], &marker[split..]] {
            let line = format!(
                "data: {}",
                serde_json::json!({
                    "choices": [{"delta": {"content": content}, "finish_reason": null}]
                })
            );
            process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        }
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        let mut joined = String::new();
        while let Ok(event) = rx.try_recv() {
            joined.push_str(&format!("{event:?}\n"));
        }
        assert!(ctx.has_emitted_tool_use, "split={split}");
        assert!(!ctx.compat_retry_requested, "split={split}");
        assert_eq!(
            joined.matches("\\\"type\\\":\\\"tool_use\\\"").count(),
            2,
            "split={split}: {joined}"
        );
        assert!(!joined.contains("Requesting TaskUpdate"), "split={split}");
    }
}

#[test]
fn dsml_and_system_tags_inside_code_samples_are_inert() {
    let dsml = concat!(
        "Example:\n```text\n",
        "<｜DSML｜tool_calls><｜DSML｜invoke name=\"Read\">",
        "<｜DSML｜parameter name=\"file_path\">secret</｜DSML｜parameter>",
        "</｜DSML｜invoke></｜DSML｜tool_calls>\n",
        "<think>literal tag</think>\n```"
    );
    let extraction = extract_and_clean_dsml_detailed(dsml);
    assert_eq!(extraction.cleaned_text, dsml);
    assert!(extraction.calls.is_empty());
    assert!(!extraction.malformed_intent);
    assert_eq!(strip_system_tags(dsml), dsml);
}

#[tokio::test]
async fn fenced_dsml_split_across_chunks_never_executes() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_fenced_dsml".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let chunks = [
        "Example:\n```text\n<｜DSML｜tool_",
        "calls><｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"file_path\">secret",
        "</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>\n```\nDone",
    ];

    for content in chunks {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert!(ctx.accumulated_text.contains("DSML"));
    assert!(!joined.contains("\"type\":\"tool_use\""), "{joined}");
}

#[tokio::test]
async fn malformed_dsml_batch_is_atomic_and_requests_retry() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_bad_dsml".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let block = concat!(
        "<｜DSML｜tool_calls>",
        "<｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"file_path\">/tmp/one</｜DSML｜parameter></｜DSML｜invoke>",
        "<｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"file_path\">/tmp/two</｜DSML｜parameter>",
        "</｜DSML｜tool_calls>"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": block}, "finish_reason": "stop"}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
    assert!(!joined.contains("/tmp/one"), "{joined}");
    assert!(!joined.contains("/tmp/two"), "{joined}");
}

#[tokio::test]
async fn dsml_search_batch_is_rejected_without_silent_drop() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_dsml_search_batch".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["WebSearch", "Read"]);
    let block = concat!(
        "<｜DSML｜tool_calls>",
        "<｜DSML｜invoke name=\"WebSearch\"><｜DSML｜parameter name=\"query\">alpha</｜DSML｜parameter></｜DSML｜invoke>",
        "<｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"file_path\">secret</｜DSML｜parameter></｜DSML｜invoke>",
        "</｜DSML｜tool_calls>"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": block}, "finish_reason": "stop"}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.intercepting_search);
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
    assert!(!joined.contains("alpha"), "{joined}");
    assert!(!joined.contains("secret"), "{joined}");
}

#[tokio::test]
async fn fenced_dsml_then_real_dsml_executes_only_real_block() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(96);
    let builder = SseEventBuilder::new("msg_dsml_after_fence".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = payload_with_tools(&["Read"]);
    let content = concat!(
        "```text\n<｜DSML｜tool_calls><｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"file_path\">fake</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>\n```\n",
        "<｜DSML｜tool_calls><｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"file_path\">real</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"
    );
    let chars = content.chars().collect::<Vec<_>>();
    for chunk in chars.chunks(13) {
        let content = chunk.iter().collect::<String>();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert_eq!(joined.matches("tool_use").count(), 1, "{joined}");
    assert!(joined.contains("real"), "{joined}");
    assert!(ctx.accumulated_text.contains("fake"));
}

#[tokio::test]
async fn direct_cron_marker_survives_every_utf8_chunk_boundary() {
    let marker = concat!(
        "Đang chuẩn bị.\n[Requesting CronCreate: ",
        "{\"cron\":\"*/30 * * * *\",\"prompt\":\"ghi tiếng Việt ✓\",\"recurring\":true}]"
    );
    let payload = payload_with_tools(&["CronCreate"]);

    for split in marker
        .char_indices()
        .map(|(index, _)| index)
        .filter(|index| *index > 0 && *index < marker.len())
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let builder = SseEventBuilder::new(format!("msg_direct_cron_{split}"), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;

        for content in [&marker[..split], &marker[split..]] {
            let line = format!(
                "data: {}",
                serde_json::json!({
                    "choices": [{"delta": {"content": content}, "finish_reason": null}]
                })
            );
            process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        }
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        let mut joined = String::new();
        while let Ok(event) = rx.try_recv() {
            joined.push_str(&format!("{event:?}\n"));
        }
        assert!(ctx.has_emitted_tool_use, "split={split}");
        assert!(!ctx.compat_retry_requested, "split={split}");
        assert_eq!(
            joined.matches("\\\"type\\\":\\\"tool_use\\\"").count(),
            1,
            "split={split}: {joined}"
        );
        assert_eq!(
            joined
                .matches("\\\"index\\\":1,\\\"type\\\":\\\"content_block_stop\\\"")
                .count(),
            1,
            "split={split}: {joined}"
        );
        assert!(joined.contains("CronCreate"), "split={split}: {joined}");
        assert!(joined.contains("recurring"), "split={split}: {joined}");
        assert!(
            !joined.contains("Requesting CronCreate"),
            "split={split}: {joined}"
        );
    }
}

#[tokio::test]
async fn duplicate_direct_marker_in_reasoning_and_text_emits_once() {
    let marker = "[Requesting CronCreate: {\"cron\":\"*/30 * * * *\",\"prompt\":\"verify\",\"recurring\":true}]";
    let payload = payload_with_tools(&["CronCreate"]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_direct_duplicate".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;

    for delta in [
        serde_json::json!({"reasoning_content": marker}),
        serde_json::json!({"content": marker}),
    ] {
        let line = format!(
            "data: {}",
            serde_json::json!({"choices": [{"delta": delta, "finish_reason": null}]})
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert_eq!(joined.matches("CronCreate").count(), 1, "{joined}");
    assert_eq!(
        joined.matches("\\\"type\\\":\\\"tool_use\\\"").count(),
        1,
        "{joined}"
    );
    assert!(!joined.contains("Requesting CronCreate"), "{joined}");
}

#[tokio::test]
async fn unverified_success_claim_before_direct_marker_is_suppressed() {
    let payload = payload_with_tools(&["CronCreate"]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_false_success".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;

    for content in [
        "Cron đã được tạo thành công.\n",
        "[Requesting CronCreate: {\"cron\":\"*/30 * * * *\",\"prompt\":\"verify\",\"recurring\":true}]",
    ] {
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(ctx.has_emitted_tool_use);
    assert!(!joined.contains("tạo thành công"), "{joined}");
    assert!(!joined.contains("Requesting CronCreate"), "{joined}");
    assert_eq!(joined.matches("CronCreate").count(), 1, "{joined}");
}

#[tokio::test]
async fn unavailable_direct_tool_never_leaks_and_requests_retry() {
    let payload = payload_with_tools(&["Read"]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_unknown_direct".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let marker = "[Requesting CronCreate: {\"cron\":\"*/30 * * * *\",\"prompt\":\"verify\"}]";
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );
    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
    assert!(!joined.contains("Requesting CronCreate"), "{joined}");
    assert!(!joined.contains("*/30"), "{joined}");
}

#[tokio::test]
async fn creating_formatter_non_json_is_fail_closed_without_leak() {
    let payload = payload_with_tools(&["CronCreate"]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_creating_formatter".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let marker = "[Creating cron: */30 * * * *, prompt: verify, recurring: true]";
    for content in marker.as_bytes().chunks(5) {
        let content = std::str::from_utf8(content).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.compat_retry_requested);
    assert!(!joined.contains("Creating cron"), "{joined}");
    assert!(!joined.contains("*/30"), "{joined}");
}

#[test]
fn tv_toolcalls_marker_from_real_session_parses_four_edit_calls() {
    let marker = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tv_toolcalls_marker.txt"
    ));
    let extraction = extract_compat_tool_requests_detailed(marker);
    assert!(!extraction.malformed_intent, "{extraction:?}");
    assert_eq!(extraction.calls.len(), 4, "{:?}", extraction.calls);
    assert!(extraction
        .cleaned_text
        .contains("model tests need explicit defaults"));
    assert!(!extraction.cleaned_text.contains("<tvToolcalls>"));
    for (name, arguments) in extraction.calls {
        assert_eq!(name, "Edit");
        let value: serde_json::Value = serde_json::from_str(&arguments).unwrap();
        assert_eq!(
            value["file_path"],
            "/home/light/Workspace/bqa-runtime-claude-owned-s01-db-r2/tests/unit/database/test_models.py"
        );
        assert!(value["old_string"].is_string());
        assert!(value["new_string"].is_string());
        assert_eq!(value["replace_all"], false);
    }
}

#[tokio::test]
async fn split_tv_toolcalls_marker_becomes_tool_uses_without_leaking_xml() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_tv_compat".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Edit".to_string(),
            description: "edit file".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "file_path":{"type":"string"},
                    "old_string":{"type":"string"},
                    "new_string":{"type":"string"},
                    "replace_all":{"type":"boolean"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "Visible prefix <tvToolcalls>",
        "<tvInvoke name=\"Edit\">",
        "<tvParameter name=\"file_path\" string=\"true\">/tmp/a.py</tvParameter>",
        "<tvParameter name=\"old_string\" string=\"true\">a &lt; b</tvParameter>",
        "<tvParameter name=\"new_string\" string=\"true\">a &gt; b</tvParameter>",
        "<tvParameter name=\"replace_all\" string=\"false\">false</tvParameter>",
        "</tvInvoke>",
        "<tvInvoke name=\"Edit\">",
        "<tvParameter name=\"file_path\" string=\"true\">/tmp/b.py</tvParameter>",
        "<tvParameter name=\"old_string\" string=\"true\">old</tvParameter>",
        "<tvParameter name=\"new_string\" string=\"true\">new</tvParameter>",
        "<tvParameter name=\"replace_all\" string=\"false\">true</tvParameter>",
        "</tvInvoke></tvToolcalls>"
    );

    for chunk in marker.as_bytes().chunks(9) {
        let content = std::str::from_utf8(chunk).unwrap();
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;

    assert!(ctx.has_emitted_tool_use);
    assert_eq!(ctx.final_stop_reason, "tool_use");
    assert_eq!(ctx.accumulated_text, "Visible prefix ");
    assert!(!ctx.compat_retry_requested);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(joined.contains("/tmp/a.py"), "{joined}");
    assert!(joined.contains("/tmp/b.py"), "{joined}");
    assert!(joined.contains("a < b"), "{joined}");
    assert!(joined.contains("a > b"), "{joined}");
    assert!(!joined.contains("tvToolcalls"), "{joined}");
    assert!(!joined.contains("tvInvoke"), "{joined}");
}

#[tokio::test]
async fn incomplete_tv_toolcalls_marker_fails_closed_and_requests_retry() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_tv_incomplete".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Edit".to_string(),
            description: "edit file".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "<tvToolcalls><tvInvoke name=\"Edit\">",
        "<tvParameter name=\"file_path\" string=\"true\">/tmp/a</tvParameter>"
    );
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
        })
    );
    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    assert!(ctx.compat_retry_requested);
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.is_empty());
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(!joined.contains("tvToolcalls"), "{joined}");
}

#[test]
fn fenced_tv_toolcalls_marker_is_inert_example_text() {
    let text = concat!(
        "```xml\n",
        "<tvToolcalls><tvInvoke name=\"Read\">",
        "<tvParameter name=\"file_path\" string=\"true\">/tmp/a</tvParameter>",
        "</tvInvoke></tvToolcalls>\n```"
    );
    let extraction = extract_compat_tool_requests_detailed(text);
    assert!(extraction.calls.is_empty());
    assert_eq!(extraction.cleaned_text, text);
    assert!(!extraction.malformed_intent);
}

#[test]
fn inline_tv_toolcalls_marker_is_inert_example_text() {
    let text = concat!(
        "Use `<tvToolcalls><tvInvoke name=\"Read\">",
        "<tvParameter name=\"file_path\" string=\"true\">/tmp/a</tvParameter>",
        "</tvInvoke></tvToolcalls>` as an example."
    );
    let extraction = extract_compat_tool_requests_detailed(text);
    assert!(extraction.calls.is_empty());
    assert_eq!(extraction.cleaned_text, text);
    assert!(!extraction.malformed_intent);
}

#[tokio::test]
async fn malformed_tv_toolcalls_structures_fail_closed_without_leaking_xml() {
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Edit".to_string(),
            description: "edit file".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let cases = [
        concat!(
            "<tvToolcalls unexpected=\"true\"><tvInvoke name=\"Edit\">",
            "<tvParameter name=\"file_path\" string=\"true\">/tmp/a</tvParameter>",
            "</tvInvoke></tvToolcalls>"
        ),
        concat!(
            "<tvToolcalls><tvInvoke name=\"Edit\" unexpected=\"true\">",
            "<tvParameter name=\"file_path\" string=\"true\">/tmp/a</tvParameter>",
            "</tvInvoke></tvToolcalls>"
        ),
        concat!(
            "<tvToolcalls><tvInvoke name=\"Edit\">",
            "<tvParameter name=\"file_path\" string=\"true\">/tmp/a</tvParameter>",
            "<tvParameter name=\"FILE_PATH\" string=\"true\">/tmp/b</tvParameter>",
            "</tvInvoke></tvToolcalls>"
        ),
        concat!(
            "<tvToolcalls><tvInvoke name=\"Edit\">",
            "<tvParameter name=\"file_path\" string=\"maybe\">/tmp/a</tvParameter>",
            "</tvInvoke></tvToolcalls>"
        ),
    ];

    for (index, marker) in cases.into_iter().enumerate() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let builder =
            SseEventBuilder::new(format!("msg_tv_malformed_{index}"), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        assert!(ctx.compat_retry_requested, "case={index}");
        assert!(!ctx.has_emitted_tool_use, "case={index}");
        assert!(ctx.accumulated_text.is_empty(), "case={index}");
        let mut joined = String::new();
        while let Ok(event) = rx.try_recv() {
            joined.push_str(&format!("{event:?}\n"));
        }
        assert!(!joined.contains("tvToolcalls"), "case={index}: {joined}");
        assert!(!joined.contains("tvInvoke"), "case={index}: {joined}");
        assert!(!joined.contains("/tmp/a"), "case={index}: {joined}");
    }
}
