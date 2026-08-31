//! Heuristic token estimation for usage accounting.

use crate::handlers::{ContentVal, MessagesRequest};

pub fn estimate_string_tokens(text: &str) -> u32 {
    let mut tokens: f32 = 0.0;
    let mut in_word = false;

    for c in text.chars() {
        if c.is_whitespace() {
            tokens += 0.25;
            in_word = false;
        } else if c.is_ascii_alphanumeric() {
            if !in_word {
                tokens += 1.0;
                in_word = true;
            } else {
                tokens += 0.22;
            }
        } else {
            tokens += 0.5;
            in_word = false;
        }
    }
    tokens.round() as u32
}

pub fn estimate_input_tokens(payload: &MessagesRequest) -> u32 {
    let mut total_tokens = 0;
    if let Some(ref sys) = payload.system {
        total_tokens += estimate_string_tokens(&sys.to_string());
    }
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => total_tokens += estimate_string_tokens(text),
            ContentVal::Multiple(blocks) => {
                for b in blocks {
                    if let Some(ref text) = b.text {
                        total_tokens += estimate_string_tokens(text);
                    }
                    if let Some(ref input) = b.input {
                        total_tokens += estimate_string_tokens(&input.to_string());
                    }
                    if let Some(ref content) = b.content {
                        total_tokens += estimate_string_tokens(&content.to_string());
                    }
                }
            }
        }
    }
    if total_tokens == 0 {
        100
    } else {
        total_tokens
    }
}
