//! Sanitization utilities for LLM output cleaning.
//!
//! Provides functions to strip system leakage tags from model responses.
//! Extracted from `forward.rs` during module split.

use crate::opencode::forward::common::{find_literal_marker_in_context, CompatMarkdownState};

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

/// Parse DSML invocations and report whether the block was structurally broken.
///
/// The second element is true when the scan hit a missing close tag (truncated
/// block) or a completed invocation carried no name. Structural integrity is
/// derived from the scan itself, not from counting raw tag occurrences, so a
/// parameter value that literally quotes the DSML grammar (e.g. contains a
/// literal `</｜DSML｜parameter>`) does not falsely mark the block malformed.
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
}
