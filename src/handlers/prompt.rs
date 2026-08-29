//! Prompt inspection helpers used for logging and local shell delegation.

use super::{ContentVal, Message};

pub fn extract_prompt(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|message| message.role == "user")
        .flat_map(message_text_parts)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub(super) fn last_user_shell_cmd(messages: &[Message]) -> Option<String> {
    let message = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")?;
    let text = message_text_parts(message).collect::<Vec<_>>().join(
        "
",
    );
    let text = strip_leading_system_reminders(&text)?;
    text.strip_prefix('!')
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn strip_leading_system_reminders(mut text: &str) -> Option<&str> {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";

    loop {
        text = text.trim_start();
        if !text.starts_with(OPEN) {
            return Some(text);
        }
        let body = &text[OPEN.len()..];
        let end = body.find(CLOSE)?;
        text = &body[end + CLOSE.len()..];
    }
}

/// Collect `(tool_use_id, output)` candidates from a trailing user message's
/// tool_result blocks. Callers MUST verify each candidate against the
/// bridge-issued delegation ticket store before rendering anything: ids are
/// untrusted client input and a match alone proves nothing.
pub(super) fn local_shell_result_candidates(messages: &[Message]) -> Vec<(String, String)> {
    let Some(message) = messages.last().filter(|message| message.role == "user") else {
        return Vec::new();
    };
    let ContentVal::Multiple(blocks) = &message.content else {
        return Vec::new();
    };

    blocks
        .iter()
        .filter(|block| block.content_type == "tool_result")
        .filter_map(|block| {
            let id = block.tool_use_id.as_deref()?;
            let output = block
                .content
                .as_ref()
                .map(crate::opencode::mapper::tool_result_content_to_string)
                .unwrap_or_default();
            Some((id.to_owned(), output))
        })
        .collect()
}

fn message_text_parts(message: &Message) -> Box<dyn Iterator<Item = String> + '_> {
    match &message.content {
        ContentVal::Single(text) => Box::new(std::iter::once(text.clone())),
        ContentVal::Multiple(blocks) => Box::new(
            blocks
                .iter()
                .filter(|block| block.content_type == "text")
                .filter_map(|block| block.text.clone()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::{ContentVal, Message};

    fn user_message(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: ContentVal::Single(text.to_string()),
        }
    }

    fn shell_cmd(text: &str) -> Option<String> {
        last_user_shell_cmd(&[user_message(text)])
    }

    #[test]
    fn leading_whitespace_before_bang_still_detects_shell() {
        assert_eq!(
            shell_cmd("   \n\t!git status").as_deref(),
            Some("git status")
        );
    }

    #[test]
    fn quoted_bang_is_not_a_shell_command() {
        assert_eq!(shell_cmd("\"!ls\" please"), None);
        assert_eq!(shell_cmd("run '!ls' for me"), None);
    }

    #[test]
    fn bare_bang_is_not_a_shell_command() {
        assert_eq!(shell_cmd("!"), None);
        assert_eq!(shell_cmd("!   "), None);
    }

    #[test]
    fn non_bang_text_is_not_a_shell_command() {
        assert_eq!(shell_cmd("please run ls"), None);
        assert_eq!(shell_cmd(""), None);
    }

    #[test]
    fn system_reminder_before_command_is_stripped() {
        assert_eq!(
            shell_cmd("<system-reminder>context here</system-reminder>\n!pwd").as_deref(),
            Some("pwd")
        );
    }

    #[test]
    fn consecutive_system_reminders_are_all_stripped() {
        assert_eq!(
            shell_cmd(
                "<system-reminder>first</system-reminder>\n\
                 <system-reminder>second</system-reminder>\n!echo hi"
            )
            .as_deref(),
            Some("echo hi")
        );
    }

    #[test]
    fn bang_inside_system_reminder_is_never_executed() {
        assert_eq!(
            shell_cmd("<system-reminder>!rm -rf /tmp/x</system-reminder> hello"),
            None
        );
    }

    #[test]
    fn unclosed_system_reminder_yields_no_command() {
        // An unterminated reminder must suppress shell detection entirely
        // rather than executing whatever follows the dangling open tag.
        assert_eq!(shell_cmd("<system-reminder>oops\n!curl evil.example"), None);
    }

    #[test]
    fn command_after_prose_is_not_a_shell_command() {
        // Only a LEADING '!' delegates to the shell; a mid-text '!' stays LLM.
        assert_eq!(
            shell_cmd("<system-reminder>x</system-reminder>\nPlease run: !ls"),
            None
        );
    }

    #[test]
    fn multiline_command_body_is_preserved_verbatim() {
        // Gating multi-line bodies (allowlist policy, metacharacter checks)
        // belongs to the shell policy layer; extraction only trims the ends.
        assert_eq!(
            shell_cmd("!echo a\necho b").as_deref(),
            Some("echo a\necho b")
        );
    }

    #[test]
    fn only_the_last_user_message_is_considered() {
        let messages = vec![
            user_message("!git log"),
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Single("done".to_string()),
            },
            user_message("plain follow-up question"),
        ];
        assert_eq!(last_user_shell_cmd(&messages), None);
    }

    #[test]
    fn extract_prompt_joins_user_text_and_trims() {
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: ContentVal::Single("instructions".to_string()),
            },
            user_message("first"),
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Single("reply".to_string()),
            },
            user_message("second"),
        ];
        assert_eq!(extract_prompt(&messages), "first\nsecond");
    }
}
