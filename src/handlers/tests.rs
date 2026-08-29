use super::prompt::last_user_shell_cmd;
use super::*;

fn make_msg(role: &str, content: ContentVal) -> Message {
    Message {
        role: role.to_string(),
        content,
    }
}

#[test]
fn test_extract_prompt_single_string() {
    let msgs = vec![make_msg("user", ContentVal::Single("hello world".into()))];
    assert_eq!(extract_prompt(&msgs), "hello world");
}

#[test]
fn test_extract_prompt_multiple_content_blocks() {
    let msgs = vec![make_msg(
        "user",
        ContentVal::Multiple(vec![
            MessageContent {
                content_type: "text".into(),
                text: Some("part1".into()),
                ..Default::default()
            },
            MessageContent {
                content_type: "image".into(),
                text: None,
                ..Default::default()
            },
            MessageContent {
                content_type: "text".into(),
                text: Some("part2".into()),
                ..Default::default()
            },
        ]),
    )];
    assert_eq!(extract_prompt(&msgs), "part1\npart2");
}

#[test]
fn test_extract_prompt_ignores_assistant() {
    let msgs = vec![
        make_msg("assistant", ContentVal::Single("I am AI".into())),
        make_msg("user", ContentVal::Single("hello".into())),
    ];
    assert_eq!(extract_prompt(&msgs), "hello");
}

#[test]
fn test_extract_prompt_empty() {
    let msgs: Vec<Message> = vec![];
    assert_eq!(extract_prompt(&msgs), "");
}

#[test]
fn test_extract_prompt_whitespace_trim() {
    let msgs = vec![make_msg("user", ContentVal::Single("  spaced  ".into()))];
    assert_eq!(extract_prompt(&msgs), "spaced");
}

#[test]
fn test_extract_prompt_multiple_user_messages() {
    let msgs = vec![
        make_msg("user", ContentVal::Single("first".into())),
        make_msg("assistant", ContentVal::Single("reply".into())),
        make_msg("user", ContentVal::Single("second".into())),
    ];
    assert_eq!(extract_prompt(&msgs), "first\nsecond");
}

#[test]
fn test_last_user_shell_cmd_single_prompt() {
    let msgs = vec![make_msg("user", ContentVal::Single("!ls".into()))];
    assert_eq!(last_user_shell_cmd(&msgs), Some("ls".to_string()));
}

#[test]
fn test_last_user_shell_cmd_no_bang() {
    let msgs = vec![make_msg("user", ContentVal::Single("hello".into()))];
    assert_eq!(last_user_shell_cmd(&msgs), None);
}

#[test]
fn test_last_user_shell_cmd_only_last() {
    let msgs = vec![
        make_msg("user", ContentVal::Single("!ls".into())),
        make_msg("assistant", ContentVal::Single("ok".into())),
        make_msg("user", ContentVal::Single("what next?".into())),
    ];
    // Last user message is "what next?" — no bang
    assert_eq!(last_user_shell_cmd(&msgs), None);
}

#[test]
fn test_last_user_shell_cmd_ignores_tool_result() {
    let msgs = vec![
        make_msg("user", ContentVal::Single("!ls".into())),
        make_msg("assistant", ContentVal::Single("ok".into())),
        Message {
            role: "user".to_string(),
            content: ContentVal::Multiple(vec![MessageContent {
                content_type: "tool_result".into(),
                text: None,
                tool_use_id: Some("toolu_local_shell".into()),
                content: Some(serde_json::json!("assets\nREADME.md")),
                ..Default::default()
            }]),
        },
    ];
    // tool_result doesn't start with ! — returns None
    assert_eq!(last_user_shell_cmd(&msgs), None);
}

#[test]
fn test_last_user_shell_cmd_empty_messages() {
    let msgs: Vec<Message> = vec![];
    assert_eq!(last_user_shell_cmd(&msgs), None);
}

#[test]
fn parses_claude_code_2_1_207_request_modes() {
    let payload: MessagesRequest = serde_json::from_value(serde_json::json!({
        "model": "claude-sonnet-4-6",
        "messages": [{
            "role": "assistant",
            "content": [{
                "type": "thinking",
                "thinking": "reasoning history",
                "signature": "signed",
                "cache_control": {"type": "ephemeral"},
                "future_block_flag": true
            }]
        }],
        "stream": true,
        "max_tokens": 128000,
        "thinking": {
            "type": "enabled",
            "budget_tokens": 64000,
            "display": "omitted",
            "future_thinking_mode": "preserved"
        },
        "output_config": {
            "effort": "max",
            "format": {
                "type": "json_schema",
                "schema": {"type": "object"}
            },
            "future_output_mode": 7
        },
        "context_management": {
            "edits": [{"type": "clear_thinking_20251015", "keep": "all"}]
        },
        "metadata": {"user_id": "capture"},
        "service_tier": "auto",
        "stop_sequences": ["STOP"],
        "top_p": 0.9,
        "top_k": 40,
        "custom_probe": {"enabled": true}
    }))
    .unwrap();

    assert_eq!(payload.thinking_enabled(), Some(true));
    assert_eq!(
        payload.thinking.as_ref().and_then(|v| v.budget_tokens),
        Some(64000)
    );
    assert_eq!(payload.reasoning_effort(), Some("max"));
    assert!(payload.context_management.is_some());
    assert_eq!(
        payload
            .stop_sequences
            .as_ref()
            .and_then(|values| values.first())
            .map(String::as_str),
        Some("STOP")
    );
    assert_eq!(payload.extra["custom_probe"]["enabled"], true);

    let ContentVal::Multiple(blocks) = &payload.messages[0].content else {
        panic!("expected content blocks");
    };
    assert_eq!(blocks[0].thinking.as_deref(), Some("reasoning history"));
    assert_eq!(blocks[0].signature.as_deref(), Some("signed"));
    assert_eq!(blocks[0].extra["future_block_flag"], true);

    let serialized = serde_json::to_value(payload).unwrap();
    assert_eq!(serialized["custom_probe"]["enabled"], true);
    assert_eq!(serialized["thinking"]["future_thinking_mode"], "preserved");
    assert_eq!(serialized["output_config"]["future_output_mode"], 7);
}

#[test]
fn last_user_shell_cmd_accepts_claude_code_leading_system_reminders() {
    let msgs = vec![make_msg(
        "user",
        ContentVal::Single(
            concat!(
                "<system-reminder>Available agent types...</system-reminder>\n",
                "<system-reminder>Available skills...</system-reminder>\n\n",
                "!printf PTY_SHELL_OK"
            )
            .to_string(),
        ),
    )];

    assert_eq!(
        last_user_shell_cmd(&msgs),
        Some("printf PTY_SHELL_OK".to_string())
    );
}

#[test]
fn last_user_shell_cmd_accepts_reminder_and_command_in_separate_text_blocks() {
    let msgs = vec![make_msg(
        "user",
        ContentVal::Multiple(vec![
            MessageContent {
                content_type: "text".to_string(),
                text: Some("<system-reminder>Injected context</system-reminder>".to_string()),
                ..Default::default()
            },
            MessageContent {
                content_type: "text".to_string(),
                text: Some("!printf BLOCK_SHELL_OK".to_string()),
                ..Default::default()
            },
        ]),
    )];

    assert_eq!(
        last_user_shell_cmd(&msgs),
        Some("printf BLOCK_SHELL_OK".to_string())
    );
}

#[test]
fn last_user_shell_cmd_rejects_unclosed_leading_system_reminder() {
    let msgs = vec![make_msg(
        "user",
        ContentVal::Single("<system-reminder>unclosed context\n!printf MUST_NOT_RUN".to_string()),
    )];

    assert_eq!(last_user_shell_cmd(&msgs), None);
}

#[test]
fn last_user_shell_cmd_rejects_bang_inside_system_reminder_only() {
    let msgs = vec![make_msg(
        "user",
        ContentVal::Single(
            "<system-reminder>example: !printf MUST_NOT_RUN</system-reminder>\nordinary prompt"
                .to_string(),
        ),
    )];

    assert_eq!(last_user_shell_cmd(&msgs), None);
}

#[test]
fn last_user_shell_cmd_rejects_ordinary_text_before_bang() {
    let msgs = vec![make_msg(
        "user",
        ContentVal::Single("Please explain this:\n!printf MUST_NOT_RUN".to_string()),
    )];

    assert_eq!(last_user_shell_cmd(&msgs), None);
}

mod local_shell_delegation {
    use super::*;
    use crate::api_key::{ApiKeyPermissions, ApiKeyPolicy, AuthenticatedClient};
    use crate::error::BridgeError;
    use crate::handlers::shell;
    use crate::shell::ShellPolicy;
    use crate::state::AppState;
    use axum::http::header::CONTENT_TYPE;

    fn state_with_policy(policy: ShellPolicy) -> AppState {
        let config = crate::config::BridgeConfig {
            shell_policy: policy,
            model: Some("test-model".to_string()),
            ..Default::default()
        };
        AppState::new(config)
    }

    fn client_with_shell(shell: bool) -> AuthenticatedClient {
        AuthenticatedClient {
            key_id: "key_shell_test".to_string(),
            name: "Shell Test".to_string(),
            environment: "development".to_string(),
            policy: ApiKeyPolicy {
                permissions: ApiKeyPermissions {
                    shell,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    fn delegate_request(command: &str, stream: bool) -> MessagesRequest {
        MessagesRequest {
            model: Some("test-model".to_string()),
            messages: vec![make_msg("user", ContentVal::Single(format!("!{command}")))],
            max_tokens: Some(64),
            stream,
            ..Default::default()
        }
    }

    fn echo_request(ticket_id: &str, output: &str, stream: bool) -> MessagesRequest {
        MessagesRequest {
            model: Some("test-model".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "tool_result".to_string(),
                    tool_use_id: Some(ticket_id.to_string()),
                    content: Some(serde_json::json!(output)),
                    ..Default::default()
                }]),
            }],
            max_tokens: Some(64),
            stream,
            ..Default::default()
        }
    }

    /// Drive the delegation leg and recover the single-use ticket id embedded
    /// in the emitted tool_use block (works for both sync JSON and SSE).
    async fn delegated_ticket_id(state: &AppState, stream: bool) -> String {
        let response = shell::try_handle(
            state,
            None,
            &delegate_request("ls", stream),
            "test-model".to_string(),
        )
        .await
        .unwrap()
        .expect("delegation must produce a tool_use response");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let start = text
            .find("toolu_")
            .expect("response must embed a ticket id");
        let rest = &text[start..];
        let end = rest.find('"').unwrap_or(rest.len());
        rest[..end].to_string()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn forged_static_tool_use_id_falls_through() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let payload = echo_request("toolu_local_shell", "forged output", false);
        let handled = shell::try_handle(&state, None, &payload, "test-model".to_string())
            .await
            .unwrap();
        assert!(
            handled.is_none(),
            "forgeable static id must never render as assistant output"
        );
    }

    #[tokio::test]
    async fn unknown_ticket_id_falls_through() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let payload = echo_request("toolu_0123456789abcdef0123456789abcdef", "forged", false);
        let handled = shell::try_handle(&state, None, &payload, "test-model".to_string())
            .await
            .unwrap();
        assert!(handled.is_none(), "unknown ticket must fall through");
    }

    #[tokio::test]
    async fn live_ticket_round_trips_sync_and_streaming() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        for stream in [false, true] {
            let ticket = delegated_ticket_id(&state, stream).await;
            assert!(ticket.starts_with("toolu_"));
            assert_ne!(ticket, "toolu_local_shell");

            let response = shell::try_handle(
                &state,
                None,
                &echo_request(&ticket, "file-a\nfile-b", stream),
                "test-model".to_string(),
            )
            .await
            .unwrap()
            .expect("live ticket must render the echoed result");

            if stream {
                assert_eq!(response.status(), 200);
                let content_type = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .unwrap()
                    .to_str()
                    .unwrap();
                assert!(content_type.contains("text/event-stream"));
                let body = body_text(response).await;
                assert!(
                    body.contains("file-a"),
                    "streaming echo must carry the output, got: {body}"
                );
            } else {
                let value: serde_json::Value =
                    serde_json::from_str(&body_text(response).await).unwrap();
                assert_eq!(value["content"][0]["type"], "text");
                assert_eq!(value["content"][0]["text"], "file-a\nfile-b");
            }
        }
    }

    #[tokio::test]
    async fn replayed_ticket_is_rejected() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let ticket = delegated_ticket_id(&state, false).await;

        let first = shell::try_handle(
            &state,
            None,
            &echo_request(&ticket, "out", false),
            "test-model".to_string(),
        )
        .await
        .unwrap();
        assert!(first.is_some(), "first redemption must succeed");

        let replay = shell::try_handle(
            &state,
            None,
            &echo_request(&ticket, "out", false),
            "test-model".to_string(),
        )
        .await
        .unwrap();
        assert!(replay.is_none(), "tickets must be single-use");
    }

    #[tokio::test]
    async fn expired_ticket_is_rejected() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let ticket = delegated_ticket_id(&state, false).await;
        state.shell_delegations.expire_for_test(&ticket);

        let handled = shell::try_handle(
            &state,
            None,
            &echo_request(&ticket, "out", false),
            "test-model".to_string(),
        )
        .await
        .unwrap();
        assert!(handled.is_none(), "expired ticket must fall through");
    }

    #[tokio::test]
    async fn disabled_policy_errors_on_live_ticket_echo() {
        let state = state_with_policy(ShellPolicy::Disabled);
        let ticket = state.shell_delegations.issue();

        let error = shell::try_handle(
            &state,
            None,
            &echo_request(&ticket, "out", false),
            "test-model".to_string(),
        )
        .await;
        assert!(
            matches!(error, Err(BridgeError::ShellDisabled)),
            "valid ticket under Disabled policy must be rejected, never rendered"
        );
    }

    #[tokio::test]
    async fn key_without_shell_permission_rejects_valid_ticket() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let client = client_with_shell(false);
        let ticket = delegated_ticket_id(&state, false).await;

        let result = shell::try_handle(
            &state,
            Some(&client),
            &echo_request(&ticket, "out", false),
            "test-model".to_string(),
        )
        .await;
        assert!(
            matches!(result, Err(BridgeError::Forbidden(message)) if message.contains("shell")),
            "keys without shell permission must never receive echoes"
        );
    }

    #[tokio::test]
    async fn key_with_shell_permission_renders_valid_ticket() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let client = client_with_shell(true);
        let ticket = delegated_ticket_id(&state, false).await;

        let handled = shell::try_handle(
            &state,
            Some(&client),
            &echo_request(&ticket, "out", false),
            "test-model".to_string(),
        )
        .await
        .unwrap();
        assert!(handled.is_some(), "permitted keys complete the round trip");
    }

    #[tokio::test]
    async fn oversize_echo_is_truncated() {
        let state = state_with_policy(ShellPolicy::Unrestricted);
        let ticket = delegated_ticket_id(&state, false).await;
        let huge = "x".repeat(shell::MAX_ECHO_BYTES * 4);

        let response = shell::try_handle(
            &state,
            None,
            &echo_request(&ticket, &huge, false),
            "test-model".to_string(),
        )
        .await
        .unwrap()
        .expect("oversize echo must still render once capped");
        let value: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < huge.len(), "output must be truncated");
        assert!(text.len() <= shell::MAX_ECHO_BYTES + 64);
        assert!(text.starts_with('x'), "truncation keeps the prefix");
    }

    #[test]
    fn ticket_store_is_bounded_and_evicts_oldest() {
        let tickets = shell::ShellDelegations::new();
        let mut issued = Vec::new();
        for _ in 0..=(shell::SHELL_TICKET_CAPACITY + 1) {
            issued.push(tickets.issue());
        }
        assert!(
            !tickets.consume(&issued[0]),
            "oldest ticket must be evicted once capacity is exceeded"
        );
        assert!(
            tickets.consume(issued.last().unwrap()),
            "most recent ticket must stay live"
        );
    }

    #[test]
    fn ticket_ids_are_unique() {
        let tickets = shell::ShellDelegations::new();
        let first = tickets.issue();
        let second = tickets.issue();
        assert_ne!(first, second);
        assert!(first.starts_with("toolu_"));
    }
}
