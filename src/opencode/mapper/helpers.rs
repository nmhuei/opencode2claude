//! Pure helpers used while translating Anthropic request fields.

pub fn extract_system_prompt(system_val: &serde_json::Value) -> String {
    if let Some(s) = system_val.as_str() {
        return s.to_string();
    }
    if let Some(arr) = system_val.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(obj) = item.as_object() {
                if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                        parts.push(text);
                    }
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

/// Tools governed by the external-web permission policy.
pub fn is_web_search_tool(name: &str) -> bool {
    is_bridge_search_tool(name)
        || matches!(name.to_ascii_lowercase().as_str(), "webfetch" | "web_fetch")
}

/// Search tools executed inside the bridge rather than by Claude Code.
///
/// `WebFetch` is deliberately excluded: it is a Claude Code client tool and
/// must be forwarded as a normal `tool_use` block so the client can fetch the
/// exact requested URL instead of receiving search-engine results.
pub fn is_bridge_search_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "websearch" | "web_search"
    )
}

/// Extract the search query from tool call arguments.
///
/// Parses the JSON tool arguments and looks for common query fields:
/// "query" or "q", falling back to the first string field found.
pub fn extract_search_query(tool_args: &str) -> String {
    let raw = tool_args.trim();
    if raw.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => extract_query_value(&value).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn extract_query_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => non_empty(text),
        serde_json::Value::Array(items) => items.iter().find_map(extract_query_value),
        serde_json::Value::Object(object) => {
            const PRIORITY_KEYS: &[&str] = &[
                "query",
                "q",
                "search_query",
                "searchQuery",
                "text",
                "prompt",
                "url",
            ];
            for key in PRIORITY_KEYS {
                if let Some(found) = object.get(*key).and_then(extract_query_value) {
                    return Some(found);
                }
            }
            object.values().find_map(extract_query_value)
        }
        _ => None,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

pub fn map_model_name(model: &str) -> String {
    let mut name = model.to_string();
    if name.starts_with("opencode/") {
        name = name["opencode/".len()..].to_string();
    }
    match name.as_str() {
        "deepseek-v4-flash" => "deepseek-v4-flash-free".to_string(),
        "nemotron-3-ultra" => "nemotron-3-ultra-free".to_string(),
        _ => name,
    }
}

pub fn tool_result_content_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let mut text_parts = Vec::new();
            for item in arr {
                if let Some(obj) = item.as_object() {
                    if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    } else {
                        text_parts.push(item.to_string());
                    }
                } else {
                    text_parts.push(item.to_string());
                }
            }
            text_parts.join("\n")
        }
        _ => val.to_string(),
    }
}
