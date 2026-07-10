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
    let text = message_text_parts(message).collect::<String>();
    text.trim()
        .strip_prefix('!')
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn local_shell_result(messages: &[Message]) -> Option<String> {
    let message = messages.last().filter(|message| message.role == "user")?;
    let ContentVal::Multiple(blocks) = &message.content else {
        return None;
    };

    blocks.iter().find_map(|block| {
        (block.content_type == "tool_result"
            && block.tool_use_id.as_deref() == Some("toolu_local_shell"))
        .then(|| {
            block
                .content
                .as_ref()
                .map(crate::opencode::mapper::tool_result_content_to_string)
                .unwrap_or_default()
        })
    })
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
