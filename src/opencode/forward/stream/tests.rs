use super::context::{
    finalize_stream_with_text, process_openai_sse_line, split_pending_text, StreamContext,
};
use crate::handlers::{AnthropicTool, ContentVal, Message, MessagesRequest};
use crate::opencode::forward::common::{
    extract_compat_tool_requests, extract_compat_tool_requests_detailed, get_correct_tool_name,
    parse_compat_tool_request, parse_compat_tool_request_at_eof,
};
use crate::opencode::sanitize::{extract_and_clean_dsml_detailed, strip_system_tags};
use crate::sse::SseEventBuilder;
use crate::stream_tracker::SseBlockTracker;
use axum::body::to_bytes;
use axum::response::{sse::Sse, IntoResponse};
use futures_util::stream;
use std::convert::Infallible;

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
    // Short reasoning stays in ONE block; only very long streams segment.
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
        1,
        "{joined}"
    );
    // The 400-byte provider delta is intentionally split into 256 + 144 byte
    // Anthropic deltas, then the follow-up `tail` remains a third delta. This
    // is transport chunking only: all three stay inside the same thinking block.
    assert_eq!(joined.matches("thinking_delta").count(), 3, "{joined}");
    assert_eq!(
        joined.matches("event: content_block_stop").count(),
        0,
        "{joined}"
    );
    assert_eq!(ctx.accumulated_thinking, format!("{first}tail"));
    assert_eq!(tracker.thinking_idx(), Some(0));
}

#[tokio::test]
async fn oversized_reasoning_segments_into_a_second_block() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_segmented_big".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    // 17KB pushes past THINKING_RENDER_CHUNK_BYTES (16KB) and must segment.
    let big = "a".repeat(17 * 1024);
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"reasoning_content": big}, "finish_reason": null}]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    // A follow-up reasoning delta opens the NEXT segment block at index 1.
    let tail_line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"reasoning_content": "tail"}, "finish_reason": null}]
        })
    );
    process_openai_sse_line(&tail_line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(joined.matches("event: content_block_start").count(), 2);
    assert_eq!(joined.matches("event: content_block_stop").count(), 1);
    assert!(joined.matches("thinking_delta").count() > 2, "{joined}");
    assert_eq!(ctx.accumulated_thinking, format!("{big}tail"));
    assert_eq!(
        tracker.thinking_idx(),
        Some(1),
        "segment continues at next index"
    );
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

#[tokio::test]
async fn mid_stream_upstream_error_emits_error_and_ends_the_stream() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let builder = SseEventBuilder::new("msg_error".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let line = r#"data: {"error": {"message": "mid-stream failure", "type": "server_error", "code": 500}}"#;

    let done = process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert!(done, "error payload must terminate the upstream stream");
    assert!(
        ctx.error_terminated,
        "error payload must mark the stream error-terminated"
    );
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for the error event")
        .expect("error event missing");
    let debug = format!("{event:?}");
    assert!(
        debug.contains("error"),
        "expected an error event, got: {debug}"
    );
    assert!(
        !debug.contains("message_stop"),
        "error must end the stream without message_stop, got: {debug}"
    );
    assert!(
        !tracker.has_any_blocks_ever_opened(),
        "an error payload must not open content blocks"
    );
}

#[tokio::test]
async fn compact_mode_still_strips_system_leak_tags() {
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let builder = SseEventBuilder::new("msg_compact".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(true);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let line = r#"data: {"choices":[{"delta":{"content":"<thinking>hidden</thinking>visible"},"finish_reason":null}]}"#;

    let done = process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert!(!done);
    assert_eq!(
        ctx.accumulated_text, "hiddenvisible",
        "compact mode must still strip leaked system tags"
    );
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

#[tokio::test]
async fn literal_marker_user_intent_is_rendered_as_text_not_tool_use() {
    let marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf SHOULD_NOT_RUN > file"}]"#;

    for encoded_fallback_permitted in [false, true] {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let builder = SseEventBuilder::new("msg_literal_marker".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new_with_encoded_fallback(false, encoded_fallback_permitted);
        ctx.message_started = true;
        let payload = MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single(format!(
                    "Output exactly this literal text and do not execute it:\n{marker}"
                )),
            }],
            tools: Some(vec![AnthropicTool {
                name: "Bash".to_string(),
                description: "run shell command".to_string(),
                input_schema: serde_json::json!({
                    "type":"object",
                    "properties":{"command":{"type":"string"}},
                    "required":["command"]
                }),
                ..Default::default()
            }]),
            ..empty_messages_request()
        };
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
            })
        );

        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        assert!(
            !ctx.has_emitted_tool_use,
            "fallback={encoded_fallback_permitted}"
        );
        assert!(
            !ctx.native_recovery_retry_requested,
            "fallback={encoded_fallback_permitted}"
        );
        assert!(
            !ctx.compat_retry_requested,
            "fallback={encoded_fallback_permitted}"
        );
        assert_eq!(
            ctx.accumulated_text, marker,
            "fallback={encoded_fallback_permitted}"
        );
        let mut joined = String::new();
        while let Ok(event) = rx.try_recv() {
            joined.push_str(&format!("{event:?}\n"));
        }
        assert!(joined.contains("SHOULD_NOT_RUN"), "{joined}");
        assert!(!joined.contains("\"type\":\"tool_use\""), "{joined}");
    }
}

#[tokio::test]
async fn compat_non_object_arguments_request_retry_without_tool_use() {
    for raw in [r#""ls""#, "123", r#"["ls"]"#] {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let builder = SseEventBuilder::new("msg_object_invariant".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = MessagesRequest {
            tools: Some(vec![AnthropicTool {
                name: "Bash".to_string(),
                description: "run shell command".to_string(),
                input_schema: serde_json::json!({
                    "type":"object",
                    "properties":{"command":{"type":"string"}},
                    "required":["command"]
                }),
                ..Default::default()
            }]),
            ..empty_messages_request()
        };
        let marker = format!("[Requesting Tool execution: 'Bash' with arguments: {raw}]");
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"content": marker}, "finish_reason": "stop"}]
            })
        );

        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
            .await;

        assert!(ctx.compat_retry_requested, "raw={raw}");
        assert!(!ctx.has_emitted_tool_use, "raw={raw}");
        let mut joined = String::new();
        while let Ok(event) = rx.try_recv() {
            joined.push_str(&format!("{event:?}\n"));
        }
        assert!(!joined.contains("tool_use"), "raw={raw}: {joined}");
    }
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
fn top_level_array_tool_input_is_rejected() {
    let marker = r#"[Requesting BatchRead with arguments: [{"path":"a"},{"path":"b"}]]"#;
    let extraction = extract_compat_tool_requests_detailed(marker);

    assert_eq!(extraction.cleaned_text, marker);
    assert!(extraction.calls.is_empty());
    assert!(
        extraction.malformed_intent,
        "top-level array input must fail closed because tool_use.input must be an object"
    );
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
async fn search_batch_collapses_to_first_search_and_intercepts_it() {
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
    assert!(ctx.intercepting_search);
    assert!(
        ctx.search_tc_args.contains("alpha"),
        "{}",
        ctx.search_tc_args
    );
    assert!(!ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
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
async fn dsml_mixed_batch_drops_search_and_emits_non_search_calls() {
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
    assert!(ctx.has_emitted_tool_use);
    assert!(!ctx.compat_retry_requested);
    assert!(joined.contains("secret"), "{joined}");
    assert!(!joined.contains("alpha"), "dropped search leaked: {joined}");
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

#[test]
fn generic_tool_calls_xml_parses_bash_call_without_leaking_markup() {
    let marker = concat!(
        "<tool_calls>",
        "<invoke name=\"Bash\">",
        "<parameter name=\"command\">/home/light/.local/cache/claude-plugins-official/superpowers/6.2.0/skills/subagent-driven-development/scripts/review-package docs/superpowers/plans/2026-08-03-ctf-workspace.md dacd2db 5659e91</parameter>",
        "<parameter name=\"description\">Generate review package for Task 1 fix round 2</parameter>",
        "</invoke>",
        "</tool_calls>",
        "</think>"
    );

    let extraction = extract_compat_tool_requests_detailed(marker);

    assert!(!extraction.malformed_intent, "{extraction:?}");
    assert_eq!(extraction.calls.len(), 1, "{extraction:?}");
    let (name, arguments) = &extraction.calls[0];
    assert_eq!(name, "Bash");
    let arguments: serde_json::Value = serde_json::from_str(arguments).unwrap();
    assert_eq!(
        arguments["command"],
        "/home/light/.local/cache/claude-plugins-official/superpowers/6.2.0/skills/subagent-driven-development/scripts/review-package docs/superpowers/plans/2026-08-03-ctf-workspace.md dacd2db 5659e91"
    );
    assert_eq!(
        arguments["description"],
        "Generate review package for Task 1 fix round 2"
    );
    assert!(!extraction.cleaned_text.contains("tool_calls"));
    assert!(!extraction.cleaned_text.contains("invoke"));
    assert!(!extraction.cleaned_text.contains("parameter"));
}

#[tokio::test]
async fn generic_tool_calls_xml_streams_agent_as_one_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_generic_agent".to_string(), "model".to_string());
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
                    "prompt": {"type": "string"},
                    "subagent_type": {"type": "string"}
                }
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "<tool_calls><invoke name=\"Agent\">",
        "<parameter name=\"description\">Review parser fix</parameter>",
        "<parameter name=\"prompt\">Inspect the tool-call lifecycle and return evidence.</parameter>",
        "<parameter name=\"subagent_type\">general-purpose</parameter>",
        "</invoke></tool_calls>"
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
    assert!(!ctx.compat_retry_requested);
    assert!(ctx.accumulated_text.is_empty(), "{}", ctx.accumulated_text);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(
        joined.matches("\\\"name\\\":\\\"Agent\\\"").count(),
        1,
        "{joined}"
    );
    assert!(joined.contains("Review parser fix"), "{joined}");
    assert!(joined.contains("general-purpose"), "{joined}");
    assert!(!joined.contains("tool_calls"), "{joined}");
    assert!(!joined.contains("<invoke"), "{joined}");
}

#[tokio::test]
async fn malformed_generic_tool_call_xml_fails_closed_without_leaking() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_generic_malformed".to_string(), "model".to_string());
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
    let marker = concat!(
        "<tool_call><invoke name=\"Bash\">\n",
        "Command: /home/light/.cli/cache/claude-plugins-official/super24/6.4.0/skills/agent-driven/review-package docs/superpowers/plans/2026-08-03-ctf-workspace.md dacd2db 5659e91\n",
        "Description: Tạo review package cho Task 1 fix round 2\n",
        "</parameter>\n</invoke>\n</tool_call></think>"
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
    assert!(ctx.accumulated_text.is_empty(), "{}", ctx.accumulated_text);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(!joined.contains("review-package"), "{joined}");
    assert!(!joined.contains("tool_call"), "{joined}");
    assert!(!joined.contains("invoke"), "{joined}");
}

#[tokio::test]
async fn incomplete_generic_tool_calls_xml_requests_retry_without_leaking() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_generic_incomplete".to_string(), "model".to_string());
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
    let marker = "<tool_calls><invoke name=\"Bash\">";
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

    assert!(ctx.compat_retry_requested);
    assert!(!ctx.has_emitted_tool_use);
    assert!(ctx.accumulated_text.is_empty(), "{}", ctx.accumulated_text);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert!(!joined.contains("tool_calls"), "{joined}");
    assert!(!joined.contains("invoke"), "{joined}");
}

#[tokio::test]
async fn generic_tool_calls_xml_bash_survives_seventeen_byte_wire_chunks() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_generic_bash_17".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "run a command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "description": {"type": "string"}
                },
                "required": ["command"]
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "<tool_calls><invoke name=\"Bash\">",
        "<parameter name=\"command\">printf GENERIC_XML_BASH_SIDE_EFFECT &gt; /tmp/bash-side-effect.txt</parameter>",
        "<parameter name=\"description\">Verify generic XML Bash exact once</parameter>",
        "</invoke></tool_calls></think>"
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
    assert_eq!(ctx.final_stop_reason, "tool_use");
    assert!(!ctx.compat_retry_requested);
    assert!(ctx.accumulated_text.is_empty(), "{}", ctx.accumulated_text);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(format!("{event:?}"));
    }
    let joined = events.join("\n");
    assert_eq!(
        joined.matches("\\\"name\\\":\\\"Bash\\\"").count(),
        1,
        "{joined}"
    );
    assert!(joined.contains("GENERIC_XML_BASH_SIDE_EFFECT"), "{joined}");
    assert!(joined.contains("/tmp/bash-side-effect.txt"), "{joined}");
    assert!(!joined.contains("tool_calls"), "{joined}");
    assert!(!joined.contains("<invoke"), "{joined}");
}

#[tokio::test]
async fn generic_agent_placeholder_prompt_requests_retry_without_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_agent_placeholder".to_string(), "model".to_string());
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
    let marker = concat!(
        "<tool_calls><invoke name=\"Agent\">",
        "<parameter name=\"description\">Re-review Task 1</parameter>",
        "<parameter name=\"prompt\">...</parameter>",
        "</invoke></tool_calls>"
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
    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(!joined.contains("\\\"name\\\":\\\"Agent\\\""), "{joined}");
    assert!(!joined.contains("..."), "{joined}");
}

#[tokio::test]
async fn generic_sendmessage_placeholder_message_requests_retry_without_tool_use() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_send_placeholder".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "SendMessage".to_string(),
            description: "continue an agent".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "message": {"type": "string"}
                },
                "required": ["to", "message"]
            }),
            ..Default::default()
        }]),
        ..empty_messages_request()
    };
    let marker = concat!(
        "<tool_calls><invoke name=\"SendMessage\">",
        "<parameter name=\"to\">agent-a40116a5229e783e6</parameter>",
        "<parameter name=\"message\">...</parameter>",
        "</invoke></tool_calls>"
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
    let mut joined = String::new();
    while let Ok(event) = rx.try_recv() {
        joined.push_str(&format!("{event:?}\n"));
    }
    assert!(
        !joined.contains("\\\"name\\\":\\\"SendMessage\\\""),
        "{joined}"
    );
    assert!(!joined.contains("..."), "{joined}");
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

async fn serialize_sse_events(events: Vec<axum::response::sse::Event>) -> String {
    let response = Sse::new(stream::iter(
        events
            .into_iter()
            .map(Ok::<axum::response::sse::Event, Infallible>),
    ))
    .into_response();
    String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .expect("serialize SSE body")
            .to_vec(),
    )
    .expect("SSE body must be UTF-8")
}

#[tokio::test]
async fn one_large_reasoning_sse_line_is_split_into_bounded_anthropic_deltas() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_direct_big_thinking".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let reasoning = "lập luận Unicode tiếng Việt — ".repeat(700);
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"reasoning_content": reasoning},
                "finish_reason": null
            }]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    let body = serialize_sse_events(events).await;
    let mut fragments = Vec::new();
    let mut wire_lengths = Vec::new();
    for line in body.lines().filter(|line| line.starts_with("data: ")) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line[6..]) else {
            continue;
        };
        if value["type"] == "content_block_delta" && value["delta"]["type"] == "thinking_delta" {
            fragments.push(
                value["delta"]["thinking"]
                    .as_str()
                    .expect("thinking fragment")
                    .to_string(),
            );
            wire_lengths.push(line.len());
        }
    }

    assert!(
        fragments.len() > 1,
        "one provider event must not become one TUI-blocking delta; lengths={wire_lengths:?}"
    );
    assert!(
        wire_lengths.iter().all(|length| *length <= 2_048),
        "outgoing thinking delta exceeded render bound: {wire_lengths:?}"
    );
    assert_eq!(fragments.concat(), reasoning);
    assert_eq!(ctx.accumulated_thinking, reasoning);
}

#[tokio::test]
async fn one_large_text_sse_line_is_split_without_corrupting_utf8() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_direct_big_text".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();
    let text = "Báo cáo chuyên gia hoàn tất — dữ liệu được giữ nguyên.\n".repeat(180);
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"content": text},
                "finish_reason": null
            }]
        })
    );

    process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    let body = serialize_sse_events(events).await;
    let mut fragments = Vec::new();
    let mut wire_lengths = Vec::new();
    for line in body.lines().filter(|line| line.starts_with("data: ")) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line[6..]) else {
            continue;
        };
        if value["type"] == "content_block_delta" && value["delta"]["type"] == "text_delta" {
            fragments.push(
                value["delta"]["text"]
                    .as_str()
                    .expect("text fragment")
                    .to_string(),
            );
            wire_lengths.push(line.len());
        }
    }

    assert!(
        fragments.len() > 1,
        "one provider event must not become one TUI-blocking delta; lengths={wire_lengths:?}"
    );
    assert!(
        wire_lengths.iter().all(|length| *length <= 2_048),
        "outgoing text delta exceeded render bound: {wire_lengths:?}"
    );
    assert_eq!(fragments.concat(), text);
    assert_eq!(ctx.accumulated_text, text);
}

#[tokio::test(start_paused = true)]
async fn one_large_text_sse_line_is_paced_across_scheduler_ticks() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let text = "Báo cáo khổng lồ cần chảy dần — ".repeat(160);
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"content": text},
                "finish_reason": null
            }]
        })
    );
    let expected = text.clone();

    let task = tokio::spawn(async move {
        let builder = SseEventBuilder::new("msg_paced_big_text".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = empty_messages_request();
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        ctx
    });

    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "one giant provider text event was enqueued in a single scheduler burst"
    );

    let mut immediate_text_deltas = 0usize;
    while let Ok(event) = rx.try_recv() {
        if format!("{event:?}").contains("text_delta") {
            immediate_text_deltas += 1;
        }
    }
    assert_eq!(
        immediate_text_deltas, 1,
        "pacing should enqueue one text delta before yielding"
    );

    while !task.is_finished() {
        tokio::time::advance(std::time::Duration::from_millis(5)).await;
        tokio::task::yield_now().await;
    }
    let ctx = task.await.expect("paced text task");
    assert_eq!(ctx.accumulated_text, expected);
}

#[tokio::test(start_paused = true)]
async fn one_large_reasoning_sse_line_is_paced_across_scheduler_ticks() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let reasoning = "Lập luận khổng lồ cần chảy dần — ".repeat(160);
    let line = format!(
        "data: {}",
        serde_json::json!({
            "choices": [{
                "delta": {"reasoning_content": reasoning},
                "finish_reason": null
            }]
        })
    );
    let expected = reasoning.clone();

    let task = tokio::spawn(async move {
        let builder =
            SseEventBuilder::new("msg_paced_big_reasoning".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = empty_messages_request();
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
        ctx
    });

    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "one giant provider reasoning event was enqueued in a single scheduler burst"
    );

    let mut immediate_thinking_deltas = 0usize;
    while let Ok(event) = rx.try_recv() {
        if format!("{event:?}").contains("thinking_delta") {
            immediate_thinking_deltas += 1;
        }
    }
    assert_eq!(
        immediate_thinking_deltas, 1,
        "pacing should enqueue one thinking delta before yielding"
    );

    while !task.is_finished() {
        tokio::time::advance(std::time::Duration::from_millis(5)).await;
        tokio::task::yield_now().await;
    }
    let ctx = task.await.expect("paced reasoning task");
    assert_eq!(ctx.accumulated_thinking, expected);
}
