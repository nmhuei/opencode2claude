//! Anthropic-compatible HTTP transport.

mod messages;
mod metadata;
mod openai;
mod prompt;
mod shell;
mod title;
mod types;

pub use messages::handle_messages;
pub use metadata::{
    handle_count_tokens, handle_health, handle_liveness, handle_models, handle_readiness,
};
pub use openai::{handle_chat_completions, openai_error_response};
pub use prompt::extract_prompt;
pub(crate) use prompt::strip_leading_system_reminders;
pub use types::{
    AnthropicTool, ContentVal, Message, MessageContent, MessagesRequest, OutputConfig,
    ThinkingConfig,
};

#[cfg(test)]
mod tests;
