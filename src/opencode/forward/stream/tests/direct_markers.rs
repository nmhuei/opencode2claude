use super::*;

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
