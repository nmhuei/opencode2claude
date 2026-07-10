
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
