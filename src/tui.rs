//! Terminal UI helpers — box-drawing alignment with Unicode/ANSI awareness.
//!
//! Provides width-safe padding and box-line construction so that box-drawing
//! borders (`║`, `═`) stay aligned regardless of ANSI escape codes or
//! multi-byte Unicode characters (CJK, emoji, Vietnamese diacritics).

use unicode_width::UnicodeWidthStr;

/// Strip ANSI escape sequences (CSI `\x1b[…m` style) from a string.
///
/// Handles the subset produced by `yansi::Paint`: bold, dim, colors, reset.
/// Since `yansi` does not emit SGR parameters beyond `m` terminators, this
/// covers all cases in this codebase.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume \x1b[N;N;...m  (only SGR "m" terminator)
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Visible display width of `content` after stripping ANSI codes.
///
/// Uses `unicode-width` to correctly measure CJK characters (width=2) vs
/// ASCII / Latin-with-diacritics (width=1).
pub fn visible_width(content: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(content).as_str())
}

/// Right-pad `content` to reach at least `target` visible columns.
///
/// ANSI codes are transparent to the measurement. The returned string
/// preserves the original ANSI styling plus trailing spaces.
pub fn pad_to_width(content: &str, target: usize) -> String {
    let w = visible_width(content);
    if w >= target {
        content.to_string()
    } else {
        format!("{content}{}", " ".repeat(target - w))
    }
}

/// Build one line of a box whose total visible width is `total_width`.
///
/// `left` is everything up to the dynamic content (including the opening `║`).
/// The returned string is `"{left}{content}{padding}║"` with the right border
/// aligned at column `total_width - 1`.
pub fn box_line(left: &str, content: &str, total_width: usize) -> String {
    let left_visible = UnicodeWidthStr::width(left);
    let content_visible = visible_width(content);
    let pad = total_width
        .saturating_sub(left_visible)
        .saturating_sub(content_visible)
        .saturating_sub(1); // closing ║
    format!("{left}{content}{}║", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_reset() {
        assert_eq!(strip_ansi("\x1b[0mhello"), "hello");
    }

    #[test]
    fn strip_ansi_bold_cyan() {
        assert_eq!(strip_ansi("\x1b[1m\x1b[36mhi\x1b[0m"), "hi");
    }

    #[test]
    fn no_ansi_passthrough() {
        assert_eq!(strip_ansi("hello"), "hello");
    }

    #[test]
    fn visible_width_ascii() {
        assert_eq!(visible_width("hello"), 5);
    }

    #[test]
    fn visible_width_ansi_invisible() {
        assert_eq!(visible_width("\x1b[36mhello\x1b[0m"), 5);
    }

    #[test]
    fn visible_width_emoji() {
        // Many emoji are width 2; unicode-width should reflect that
        assert!(visible_width("🎉") == 1 || visible_width("🎉") == 2);
    }

    #[test]
    fn pad_to_width_ascii() {
        let padded = pad_to_width("test", 10);
        assert_eq!(padded, "test      ");
        assert_eq!(visible_width(&padded), 10);
    }

    #[test]
    fn pad_to_width_ansi() {
        let styled = format!("\x1b[1m\x1b[36m{}\x1b[0m", "test");
        let padded = pad_to_width(&styled, 10);
        assert_eq!(visible_width(&padded), 10);
        assert!(padded.starts_with("\x1b[1m\x1b[36m"));
        assert!(padded.ends_with("      "));
    }

    #[test]
    fn pad_to_width_exact_noop() {
        let padded = pad_to_width("test", 4);
        assert_eq!(padded, "test");
    }

    #[test]
    fn pad_to_width_overflow_noop() {
        let padded = pad_to_width("longer", 3);
        assert_eq!(padded, "longer");
    }

    #[test]
    fn box_line_bridge() {
        let line = box_line("║  Bridge:  http://", "127.0.0.1:4000", 48);
        assert_eq!(
            UnicodeWidthStr::width(line.as_str()),
            48,
            "box line must be exactly 48 visible columns, got {}: {line:?}",
            UnicodeWidthStr::width(line.as_str())
        );
        assert!(line.starts_with("║"));
        assert!(line.ends_with('║'));
    }

    #[test]
    fn box_line_model() {
        let line = box_line("║  Model:   ", "deepseek-v4-flash", 48);
        assert_eq!(
            UnicodeWidthStr::width(line.as_str()),
            48,
            "box line must be exactly 48 visible columns, got {}: {line:?}",
            UnicodeWidthStr::width(line.as_str())
        );
    }

    #[test]
    fn box_line_empty_content() {
        let line = box_line("║  Auth:    ", "", 48);
        assert_eq!(
            UnicodeWidthStr::width(line.as_str()),
            48,
            "box line must be exactly 48 visible columns, got {}: {line:?}",
            UnicodeWidthStr::width(line.as_str())
        );
        assert!(line.ends_with('║'));
    }
}
