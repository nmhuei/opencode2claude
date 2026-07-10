//! Anthropic-compatible HTTP transport.

mod messages;
mod metadata;
mod prompt;
mod shell;
mod types;

pub use messages::handle_messages;
pub use metadata::{handle_count_tokens, handle_health, handle_models};
pub use prompt::extract_prompt;
pub use types::{AnthropicTool, ContentVal, Message, MessageContent, MessagesRequest};

#[cfg(test)]
mod tests;
