use super::*;

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
