//! Sanitization utilities for LLM output cleaning.
//!
//! Provides functions to strip system leakage tags from model responses.
//! Extracted from `forward.rs` during module split.

/// Strip system leakage tags (like `</think>`, `</parameter>`, etc.) from LLM outputs.
///
/// Removes known tags that models sometimes leak from their system prompt context,
/// including HTML-encoded variants. Also trims leading whitespace when tags were
/// stripped from the beginning of the text.
pub fn strip_system_tags(text: &str) -> String {
    let mut cleaned = text.to_string();
    let tags = [
        "</think>",
        "<think>",
        "</parameter>",
        "<parameter>",
        "</｜DSML｜parameter>",
        "<｜DSML｜parameter>",
        "&lt;/think&gt;",
        "&lt;think&gt;",
    ];
    for tag in &tags {
        if cleaned.contains(tag) {
            cleaned = cleaned.replace(tag, "");
        }
    }
    // Trim leading newlines and whitespace if we stripped tags from the beginning
    if cleaned.trim_start() != text.trim_start() {
        cleaned = cleaned.trim_start().to_string();
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_system_tags() {
        assert_eq!(strip_system_tags("</think>Hello"), "Hello");
        assert_eq!(strip_system_tags("</think>\n\nHello"), "Hello");
        assert_eq!(strip_system_tags("Hello</think>"), "Hello");
        assert_eq!(strip_system_tags("Hello</parameter>World"), "HelloWorld");
        assert_eq!(strip_system_tags("</｜DSML｜parameter>\nHello"), "Hello");
        assert_eq!(
            strip_system_tags("<think>Some thinking</think>Response"),
            "Some thinkingResponse"
        );
        assert_eq!(strip_system_tags("Normal text"), "Normal text");
    }
}
