//! Anthropic-to-OpenAI request mapping, split into policy, helpers, and conversion.

mod helpers;
mod policy;
mod request;

pub use helpers::{
    extract_search_query, extract_system_prompt, is_web_search_tool, map_model_name,
    tool_result_content_to_string,
};
pub use policy::is_compact_request;
pub use request::map_anthropic_to_openai;

#[cfg(test)]
mod tests;
