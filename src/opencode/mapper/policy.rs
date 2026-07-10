//! Model- and request-shape policy for upstream request mapping.

use super::helpers::extract_system_prompt;
use crate::handlers::{ContentVal, MessagesRequest};

pub(super) const DEFAULT_MIN_REASONING_STREAM_TOKENS: u32 = 1024;

pub(super) fn is_reasoning_heavy_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    (name.contains("deepseek") && (name.contains("r1") || name.contains("reasoner")))
        || name.contains("reasoning")
        || name.contains("-r1")
}

pub(super) fn min_reasoning_stream_tokens() -> u32 {
    std::env::var("BRIDGE_MIN_REASONING_STREAM_TOKENS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MIN_REASONING_STREAM_TOKENS)
}

pub fn is_compact_request(payload: &MessagesRequest) -> bool {
    if let Some(system_val) = &payload.system {
        let system_str = extract_system_prompt(system_val).to_lowercase();
        if system_str.contains("compact") || system_str.contains("summari") {
            return true;
        }
    }
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => {
                let text_lower = text.to_lowercase();
                if text_lower.contains("compact") || text_lower.contains("summari") {
                    return true;
                }
            }
            ContentVal::Multiple(blocks) => {
                for block in blocks {
                    if block.content_type == "text" {
                        if let Some(text) = &block.text {
                            let text_lower = text.to_lowercase();
                            if text_lower.contains("compact") || text_lower.contains("summari") {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

pub(super) fn include_reasoning_for_stream(
    stream: bool,
    mapped_model: &str,
    is_compact: bool,
) -> Option<bool> {
    if is_compact {
        return None;
    }
    if stream && is_reasoning_heavy_model(mapped_model) {
        Some(true)
    } else {
        None
    }
}

pub(super) fn normalize_upstream_max_tokens(
    requested: Option<u32>,
    stream: bool,
    mapped_model: &str,
    is_compact: bool,
) -> Option<u32> {
    if is_compact {
        return requested;
    }
    if !stream || !is_reasoning_heavy_model(mapped_model) {
        return requested;
    }

    let floor = min_reasoning_stream_tokens();
    Some(requested.map(|v| v.max(floor)).unwrap_or(floor))
}
