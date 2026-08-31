use super::*;

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
