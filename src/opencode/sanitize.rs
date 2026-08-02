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
    strip_system_tags_with_context(text, &CompatMarkdownState::default())
}

pub(crate) fn strip_system_tags_with_context(
    text: &str,
    initial_state: &CompatMarkdownState,
) -> String {
    const TAGS: [&str; 16] = [
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
        "&lt;/think&gt;",
        "&lt;think&gt;",
        "</tool_call>",
        "<tool_call>",
    ];

    let mut state = initial_state.clone();
    let mut cleaned = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut stripped_before_visible_text = false;
    let mut has_visible_text = false;

    while cursor < text.len() {
        if state.is_executable_context() {
            if let Some(tag) = TAGS.iter().find(|tag| text[cursor..].starts_with(**tag)) {
                if !has_visible_text {
                    stripped_before_visible_text = true;
                }
                cursor += tag.len();
                continue;
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
    let mut calls = Vec::new();
    let mut search_pos = 0;

    while let Some(invoke_start) = text[search_pos..].find("<｜DSML｜invoke") {
        let absolute_invoke_start = search_pos + invoke_start;
        let remaining = &text[absolute_invoke_start..];
        let Some(tag_open_end) = remaining.find('>') else {
            break;
        };
        let tag_open_content = &remaining[..tag_open_end];

        let name = extract_attribute(tag_open_content, "name");

        let Some(invoke_end) = remaining.find("</｜DSML｜invoke>") else {
            break;
        };
        let invoke_body = &remaining[tag_open_end + 1..invoke_end];

        let mut params = serde_json::Map::new();
        let mut p_pos = 0;
        while let Some(p_start) = invoke_body[p_pos..].find("<｜DSML｜parameter") {
            let abs_p_start = p_pos + p_start;
            let p_rem = &invoke_body[abs_p_start..];
            let Some(p_open_end) = p_rem.find('>') else {
                break;
            };
            let p_open_content = &p_rem[..p_open_end];

            let p_name = extract_attribute(p_open_content, "name");

            let Some(p_close) = p_rem.find("</｜DSML｜parameter>") else {
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

        if !name.is_empty() {
            calls.push(ParsedDsmlCall {
                name,
                arguments: serde_json::Value::Object(params),
            });
        }

        search_pos = absolute_invoke_start + invoke_end + "</｜DSML｜invoke>".len();
    }

    calls
}

pub fn parse_dsml_tool_calls_detailed(text: &str) -> (Vec<ParsedDsmlCall>, bool) {
    let calls = parse_dsml_tool_calls(text);
    let invoke_open = text.matches("<｜DSML｜invoke").count();
    let invoke_close = text.matches("</｜DSML｜invoke>").count();
    let parameter_open = text.matches("<｜DSML｜parameter").count();
    let parameter_close = text.matches("</｜DSML｜parameter>").count();
    let malformed = invoke_open != invoke_close
        || parameter_open != parameter_close
        || calls.len() != invoke_open;
    (calls, malformed)
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
            strip_system_tags(&cleaned_text)
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
