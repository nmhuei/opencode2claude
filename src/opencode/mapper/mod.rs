//! Anthropic-to-OpenAI request mapping, split into policy, helpers, and conversion.

mod helpers;
mod policy;
mod request;

pub use helpers::{
    extract_search_query, extract_system_prompt, is_bridge_search_tool, is_web_search_tool,
    map_model_name, tool_result_content_to_string,
};
pub use policy::{is_compact_request, is_deepseek_v4_model};
pub use request::{map_anthropic_to_openai, map_anthropic_to_openai_with_policy};

#[cfg(test)]
mod tests;
