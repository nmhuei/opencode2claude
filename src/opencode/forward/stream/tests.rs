use super::context::{process_openai_sse_line, split_pending_text, StreamContext};
use crate::handlers::{AnthropicTool, MessagesRequest};
use crate::opencode::forward::common::get_correct_tool_name;
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
        }]),
        tool_choice: None,
        stream: false,
        temperature: None,
        max_tokens: Some(100),
    };
    assert_eq!(get_correct_tool_name("skill", &req), "Skill");
    assert_eq!(get_correct_tool_name("Skill", &req), "Skill");
    assert_eq!(get_correct_tool_name("other", &req), "other");
}
