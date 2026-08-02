//! Terminal text helpers — Unicode width, ANSI stripping, and safe padding.
//!
//! Despite the historical module name, these helpers are used by the regular
//! line-oriented CLI. They do not implement a full-screen terminal UI.

use unicode_width::UnicodeWidthStr;

/// Strip common terminal escape sequences from externally sourced text.
///
/// Handles CSI sequences (colors, cursor controls, erase commands) and OSC
/// sequences (window titles, hyperlinks). This is intentionally broader than
/// the subset emitted by `yansi`, because Docker and daemon logs may contain
/// their own terminal control codes.
pub fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let rest = &s[index..];
            if let Some(character) = rest.chars().next() {
                out.push(character);
                index += character.len_utf8();
            } else {
                break;
            }
            continue;
        }

        index += 1;
        if index >= bytes.len() {
            break;
        }

        match bytes[index] {
            b'[' => {
                // CSI: consume until the final byte in the 0x40..=0x7e range.
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' => {
                // OSC: terminate at BEL or ST (ESC backslash).
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\'
                    {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => {
                // Two-byte escape sequence.
                index += 1;
            }
        }
    }

    out
}

/// Visible display width of `content` after stripping ANSI codes.
pub fn visible_width(content: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(content).as_str())
}

/// Right-pad `content` to reach at least `target` visible columns.
pub fn pad_to_width(content: &str, target: usize) -> String {
    let width = visible_width(content);
    if width >= target {
        content.to_string()
    } else {
        format!("{content}{}", " ".repeat(target - width))
    }
}

/// Build one line of a legacy box whose total visible width is `total_width`.
///
/// Kept for compatibility with the foreground startup banner. New CLI output
/// should prefer borderless facts and tables from `presentation`.
pub fn box_line(left: &str, content: &str, total_width: usize) -> String {
    let left_visible = UnicodeWidthStr::width(left);
    let available = total_width.saturating_sub(left_visible).saturating_sub(1);
    let content = if visible_width(content) > available {
        crate::presentation::truncate(&strip_ansi(content), available)
    } else {
        content.to_string()
    };
    let content_visible = visible_width(&content);
    let pad = available.saturating_sub(content_visible);
    format!("{left}{content}{}║", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sgr_sequences() {
        assert_eq!(strip_ansi("\x1b[1m\x1b[36mhi\x1b[0m"), "hi");
    }

    #[test]
    fn strips_osc_hyperlinks() {
        assert_eq!(
            strip_ansi("\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
    }

    #[test]
    fn strips_cursor_control_sequences() {
        assert_eq!(strip_ansi("a\x1b[2Kb"), "ab");
    }

    #[test]
    fn visible_width_ignores_ansi() {
        assert_eq!(visible_width("\x1b[36mhello\x1b[0m"), 5);
    }

    #[test]
    fn pad_to_width_preserves_styled_content() {
        let styled = "\x1b[1m\x1b[36mtest\x1b[0m";
        let padded = pad_to_width(styled, 10);
        assert_eq!(visible_width(&padded), 10);
        assert!(padded.ends_with("      "));
    }

    #[test]
    fn box_line_truncates_overflow() {
        let line = box_line("║  Model:   ", "a-very-long-model-name-that-cannot-fit", 32);
        assert_eq!(UnicodeWidthStr::width(line.as_str()), 32);
        assert!(line.ends_with('║'));
    }
}
