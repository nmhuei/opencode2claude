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

pub fn is_web_search_tool(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower == "websearch"
        || name_lower == "web_search"
        || name_lower == "webfetch"
        || name_lower == "web_fetch"
}

/// Extract the search query from tool call arguments.
///
/// Parses the JSON tool arguments and looks for common query fields:
/// "query" or "q", falling back to the first string field found.
pub fn extract_search_query(tool_args: &str) -> String {
    let input_val: serde_json::Value = serde_json::from_str(tool_args)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    if let Some(obj) = input_val.as_object() {
        if let Some(q_val) = obj.get("query").and_then(|v| v.as_str()) {
            return q_val.to_string();
        }
        if let Some(q_val) = obj.get("q").and_then(|v| v.as_str()) {
            return q_val.to_string();
        }
        for (_, v) in obj {
            if let Some(s) = v.as_str() {
                return s.to_string();
            }
        }
    }
    String::new()
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
