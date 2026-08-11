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
