use super::*;

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
