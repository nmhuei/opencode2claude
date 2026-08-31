use super::*;

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
