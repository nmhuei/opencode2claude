use super::*;

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
