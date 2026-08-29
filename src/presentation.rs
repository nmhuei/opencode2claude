//! Width-aware primitives for the line-oriented CLI presentation layer.
//!
//! The CLI deliberately avoids a full-screen TUI. These helpers keep regular
//! command output aligned, responsive, Unicode-safe, and consistent across
//! terminals without relying on hand-written box borders.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const DEFAULT_TERMINAL_WIDTH: usize = 100;
pub const MIN_TERMINAL_WIDTH: usize = 40;
pub const MAX_TERMINAL_WIDTH: usize = 240;
pub const MAX_CONTENT_WIDTH: usize = 96;
pub const INDENT: usize = 2;
pub const LABEL_GAP: usize = 3;
pub const BRAND_SYMBOL: &str = "◆";

pub fn terminal_width() -> usize {
    crate::config::terminal_columns()
        .filter(|width| (MIN_TERMINAL_WIDTH..=MAX_TERMINAL_WIDTH).contains(width))
        .unwrap_or(DEFAULT_TERMINAL_WIDTH)
}

pub fn content_width() -> usize {
    terminal_width()
        .saturating_sub(INDENT)
        .clamp(MIN_TERMINAL_WIDTH.saturating_sub(INDENT), MAX_CONTENT_WIDTH)
}

pub fn compact() -> bool {
    content_width() < 68
}

pub fn rule(character: char) -> String {
    character.to_string().repeat(content_width())
}

pub fn truncate(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let target = max_width - 1;
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let width = character.width().unwrap_or(0);
        if used + width > target {
            break;
        }
        result.push(character);
        used += width;
    }
    result.push('…');
    result
}

pub fn wrap(value: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    let mut lines = Vec::new();

    for paragraph in value.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split_whitespace() {
            let word_width = UnicodeWidthStr::width(word);
            let separator = usize::from(!line.is_empty());
            if !line.is_empty() && line_width + separator + word_width > max_width {
                lines.push(line);
                line = String::new();
                line_width = 0;
            }

            if word_width > max_width {
                let mut chunk = String::new();
                let mut chunk_width = 0;
                for character in word.chars() {
                    let width = character.width().unwrap_or(0);
                    if chunk_width + width > max_width && !chunk.is_empty() {
                        lines.push(chunk);
                        chunk = String::new();
                        chunk_width = 0;
                    }
                    chunk.push(character);
                    chunk_width += width;
                }
                line = chunk;
                line_width = chunk_width;
                continue;
            }

            if !line.is_empty() {
                line.push(' ');
                line_width += 1;
            }
            line.push_str(word);
            line_width += word_width;
        }

        if !line.is_empty() {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Render a borderless list of label/value facts.
pub fn facts(rows: &[(&str, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let available = content_width().saturating_sub(INDENT);
    if compact() {
        return rows
            .iter()
            .map(|(label, value)| {
                let label = truncate(label, available);
                let clean = crate::tui::strip_ansi(value);
                let value_lines = wrap(&clean, available);
                let rendered_value = value_lines
                    .iter()
                    .map(|line| format!("{}{}", " ".repeat(INDENT * 2), line))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}{}\n{}", " ".repeat(INDENT), label, rendered_value)
            })
            .collect::<Vec<_>>()
            .join("\n");
    }

    let label_width = rows
        .iter()
        .map(|(label, _)| UnicodeWidthStr::width(*label))
        .max()
        .unwrap_or(0)
        .min(22);
    let value_width = available
        .saturating_sub(label_width)
        .saturating_sub(LABEL_GAP)
        .max(12);

    rows.iter()
        .map(|(label, value)| {
            let label = truncate(label, label_width);
            let label = crate::tui::pad_to_width(&label, label_width);
            let clean = crate::tui::strip_ansi(value);
            let value_lines = if crate::tui::visible_width(value) > value_width {
                wrap(&clean, value_width)
            } else {
                vec![value.clone()]
            };
            let continuation_prefix = " ".repeat(INDENT + label_width + LABEL_GAP);
            let mut rendered = format!(
                "{}{}{}{}",
                " ".repeat(INDENT),
                label,
                " ".repeat(LABEL_GAP),
                value_lines.first().cloned().unwrap_or_default()
            );
            for line in value_lines.iter().skip(1) {
                rendered.push('\n');
                rendered.push_str(&continuation_prefix);
                rendered.push_str(line);
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn summary(parts: &[String]) -> String {
    format!("{}{}", " ".repeat(INDENT), parts.join("   "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_without_exceeding_visible_width() {
        let lines = wrap("a short sentence that needs wrapping", 12);
        assert!(lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 12));
        assert!(lines.len() > 1);
    }

    #[test]
    fn truncates_unicode_to_requested_width() {
        let value = truncate("hello世界", 7);
        assert!(UnicodeWidthStr::width(value.as_str()) <= 7);
        assert!(value.ends_with('…'));
    }

    #[test]
    fn facts_wrap_long_values_without_losing_content() {
        std::env::set_var("COLUMNS", "58");
        let value =
            "/home/light/Downloads/bqa/opencode2claude-branch-audit/runtime/opencode2api.log";
        let output = facts(&[("Log", value.to_string())]);
        assert!(!output.contains('…'));
        assert_eq!(
            output.split_whitespace().collect::<String>(),
            format!("Log{value}")
        );
        std::env::remove_var("COLUMNS");
    }

    #[test]
    fn facts_align_values_without_box_borders() {
        std::env::set_var("COLUMNS", "100");
        let output = facts(&[("Port", "4000".into()), ("Long label", "value".into())]);
        assert!(!output.contains('│'));
        assert!(!output.contains('┌'));
        assert!(output.contains("Port"));
        assert!(output.contains("4000"));
        std::env::remove_var("COLUMNS");
    }
}
