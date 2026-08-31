use super::*;

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

#[tokio::test(start_paused = true)]
async fn stalled_receiver_bounds_context_emit_blocking_instead_of_parking_forever() {
    // transport::send_sse bounds every executor-side emit to a 5s window so a
    // slow-but-alive SSE consumer can never wedge the spawned stream task. The
    // context-layer emit helpers must honor the same bound: with a full
    // capacity-1 channel whose receiver stays alive but is never polled,
    // processing a text delta has to give up within the bounded send window
    // instead of parking the task (and the upstream connection) forever.
    let (tx, _rx_alive_but_never_polled) = tokio::sync::mpsc::channel(1);
    let builder = SseEventBuilder::new("msg_stall".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();

    // Capacity 1: content_block_start fills the buffer slot instantly, so the
    // following text_delta send hits the backpressure boundary.
    let line = r#"data: {"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#;
    let done = tokio::time::timeout(
        std::time::Duration::from_secs(7),
        process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload),
    )
    .await;

    assert!(
        done.is_ok(),
        "process_openai_sse_line parked on a stalled consumer; context emits must be time-bounded like executor emits"
    );
}

#[tokio::test(start_paused = true)]
async fn failed_send_marks_context_and_halts_further_processing() {
    // Once a bounded send has failed (stalled consumer or closed receiver),
    // continuing to emit individual events would drop block-bearing pieces
    // and desynchronize content_block_start/stop pairing. The context must
    // record the failure, refuse further processing, and let the executor
    // tear the response down instead of drip-feeding a dead connection.
    let (tx, _rx_alive_but_never_polled) = tokio::sync::mpsc::channel(1);
    let builder = SseEventBuilder::new("msg_dead_sink".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();

    // Capacity 1: content_block_start fills the free slot; the text delta
    // send hits backpressure and times out inside the bounded window.
    let first = r#"data: {"choices":[{"delta":{"content":"one"},"finish_reason":null}]}"#;
    let done_first =
        process_openai_sse_line(first, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert!(
        ctx.send_failed,
        "a timed-out send must be recorded as transport death"
    );
    assert!(!done_first);
    assert_eq!(ctx.accumulated_text, "one");

    // Any later upstream line must be refused outright: nothing further may
    // be parsed, emitted, or accumulated for a dead consumer.
    let second = r#"data: {"choices":[{"delta":{"content":"two"},"finish_reason":null}]}"#;
    let done_second =
        process_openai_sse_line(second, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

    assert!(
        done_second,
        "processing must report completion so the executor tears the stream down"
    );
    assert_eq!(
        ctx.accumulated_text, "one",
        "no further content may accumulate after a failed send"
    );
}

// ── Accumulator caps (bounded retention) ─────────────────────────────────

#[test]
fn push_bounded_fills_exactly_to_cap_then_stops() {
    let mut buf = String::new();
    push_bounded(&mut buf, "abcdefgh", 8);
    assert_eq!(buf, "abcdefgh");
    assert_eq!(buf.len(), 8);

    // Exactly at the boundary: further appends are no-ops, never panics.
    push_bounded(&mut buf, "x", 8);
    push_bounded(&mut buf, "yz", 8);
    assert_eq!(buf.len(), 8);
    assert_eq!(buf, "abcdefgh");
}

#[test]
fn push_bounded_never_splits_multibyte_chars() {
    // '€' is 3 bytes; a cap of 7 fits only two of "€€€" (9 bytes).
    let mut buf = String::new();
    push_bounded(&mut buf, "€€€", 7);
    assert_eq!(buf, "€€");
    assert_eq!(buf.len(), 6);

    // Remaining room smaller than the multibyte char drops the whole char.
    let mut tight = String::new();
    push_bounded(&mut tight, "a€b", 2);
    assert_eq!(tight, "a");

    // Zero-cap buffer never grows and stays valid UTF-8.
    let mut zero = String::new();
    push_bounded(&mut zero, "data", 0);
    assert!(zero.is_empty());
}

#[tokio::test(start_paused = true)]
async fn visible_text_accumulator_caps_but_streaming_stays_complete() {
    // Feeding more than MAX_ACCUMULATOR_BYTES of visible text must keep every
    // byte on the wire while retaining at most the cap for retry decisions,
    // search synthesis, and usage estimation. Feed chunks misaligned with the
    // render-fragment size so the crossing chunk exercises truncation.
    // Capacity comfortably above the fragment count so no send ever blocks;
    // everything is drained after feeding.
    let (tx, mut rx) = tokio::sync::mpsc::channel(16384);
    let builder = SseEventBuilder::new("msg_cap_text".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();

    let total_text = format!("{}TAILMARK", "a".repeat(MAX_ACCUMULATOR_BYTES));
    for chunk in total_text.as_bytes().chunks(1000) {
        let line = format!(
            r#"data: {{"choices":[{{"delta":{{"content":"{}"}},"finish_reason":null}}]}}"#,
            String::from_utf8(chunk.to_vec()).unwrap(),
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    drop(tx);

    assert_eq!(
        ctx.accumulated_text.len(),
        MAX_ACCUMULATOR_BYTES,
        "the retained text must stop at exactly the cap"
    );

    // Streaming is untouched by the retention cap: every fragment reaches
    // the channel, including the tail marker past the cap boundary.
    let mut deltas = 0_usize;
    let mut saw_tail = false;
    while let Ok(event) = rx.try_recv() {
        let rendered = format!("{event:?}");
        if rendered.contains("text_delta") {
            deltas += 1;
        }
        if rendered.contains("TAILMARK") {
            saw_tail = true;
        }
    }
    assert!(saw_tail, "the final bytes must still be streamed");
    // Each upstream feed chunk is split into 256-byte render fragments, and
    // its short tail becomes one padded fragment of its own.
    let expected_fragments: usize = total_text
        .as_bytes()
        .chunks(1000)
        .map(|chunk| chunk.len().div_ceil(256))
        .sum();
    assert_eq!(
        deltas, expected_fragments,
        "every render fragment must be emitted regardless of the cap"
    );
}

#[tokio::test(start_paused = true)]
async fn thinking_accumulator_caps_but_streaming_stays_complete() {
    // Capacity comfortably above the fragment count so no send ever blocks;
    // everything is drained after feeding.
    let (tx, mut rx) = tokio::sync::mpsc::channel(16384);
    let builder = SseEventBuilder::new("msg_cap_think".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new(false);
    ctx.message_started = true;
    let payload = empty_messages_request();

    let total_text = format!("{}TAILMARK", "r".repeat(MAX_ACCUMULATOR_BYTES));
    for chunk in total_text.as_bytes().chunks(1000) {
        let line = format!(
            r#"data: {{"choices":[{{"delta":{{"reasoning_content":"{}"}},"finish_reason":null}}]}}"#,
            String::from_utf8(chunk.to_vec()).unwrap(),
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }
    drop(tx);

    assert_eq!(
        ctx.accumulated_thinking.len(),
        MAX_ACCUMULATOR_BYTES,
        "the retained reasoning must stop at exactly the cap"
    );

    let mut deltas = 0_usize;
    let mut saw_tail = false;
    while let Ok(event) = rx.try_recv() {
        let rendered = format!("{event:?}");
        if rendered.contains("thinking_delta") {
            deltas += 1;
        }
        if rendered.contains("TAILMARK") {
            saw_tail = true;
        }
    }
    assert!(saw_tail, "the final reasoning bytes must still be streamed");
    // Each upstream feed chunk is split into 256-byte render fragments, and
    // its short tail becomes one padded fragment of its own.
    let expected_fragments: usize = total_text
        .as_bytes()
        .chunks(1000)
        .map(|chunk| chunk.len().div_ceil(256))
        .sum();
    assert_eq!(
        deltas, expected_fragments,
        "every render fragment must be emitted regardless of the cap"
    );
}

fn bash_tool_payload() -> MessagesRequest {
    MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "execute a shell command".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    }
}

fn feed_reasoning(line_text: &str) -> String {
    format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"reasoning_content": line_text}, "finish_reason": null}]
        })
    )
}

fn feed_text(line_text: &str) -> String {
    format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": line_text}, "finish_reason": null}]
        })
    )
}

/// A hostile upstream can stream unbounded native tool-call argument fragments
/// before `finish_reason` ever arrives. Retention must be capped per call, and
/// a truncated call must degrade to the existing invalid-JSON recovery (clean
/// retry when nothing was emitted), never execute partial arguments.
#[tokio::test]
async fn native_tool_argument_retention_is_bounded() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_bound".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new_with_encoded_fallback(false, true);
    ctx.message_started = true;
    let payload = bash_tool_payload();

    let fragment_len = 64 * 1024;
    for i in 0..200_usize {
        // Distinct non-cumulative fragments so every merge appends.
        let arguments = format!("{i:08}{}", "A".repeat(fragment_len));
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"tool_calls": [{
                    "index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "Bash", "arguments": arguments}
                }]}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }

    let retained = ctx.test_native_arguments_bytes();
    assert!(
        retained <= MAX_NATIVE_TOOL_ARGUMENT_BYTES + fragment_len + 16,
        "native argument retention {retained} exceeded the per-call bound"
    );

    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    assert!(
        ctx.compat_retry_requested,
        "truncated native arguments must request a clean retry instead of executing"
    );
    assert!(
        !ctx.has_emitted_tool_use,
        "truncated native arguments must never open a tool_use block"
    );
    while let Ok(event) = rx.try_recv() {
        let rendered = format!("{event:?}");
        assert!(
            !rendered.contains("tool_use"),
            "no tool_use events may reach the client for truncated arguments: {rendered}"
        );
    }
}

/// The global pending budget must also bound the entry-count attack: many
/// distinct indices each carrying modest fragments cannot balloon memory.
#[tokio::test]
async fn native_pending_budget_bounds_entry_count_attack() {
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let builder = SseEventBuilder::new("msg_bound_many".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new_with_encoded_fallback(false, true);
    ctx.message_started = true;
    let payload = bash_tool_payload();

    let fragment_len = 32 * 1024;
    for i in 0..512_usize {
        let arguments = format!("{i:06}{}", "B".repeat(fragment_len));
        let line = format!(
            "data: {}",
            serde_json::json!({
                "choices": [{"delta": {"tool_calls": [{
                    "index": i, "id": format!("call_{i}"), "type": "function",
                    "function": {"name": "Bash", "arguments": arguments}
                }]}, "finish_reason": null}]
            })
        );
        process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;
    }

    let retained = ctx.test_native_arguments_bytes();
    assert!(
        retained <= MAX_NATIVE_PENDING_BYTES + fragment_len + 16,
        "global native retention {retained} exceeded the pending budget"
    );
}

/// While encoded execution is deferred behind native finalization, a parsed
/// compat marker parks at position zero and every later chunk appends to the
/// retained buffer. That parked state previously grew without any bound; it
/// must hit the same oversized-discard treatment as malformed markers, and
/// the discarded marker must neither execute nor trigger a replay.
#[tokio::test]
async fn oversized_parked_compat_marker_is_discarded_under_native_deferral() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_parked".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    let mut ctx = StreamContext::new_with_encoded_fallback(false, true);
    ctx.message_started = true;
    let payload = bash_tool_payload();

    let marker = "[Requesting Bash with arguments: {\"command\":\"ls\"}]";
    process_openai_sse_line(
        &feed_reasoning(marker),
        &mut ctx,
        &mut tracker,
        &tx,
        &builder,
        &payload,
    )
    .await;
    for _ in 0..8 {
        let filler = "x".repeat(16 * 1024);
        process_openai_sse_line(
            &feed_reasoning(&filler),
            &mut ctx,
            &mut tracker,
            &tx,
            &builder,
            &payload,
        )
        .await;
    }

    let retained = ctx.test_reasoning_buffer_bytes();
    assert!(
        retained <= MAX_COMPAT_TOOL_BUFFER_SIZE + 16 * 1024 + 16,
        "parked marker buffer retained {retained} bytes with no bound"
    );
    assert!(
        ctx.test_discarding_reasoning(),
        "oversized parked marker must enter fail-closed discard mode"
    );

    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    assert!(
        !ctx.has_emitted_tool_use,
        "discarded oversized marker must never execute as a tool call"
    );
    assert!(
        !ctx.compat_retry_requested,
        "oversized discard follows the established no-replay semantics"
    );

    let mut placeholders = 0_usize;
    while let Ok(event) = rx.try_recv() {
        if format!("{event:?}").contains("[Oversized tool request omitted]") {
            placeholders += 1;
        }
    }
    assert_eq!(placeholders, 1, "exactly one placeholder must be emitted");
}

/// A safe execution preamble followed by an incomplete marker is buffered
/// whole until the candidate completes or EOF decides. With parser
/// activation still strict (first attempt), that hold previously grew
/// unbounded AND ended in a full-attempt replay at EOF. It must instead hit
/// the oversized discard bound without requesting a replay storm.
#[tokio::test]
async fn preamble_hold_overflow_is_bounded_without_replay_storm() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let builder = SseEventBuilder::new("msg_preamble".to_string(), "model".to_string());
    let mut tracker = SseBlockTracker::new();
    // Strict first-attempt gate: encoded_parser_activated = false, exactly
    // like the production constructor on the first try.
    let mut ctx = StreamContext::new_with_encoded_fallback(false, false);
    ctx.message_started = true;
    let payload = bash_tool_payload();

    let preamble = "I'll use the Bash tool.\n";
    let opener = "[Requesting Bash with arguments: {\"cmd\":\"";
    process_openai_sse_line(
        &feed_text(&(preamble.to_string() + opener)),
        &mut ctx,
        &mut tracker,
        &tx,
        &builder,
        &payload,
    )
    .await;
    for _ in 0..8 {
        let filler = "y".repeat(16 * 1024);
        process_openai_sse_line(
            &feed_text(&filler),
            &mut ctx,
            &mut tracker,
            &tx,
            &builder,
            &payload,
        )
        .await;
    }

    let retained = ctx.test_text_buffer_bytes();
    assert!(
        retained <= MAX_COMPAT_TOOL_BUFFER_SIZE + 16 * 1024 + 16,
        "preamble-held buffer retained {retained} bytes with no bound"
    );
    assert!(
        ctx.test_discarding_text(),
        "oversized preamble hold must enter fail-closed discard mode"
    );

    ctx.flush_remaining(&mut tracker, &tx, &builder, &payload)
        .await;
    assert!(
        !ctx.compat_retry_requested,
        "bounded discard must not escalate into a whole-attempt replay"
    );
    assert!(!ctx.has_emitted_tool_use);

    let mut placeholders = 0_usize;
    while let Ok(event) = rx.try_recv() {
        if format!("{event:?}").contains("[Oversized tool request omitted]") {
            placeholders += 1;
        }
    }
    assert_eq!(placeholders, 1, "exactly one placeholder must be emitted");
}
