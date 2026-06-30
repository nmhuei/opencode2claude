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
        "</｜DSML｜invoke>",
        "<｜DSML｜invoke>",
        "</｜DSML｜tool_calls>",
        "<｜DSML｜tool_calls>",
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

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDsmlCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

fn extract_attribute(tag_content: &str, attr_name: &str) -> String {
    let pattern = attr_name.to_string();
    let mut pos = 0;
    while let Some(match_pos) = tag_content[pos..].find(&pattern) {
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

pub fn extract_and_clean_dsml(text: &str) -> (String, Vec<ParsedDsmlCall>) {
    let mut cleaned_text = String::new();
    let mut calls = Vec::new();
    let mut last_pos = 0;

    while let Some(start_pos) = text[last_pos..].find("<｜DSML｜tool_calls>") {
        let abs_start = last_pos + start_pos;
        cleaned_text.push_str(&text[last_pos..abs_start]);

        let rem = &text[abs_start..];
        if let Some(end_pos) = rem.find("</｜DSML｜tool_calls>") {
            let abs_end = abs_start + end_pos + "</｜DSML｜tool_calls>".len();
            let dsml_block = &text[abs_start..abs_end];
            let parsed_calls = parse_dsml_tool_calls(dsml_block);
            calls.extend(parsed_calls);
            last_pos = abs_end;
        } else {
            let dsml_block = &text[abs_start..];
            let parsed_calls = parse_dsml_tool_calls(dsml_block);
            calls.extend(parsed_calls);
            last_pos = text.len();
        }
    }
    cleaned_text.push_str(&text[last_pos..]);

    let final_cleaned = strip_system_tags(&cleaned_text);
    (final_cleaned, calls)
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
