//! Sanitization utilities for LLM output cleaning.
//!
//! Provides functions to strip system leakage tags from model responses.
//! Extracted from `forward.rs` during module split.

use crate::opencode::forward::common::{find_literal_marker_in_context, CompatMarkdownState};
use tracing::warn;

/// Strip system leakage tags (like `</think>`, `</parameter>`, etc.) from LLM outputs.
///
/// Removes known tags that models sometimes leak from their system prompt context,
/// including HTML-encoded variants. Also trims leading whitespace when tags were
/// stripped from the beginning of the text.
pub fn strip_system_tags(text: &str) -> String {
    strip_system_tags_impl(text, &CompatMarkdownState::default(), false)
}

pub(crate) fn strip_system_tags_with_context(
    text: &str,
    initial_state: &CompatMarkdownState,
) -> String {
    strip_system_tags_impl(text, initial_state, false)
}

/// Strip ordinary system-leak tags while preserving generic XML tool protocol
/// markers for the compatibility parser that runs immediately afterwards.
///
/// The synchronous pipeline first extracts canonical DSML and then extracts
/// compatibility markers. Removing `<tool_calls>/<invoke>/<parameter>` during
/// the DSML phase would erase a valid tool request before the compatibility
/// parser can validate and convert it.
fn strip_system_tags_preserving_compat_xml(text: &str) -> String {
    strip_system_tags_impl(text, &CompatMarkdownState::default(), true)
}

fn strip_system_tags_impl(
    text: &str,
    initial_state: &CompatMarkdownState,
    preserve_compat_xml: bool,
) -> String {
    const TAGS: [&str; 28] = [
        "</think>",
        "<think>",
        "</thinking>",
        "<thinking>",
        "</parameter>",
        "<parameter>",
        "</｜DSML｜parameter>",
        "<｜DSML｜parameter>",
        "</｜DSML｜invoke>",
        "<｜DSML｜invoke>",
        "</｜DSML｜tool_calls>",
        "<｜DSML｜tool_calls>",
        // ASCII-pipe and plain-tag variants free models emit in place of the
        // full-width DSML markers; they must not reach the visible stream.
        "<|DSML|tool_calls>",
        "</|DSML|tool_calls>",
        "<|DSML|invoke>",
        "</|DSML|invoke>",
        "<|DSML|parameter>",
        "</|DSML|parameter>",
        "<dsml>",
        "</dsml>",
        "&lt;/think&gt;",
        "&lt;think&gt;",
        "</tool_calls>",
        "<tool_calls>",
        "</tool_call>",
        "<tool_call>",
        "</invoke>",
        "<invoke>",
    ];

    /// Tool-protocol opening tags can carry attributes (`<invoke name="…">`),
    /// which the exact-match `TAGS` list cannot match; skip through the closing
    /// `>`, respecting quotes so a `>` inside an attribute does not truncate.
    const TOOL_XML_OPEN_PREFIXES: [&str; 10] = [
        "<|DSML|invoke",
        "<|DSML|tool_calls",
        "<|DSML|parameter",
        "<｜DSML｜invoke",
        "<｜DSML｜tool_calls",
        "<｜DSML｜parameter",
        "<tool_calls",
        "<tool_call",
        "<invoke",
        "<parameter",
    ];

    let mut state = initial_state.clone();
    let mut cleaned = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut stripped_before_visible_text = false;
    let mut has_visible_text = false;

    while cursor < text.len() {
        if state.is_executable_context() {
            if let Some(tag) = TAGS.iter().find(|tag| {
                (!preserve_compat_xml || !is_compat_xml_exact_tag(tag))
                    && text[cursor..].starts_with(**tag)
            }) {
                if !has_visible_text {
                    stripped_before_visible_text = true;
                }
                cursor += tag.len();
                continue;
            }
            let rest = &text[cursor..];
            if let Some(prefix) = TOOL_XML_OPEN_PREFIXES.iter().find(|prefix| {
                (!preserve_compat_xml || !is_compat_xml_open_prefix(prefix))
                    && rest.starts_with(**prefix)
            }) {
                let mut scan = prefix.len();
                let mut quote: Option<char> = None;
                let mut closed_at = None;
                while scan < rest.len() {
                    let ch = rest[scan..].chars().next().expect("valid UTF-8 boundary");
                    if let Some(active) = quote {
                        if ch == active {
                            quote = None;
                        }
                    } else if ch == '"' || ch == '\'' {
                        quote = Some(ch);
                    } else if ch == '>' {
                        closed_at = Some(scan);
                        break;
                    }
                    scan += ch.len_utf8();
                }
                // Only strip a properly terminated tag; an unterminated leak
                // (no `>`) is left untouched rather than swallowing the message.
                if let Some(end) = closed_at {
                    if !has_visible_text {
                        stripped_before_visible_text = true;
                    }
                    cursor += end + 1;
                    continue;
                }
            }
        }

        let ch = text[cursor..].chars().next().expect("valid UTF-8 boundary");
        let next = if ch == '`' || ch == '~' {
            cursor
                + text[cursor..]
                    .chars()
                    .take_while(|value| *value == ch)
                    .count()
                    * ch.len_utf8()
        } else {
            cursor + ch.len_utf8()
        };
        let fragment = &text[cursor..next];
        cleaned.push_str(fragment);
        if !has_visible_text && fragment.chars().any(|value| !value.is_whitespace()) {
            has_visible_text = true;
        }
        state.advance(fragment);
        cursor = next;
    }

    if stripped_before_visible_text {
        let trim_bytes = cleaned.len() - cleaned.trim_start().len();
        cleaned.drain(..trim_bytes);
    }
    cleaned
}

fn is_compat_xml_exact_tag(tag: &str) -> bool {
    matches!(
        tag,
        "<parameter>"
            | "</parameter>"
            | "<tool_calls>"
            | "</tool_calls>"
            | "<tool_call>"
            | "</tool_call>"
            | "<invoke>"
            | "</invoke>"
    )
}

fn is_compat_xml_open_prefix(prefix: &str) -> bool {
    matches!(
        prefix,
        "<tool_calls" | "<tool_call" | "<invoke" | "<parameter"
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDsmlCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

fn extract_attribute(tag_content: &str, attr_name: &str) -> String {
    let pattern = attr_name;
    let mut pos = 0;
    while let Some(match_pos) = tag_content[pos..].find(pattern) {
        let abs_match_pos = pos + match_pos;
        // Boundary-aware: the attribute name must start the tag content or
        // follow whitespace, so a lookup for `name` never matches inside
        // `filename` / `pathname` / `data-name`.
        let at_boundary = abs_match_pos == 0
            || tag_content[..abs_match_pos]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        if !at_boundary {
            pos = abs_match_pos + pattern.len();
            continue;
        }
        let rem = &tag_content[abs_match_pos + pattern.len()..];
        let mut eq_found = false;
        let mut val_start_pos = None;
        let mut quote_char = None;
        for (i, c) in rem.char_indices() {
            if c.is_whitespace() {
                continue;
            }
            if c == '=' {
                eq_found = true;
                continue;
            }
            if eq_found {
                if c == '"' || c == '\'' {
                    quote_char = Some(c);
                    val_start_pos = Some(i + 1);
                    break;
                } else {
                    val_start_pos = Some(i);
                    break;
                }
            } else {
                break;
            }
        }
        if let Some(start) = val_start_pos {
            let rem_val = &rem[start..];
            if let Some(q) = quote_char {
                if let Some(end) = rem_val.find(q) {
                    return rem_val[..end].to_string();
                }
            } else {
                let end = rem_val
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(rem_val.len());
                return rem_val[..end].to_string();
            }
        }
        pos = abs_match_pos + pattern.len();
    }
    String::new()
}

pub fn parse_dsml_tool_calls(text: &str) -> Vec<ParsedDsmlCall> {
    parse_dsml_tool_calls_checked(text).0
}

/// Locate a foreign `<｜DSML｜invoke` opening at the TOP LEVEL of an
/// invocation body.
///
/// Parameter elements are consumed wholesale (up to their close tag), so a
/// literal invoke marker quoted inside a parameter value is value text, not a
/// foreign open, and never triggers resynchronization.
fn find_foreign_invoke_open(body: &str) -> Option<usize> {
    const PARAM_CLOSE: &str = "</｜DSML｜parameter>";
    let mut pos = 0;
    while pos < body.len() {
        let rest = &body[pos..];
        if rest.starts_with("<｜DSML｜invoke") {
            return Some(pos);
        }
        if rest.starts_with("<｜DSML｜parameter") {
            // An unterminated parameter element ends detection; the main
            // scan reports it truncated separately.
            let close_rel = rest.find(PARAM_CLOSE)?;
            pos += close_rel + PARAM_CLOSE.len();
            continue;
        }
        pos += rest.chars().next()?.len_utf8();
    }
    None
}

/// Parse DSML invocations and report whether the block was structurally broken.
///
/// The second element is true when the scan hit an unrecoverable missing close
/// tag (truncated block) or a completed invocation carried no name.
/// Structural integrity is derived from the scan itself, not from counting raw
/// tag occurrences, so a parameter value that literally quotes the DSML
/// grammar (e.g. contains a literal `</｜DSML｜parameter>`) does not falsely
/// mark the block malformed.
///
/// Resynchronization: when an invocation body contains a foreign top-level
/// `<｜DSML｜invoke` opening, this invocation was never closed — the close tag
/// that follows belongs to the later invocation. The unterminated fragment is
/// dropped (never merged into another call; evidence logged) and the scan
/// resumes at the foreign open, so the well-formed invocation is still parsed
/// and the batch stays clean. Only a breakage with nothing recoverable after
/// it marks the block broken.
fn parse_dsml_tool_calls_checked(text: &str) -> (Vec<ParsedDsmlCall>, bool) {
    let mut calls = Vec::new();
    let mut search_pos = 0;
    let mut truncated = false;
    let mut invokes_processed = 0usize;

    while let Some(invoke_start) = text[search_pos..].find("<｜DSML｜invoke") {
        let absolute_invoke_start = search_pos + invoke_start;
        let remaining = &text[absolute_invoke_start..];
        let Some(tag_open_end) = remaining.find('>') else {
            truncated = true;
            break;
        };
        let tag_open_content = &remaining[..tag_open_end];

        let name = extract_attribute(tag_open_content, "name");

        let Some(invoke_end) = remaining.find("</｜DSML｜invoke>") else {
            truncated = true;
            break;
        };
        let invoke_body = &remaining[tag_open_end + 1..invoke_end];

        // Resynchronization: a foreign invoke opening at the top level of
        // this body means this invocation was never closed and the close tag
        // we matched belongs to the later invocation. Quarantine this
        // fragment — drop it without merging any of its parameters — and
        // resume at the foreign open so the well-formed invocation still
        // parses and executes.
        if let Some(foreign_rel) = find_foreign_invoke_open(invoke_body) {
            warn!(
                dropped_bytes = tag_open_end + 1 + foreign_rel,
                "Dropping unterminated DSML invoke; resynchronizing at the next invocation"
            );
            search_pos = absolute_invoke_start + tag_open_end + 1 + foreign_rel;
            continue;
        }

        let mut params = serde_json::Map::new();
        let mut p_pos = 0;
        while let Some(p_start) = invoke_body[p_pos..].find("<｜DSML｜parameter") {
            let abs_p_start = p_pos + p_start;
            let p_rem = &invoke_body[abs_p_start..];
            let Some(p_open_end) = p_rem.find('>') else {
                truncated = true;
                break;
            };
            let p_open_content = &p_rem[..p_open_end];

            let p_name = extract_attribute(p_open_content, "name");

            let Some(p_close) = p_rem.find("</｜DSML｜parameter>") else {
                truncated = true;
                break;
            };
            let p_val_str = p_rem[p_open_end + 1..p_close].trim();
            let mut clean_val = p_val_str.to_string();
            if clean_val.starts_with("```") {
                if let Some(newline_pos) = clean_val.find('\n') {
                    clean_val = clean_val[newline_pos + 1..].to_string();
                } else {
                    clean_val = clean_val[3..].to_string();
                }
                if clean_val.ends_with("```") {
                    clean_val = clean_val[..clean_val.len() - 3].to_string();
                }
                clean_val = clean_val.trim().to_string();
            }

            let val = if (clean_val.starts_with('{') && clean_val.ends_with('}'))
                || (clean_val.starts_with('[') && clean_val.ends_with(']'))
            {
                serde_json::from_str(&clean_val)
                    .unwrap_or_else(|_| serde_json::Value::String(clean_val.clone()))
            } else {
                serde_json::Value::String(clean_val)
            };

            if !p_name.is_empty() {
                if p_name == "path" {
                    params.insert("file".to_string(), val.clone());
                }
                params.insert(p_name, val);
            }

            p_pos = abs_p_start + p_close + "</｜DSML｜parameter>".len();
        }

        invokes_processed += 1;
        if !name.is_empty() {
            calls.push(ParsedDsmlCall {
                name,
                arguments: serde_json::Value::Object(params),
            });
        }

        search_pos = absolute_invoke_start + invoke_end + "</｜DSML｜invoke>".len();
    }

    let structurally_broken = truncated || invokes_processed != calls.len();
    (calls, structurally_broken)
}

pub fn parse_dsml_tool_calls_detailed(text: &str) -> (Vec<ParsedDsmlCall>, bool) {
    parse_dsml_tool_calls_checked(text)
}

#[derive(Debug, Clone, PartialEq)]
pub struct DsmlExtraction {
    pub cleaned_text: String,
    pub calls: Vec<ParsedDsmlCall>,
    pub malformed_intent: bool,
}

pub fn extract_and_clean_dsml(text: &str) -> (String, Vec<ParsedDsmlCall>) {
    let extraction = extract_and_clean_dsml_detailed(text);
    (extraction.cleaned_text, extraction.calls)
}

pub fn extract_and_clean_dsml_detailed(text: &str) -> DsmlExtraction {
    const OPEN: &str = "<｜DSML｜tool_calls>";
    const CLOSE: &str = "</｜DSML｜tool_calls>";

    let mut cleaned_text = String::with_capacity(text.len());
    let mut calls = Vec::new();
    let mut offset = 0;
    let mut state = CompatMarkdownState::default();
    let mut malformed_intent = false;

    while let Some(relative_start) = find_literal_marker_in_context(&text[offset..], OPEN, &state) {
        let start = offset + relative_start;
        let safe_prefix = &text[offset..start];
        cleaned_text.push_str(safe_prefix);
        state.advance(safe_prefix);

        let remaining = &text[start..];
        let Some(relative_end) = remaining.find(CLOSE) else {
            malformed_intent = true;
            cleaned_text.push_str(remaining);
            offset = text.len();
            break;
        };
        let end = start + relative_end + CLOSE.len();
        let block = &text[start..end];
        let (parsed, malformed) = parse_dsml_tool_calls_detailed(block);
        if malformed {
            malformed_intent = true;
            cleaned_text.push_str(block);
        } else {
            calls.extend(parsed);
        }
        offset = end;
    }

    cleaned_text.push_str(&text[offset..]);
    DsmlExtraction {
        cleaned_text: if malformed_intent {
            cleaned_text
        } else {
            strip_system_tags_preserving_compat_xml(&cleaned_text)
        },
        calls,
        malformed_intent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_system_tags() {
        assert_eq!(strip_system_tags("</think>Hello"), "Hello");
        assert_eq!(strip_system_tags("</think>\n\nHello"), "Hello");
        assert_eq!(strip_system_tags("Hello</think>"), "Hello");
        assert_eq!(strip_system_tags("Hello</thinking>"), "Hello");
        assert_eq!(strip_system_tags("Hello</parameter>World"), "HelloWorld");
        assert_eq!(strip_system_tags("</｜DSML｜parameter>\nHello"), "Hello");
        assert_eq!(strip_system_tags("</｜DSML｜invoke>\nHello"), "Hello");
        assert_eq!(strip_system_tags("</｜DSML｜tool_calls>\nHello"), "Hello");
        assert_eq!(
            strip_system_tags("<think>Some thinking</think>Response"),
            "Some thinkingResponse"
        );
        assert_eq!(strip_system_tags("Normal text"), "Normal text");
    }

    #[test]
    fn strips_ascii_pipe_and_plain_dsml_variants() {
        assert_eq!(
            strip_system_tags("<|DSML|tool_calls>hello</|DSML|tool_calls>"),
            "hello"
        );
        assert_eq!(
            strip_system_tags("<|DSML|invoke name=\"Bash\">x</|DSML|invoke>"),
            "x"
        );
        assert_eq!(
            strip_system_tags("<|DSML|parameter name=\"p\">v</|DSML|parameter>"),
            "v"
        );
        assert_eq!(strip_system_tags("<dsml>hello</dsml>"), "hello");
    }

    #[test]
    fn strips_generic_xml_tool_tags_with_attributes_outside_code() {
        assert_eq!(
            strip_system_tags(
                "<tool_calls><invoke name=\"Bash\"><parameter name=\"command\">echo ok</parameter></invoke></tool_calls>"
            ),
            "echo ok"
        );
        assert_eq!(
            strip_system_tags(
                "<tool_call><invoke name=\"Agent\"><parameter name=\"prompt\">review</parameter></invoke></tool_call>"
            ),
            "review"
        );
    }

    #[test]
    fn generic_xml_tool_tags_inside_code_are_inert() {
        let fenced = "```xml\n<tool_calls><invoke name=\"Bash\"><parameter name=\"command\">echo ok</parameter></invoke></tool_calls>\n```";
        let inline = "Use `<tool_call><invoke name=\"Bash\"></invoke></tool_call>` as an example.";
        assert_eq!(strip_system_tags(fenced), fenced);
        assert_eq!(strip_system_tags(inline), inline);
    }

    #[test]
    fn test_extract_attribute() {
        assert_eq!(extract_attribute(r#"name="bash""#, "name"), "bash");
        assert_eq!(extract_attribute(r#"name='bash'"#, "name"), "bash");
        assert_eq!(extract_attribute(r#"name = "bash""#, "name"), "bash");
        assert_eq!(extract_attribute(r#"name  =  'bash'"#, "name"), "bash");
        assert_eq!(
            extract_attribute(r#"other="val" name="bash""#, "name"),
            "bash"
        );
    }

    /// Boundary-aware matching: an attribute name must be preceded by
    /// whitespace or the start of the tag content, so a lookup for `name`
    /// never matches inside `filename` / `pathname` / `data-name`.
    #[test]
    fn filename_before_name_does_not_shadow_name_lookup() {
        let tag = r#"<｜DSML｜invoke filename="x" name="Read""#;
        assert_eq!(extract_attribute(tag, "name"), "Read");
        assert_eq!(extract_attribute(tag, "filename"), "x");
    }

    #[test]
    fn name_before_filename_still_resolves_name() {
        let tag = r#"<｜DSML｜invoke name="Read" filename="notes.md""#;
        assert_eq!(extract_attribute(tag, "name"), "Read");
        assert_eq!(extract_attribute(tag, "filename"), "notes.md");
    }

    #[test]
    fn substring_attribute_names_never_match_partial() {
        // Suffix/prefix overlaps must not satisfy a lookup either.
        assert_eq!(
            extract_attribute(r#"<invoke pathname="/tmp/a" name="R""#, "name"),
            "R"
        );
        assert_eq!(
            extract_attribute(r#"<invoke data-name="x" name="R""#, "name"),
            "R"
        );
    }

    /// Current semantics: among boundary-valid occurrences of the same
    /// attribute name, the first one wins.
    #[test]
    fn repeated_attribute_first_occurrence_wins() {
        let tag = r#"<｜DSML｜invoke name="first" name="second""#;
        assert_eq!(extract_attribute(tag, "name"), "first");
    }

    #[test]
    fn test_parse_dsml_tool_calls() {
        let sample = r#"
            <｜DSML｜tool_calls>
              <｜DSML｜invoke name="Edit">
                <｜DSML｜parameter name="path">scripts/lib/process.sh</｜DSML｜parameter>
                <｜DSML｜parameter name="edits">
```json
[
  {"oldText": "foo", "newText": "bar"}
]
```
                </｜DSML｜parameter>
              </｜DSML｜invoke>
            </｜DSML｜tool_calls>
        "#;
        let res = parse_dsml_tool_calls(sample);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "Edit");
        assert_eq!(res[0].arguments["path"], "scripts/lib/process.sh");
        assert_eq!(res[0].arguments["file"], "scripts/lib/process.sh");
        assert_eq!(res[0].arguments["edits"][0]["oldText"], "foo");
        assert_eq!(res[0].arguments["edits"][0]["newText"], "bar");
    }

    #[test]
    fn test_extract_and_clean_dsml() {
        let sample = "Hello <｜DSML｜tool_calls><｜DSML｜invoke name=\"bash\"><｜DSML｜parameter name=\"command\">git status</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls> World";
        let (text, calls) = extract_and_clean_dsml(sample);
        assert_eq!(text, "Hello  World");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "git status");
    }
}

#[cfg(test)]
mod dsml_regression_tests {
    use super::*;

    #[test]
    fn parses_multiple_invocations_and_typed_parameters() {
        let sample = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Bash\">",
            "<｜DSML｜parameter name=\"command\">printf ONE</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "<｜DSML｜invoke name=\"Edit\">",
            "<｜DSML｜parameter name=\"path\">README.md</｜DSML｜parameter>",
            "<｜DSML｜parameter name=\"replace_all\">true</｜DSML｜parameter>",
            "<｜DSML｜parameter name=\"edits\">[{\"oldText\":\"a\",\"newText\":\"b\"}]</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );

        let calls = parse_dsml_tool_calls(sample);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "printf ONE");
        assert_eq!(calls[1].name, "Edit");
        assert_eq!(calls[1].arguments["path"], "README.md");
        assert_eq!(calls[1].arguments["file"], "README.md");
        assert_eq!(calls[1].arguments["replace_all"], "true");
        assert_eq!(calls[1].arguments["edits"][0]["newText"], "b");
    }

    #[test]
    fn parameter_value_containing_literal_close_tag_is_not_malformed() {
        let sample = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Write\">",
            "<｜DSML｜parameter name=\"content\">",
            "write this literal text: </｜DSML｜parameter> is not a real close",
            "</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        let (calls, malformed) = parse_dsml_tool_calls_detailed(sample);
        assert!(
            !malformed,
            "quoted grammar inside a parameter value must not mark the block malformed"
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Write");
    }

    #[test]
    fn unclosed_tool_calls_wrapper_is_fail_closed() {
        let sample = concat!(
            "before ",
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"file_path\">src/lib.rs</｜DSML｜parameter>",
            "</｜DSML｜invoke>"
        );

        let extraction = extract_and_clean_dsml_detailed(sample);
        assert_eq!(extraction.cleaned_text, sample);
        assert!(extraction.calls.is_empty());
        assert!(extraction.malformed_intent);
    }

    /// Defect B regression: an unterminated invocation must never swallow the
    /// following complete invocation's close tag into a merged hybrid call.
    /// The broken fragment is dropped (quarantined, evidence logged) and the
    /// well-formed invocation is still extracted so it can execute normally —
    /// mirroring the compat-marker resync contract
    /// (`malformed_then_valid_marker_resynchronizes_without_leaking`).
    #[test]
    fn unterminated_invoke_does_not_merge_with_following_invocation() {
        let sample = concat!(
            "<｜DSML｜invoke name=\"Broken\">",
            "<｜DSML｜parameter name=\"command\">echo ghost</｜DSML｜parameter>",
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"file_path\">src/lib.rs</｜DSML｜parameter>",
            "</｜DSML｜invoke>"
        );

        let (calls, structurally_broken) = parse_dsml_tool_calls_detailed(sample);
        assert_eq!(calls.len(), 1, "exactly the well-formed call survives");
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].arguments["file_path"], "src/lib.rs");
        assert!(
            !calls.iter().any(|call| call.name == "Broken"),
            "the broken fragment must never surface as a tool call"
        );
        assert!(
            !structurally_broken,
            "full resynchronization recovers a clean batch"
        );
    }

    #[test]
    fn unterminated_invoke_resync_flows_through_extraction() {
        let sample = concat!(
            "prefix ",
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Broken\">",
            "<｜DSML｜parameter name=\"command\">ghost</｜DSML｜parameter>",
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"file_path\">src/lib.rs</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>",
            " suffix"
        );

        let extraction = extract_and_clean_dsml_detailed(sample);
        assert_eq!(extraction.calls.len(), 1);
        assert_eq!(extraction.calls[0].name, "Read");
        assert!(!extraction.malformed_intent);
        assert_eq!(
            extraction.cleaned_text, "prefix  suffix",
            "recovered block is stripped like any healthy block"
        );
    }

    #[test]
    fn unterminated_invoke_before_multiple_complete_invocations_recovers_all() {
        let sample = concat!(
            "<｜DSML｜invoke name=\"Broken\">",
            "<｜DSML｜invoke name=\"Bash\">",
            "<｜DSML｜parameter name=\"command\">true</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"file_path\">a.rs</｜DSML｜parameter>",
            "</｜DSML｜invoke>"
        );

        let (calls, structurally_broken) = parse_dsml_tool_calls_detailed(sample);
        let names: Vec<&str> = calls.iter().map(|call| call.name.as_str()).collect();
        assert_eq!(names, vec!["Bash", "Read"]);
        assert!(!structurally_broken);
    }

    /// Resync must not fire when recovery is impossible: with no close tag
    /// anywhere the scan stays truncated and fails closed.
    #[test]
    fn unterminated_invoke_without_any_close_stays_fail_closed() {
        let sample = concat!(
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"file_path\">src/lib.rs</｜DSML｜parameter>"
        );

        let (calls, structurally_broken) = parse_dsml_tool_calls_detailed(sample);
        assert!(calls.is_empty());
        assert!(structurally_broken);

        let wrapped = format!("<｜DSML｜tool_calls>{sample}</｜DSML｜tool_calls>");
        let extraction = extract_and_clean_dsml_detailed(&wrapped);
        assert!(extraction.malformed_intent);
        assert!(extraction.calls.is_empty());
    }

    /// A literal invoke marker quoted INSIDE a parameter value is value text,
    /// not a top-level open: it must not trigger the resync drop.
    #[test]
    fn invoke_marker_inside_parameter_value_is_not_a_foreign_open() {
        let sample = concat!(
            "<｜DSML｜invoke name=\"Write\">",
            "<｜DSML｜parameter name=\"content\">",
            "example: <｜DSML｜invoke name=\"Fake\"> body",
            "</｜DSML｜parameter>",
            "</｜DSML｜invoke>"
        );

        let (calls, structurally_broken) = parse_dsml_tool_calls_detailed(sample);
        assert!(!structurally_broken);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Write");
        assert!(
            calls[0].arguments["content"]
                .as_str()
                .unwrap()
                .contains("<｜DSML｜invoke name=\"Fake\">"),
            "value content must survive verbatim"
        );
    }
}

/// Context invariants for the protected parse layer: DSML markers inside code
/// fences, inline code, or quoted strings must never execute as tools, and
/// structurally broken blocks must be preserved verbatim instead of being
/// half-executed. These tests pin that invariant against regressions.
#[cfg(test)]
mod dsml_context_invariant_tests {
    use super::*;

    #[test]
    fn full_dsml_block_inside_code_fence_does_not_execute() {
        let sample = concat!(
            "Example:\n",
            "```xml\n",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"Bash\">",
            "<｜DSML｜parameter name=\"command\">echo fenced</｜DSML｜parameter>",
            "</｜DSML｜invoke></｜DSML｜tool_calls>\n",
            "```\n",
            "done"
        );
        let extraction = extract_and_clean_dsml_detailed(sample);
        assert_eq!(
            extraction.cleaned_text, sample,
            "fenced DSML must survive byte-for-byte"
        );
        assert!(
            extraction.calls.is_empty(),
            "fenced block must not yield tool calls"
        );
        assert!(!extraction.malformed_intent);
    }

    #[test]
    fn dsml_open_marker_inside_inline_code_does_not_execute() {
        let sample = "Document the literal `<｜DSML｜tool_calls>` marker in prose.";
        let extraction = extract_and_clean_dsml_detailed(sample);
        assert_eq!(extraction.cleaned_text, sample);
        assert!(extraction.calls.is_empty());
        assert!(!extraction.malformed_intent);
    }

    #[test]
    fn dsml_open_marker_inside_double_quoted_string_does_not_execute() {
        // A single opening quote before the marker and a single closing quote
        // after it: while the quote is open the marker must be inert.
        let sample = "Config value: \"quoted <｜DSML｜tool_calls> payload\" stays text.";
        let extraction = extract_and_clean_dsml_detailed(sample);
        assert_eq!(extraction.cleaned_text, sample);
        assert!(extraction.calls.is_empty());
        assert!(!extraction.malformed_intent);
    }

    #[test]
    fn leak_tags_inside_quotes_and_fences_are_preserved_by_strip() {
        let quoted = "Say \"</think> oops\" out loud.";
        assert_eq!(strip_system_tags(quoted), quoted);

        let fenced = "```\n<think>not a leak</think>\n```";
        assert_eq!(strip_system_tags(fenced), fenced);

        let inline = "Inline `<｜DSML｜parameter>` stays visible.";
        assert_eq!(strip_system_tags(inline), inline);
    }

    #[test]
    fn truncated_parameter_close_marks_block_malformed_and_verbatim() {
        let sample = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"path\">src/lib.rs",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        let extraction = extract_and_clean_dsml_detailed(sample);
        assert!(
            extraction.malformed_intent,
            "missing parameter close is truncated"
        );
        assert!(
            extraction.calls.is_empty(),
            "truncated block must not execute"
        );
        assert_eq!(
            extraction.cleaned_text, sample,
            "block evidence must be preserved"
        );
    }

    #[test]
    fn malformed_dsml_intent_skips_the_tag_stripping_pass() {
        // When a block is malformed the cleaned text is returned untouched so
        // the retry/diagnostics layer can inspect the raw evidence; leaked
        // tags elsewhere in the message stay in place by design.
        let sample = "<think>leak</think> <｜DSML｜tool_calls><｜DSML｜invoke name=\"A\">";
        let extraction = extract_and_clean_dsml_detailed(sample);
        assert!(extraction.malformed_intent);
        assert_eq!(extraction.cleaned_text, sample);
        assert!(extraction.calls.is_empty());
    }

    #[test]
    fn invalid_json_parameter_values_fall_back_to_raw_string() {
        let sample = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Edit\">",
            "<｜DSML｜parameter name=\"edits\">{broken object}</｜DSML｜parameter>",
            "<｜DSML｜parameter name=\"list\">[not, valid]</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        let calls = parse_dsml_tool_calls(sample);
        assert_eq!(calls.len(), 1);
        // Brace/bracket-shaped but invalid JSON degrades to a string value;
        // it must never become `null` or abort the scan.
        assert_eq!(calls[0].arguments["edits"], "{broken object}");
        assert_eq!(calls[0].arguments["list"], "[not, valid]");
    }

    #[test]
    fn nested_json_parameter_values_parse_as_json() {
        let sample = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Write\">",
            "<｜DSML｜parameter name=\"meta\">{\"inner\":{\"k\":1}}</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        let calls = parse_dsml_tool_calls(sample);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["meta"]["inner"]["k"], 1);
    }

    #[test]
    fn invoke_tag_with_extra_attributes_still_resolves_name() {
        let sample = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Bash\" id=\"call_7\">",
            "<｜DSML｜parameter name=\"command\">true</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        let calls = parse_dsml_tool_calls(sample);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
    }

    /// Guard for the resync repair: even a broken-then-complete invocation
    /// pair inside a code fence stays inert end-to-end — the context gate
    /// must refuse the block before the parser ever sees it.
    #[test]
    fn fenced_broken_then_complete_invocation_pair_never_executes() {
        let sample = concat!(
            "Docs:\n```xml\n",
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Ghost\">",
            "<｜DSML｜invoke name=\"Read\">",
            "<｜DSML｜parameter name=\"file_path\">secret.txt</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>\n",
            "```\nend"
        );
        let extraction = extract_and_clean_dsml_detailed(sample);
        assert_eq!(
            extraction.cleaned_text, sample,
            "fenced DSML must survive byte-for-byte"
        );
        assert!(
            extraction.calls.is_empty(),
            "fenced broken/complete pair must not yield tool calls"
        );
        assert!(!extraction.malformed_intent);
    }
}
