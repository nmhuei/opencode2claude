//! Schema coercion and tool-name utilities.
//!
//! Coerces DSML parameter strings to the types declared by the matching
//! Anthropic tool schema, and provides tool-name matching and deduplication
//! helpers.

use super::header_parse::skip_compat_whitespace;
use super::json_repair::{normalize_compat_json_arguments, scan_json_value_end};
use super::{MAX_COMPAT_ARGUMENT_BYTES, MAX_COMPAT_BATCH_ITEMS};
use crate::handlers::MessagesRequest;

pub(crate) fn matching_tool_name(name: &str, payload: &MessagesRequest) -> Option<String> {
    payload.tools.as_ref().and_then(|tools| {
        tools
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(name))
            .map(|tool| tool.name.clone())
    })
}

/// Stable semantic identity for duplicate suppression within one assistant turn.
/// Tool names are case-insensitive and parsed JSON is serialized canonically by
/// serde_json's map representation, so native, DSML, reasoning, and text-marker
/// encodings of the same invocation collapse to one execution.
pub(crate) fn tool_call_fingerprint(name: &str, arguments: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(arguments).unwrap_or_else(|_| "null".to_string());
    format!("{}\u{0}{serialized}", name.to_ascii_lowercase())
}

pub(crate) fn looks_like_unverified_tool_success(text: &str) -> bool {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.len() > 4096 {
        return false;
    }
    [
        "scheduled successfully",
        "successfully scheduled",
        "has been scheduled",
        "have scheduled",
        "created successfully",
        "successfully created",
        "has been created",
        "have created",
        "completed successfully",
        "successfully completed",
        "has completed",
        "have completed",
        "ran successfully",
        "executed successfully",
        "updated successfully",
        "deleted successfully",
        "đã tạo",
        "đã chạy",
        "đã hoàn thành",
        "đã lên lịch",
        "đã schedule",
        "schedule rồi",
        "thành công",
        "hoàn tất",
        "xong rồi",
    ]
    .iter()
    .any(|claim| normalized.contains(claim))
}

#[cfg(test)]
pub(crate) fn get_correct_tool_name(name: &str, payload: &MessagesRequest) -> String {
    matching_tool_name(name, payload).unwrap_or_else(|| name.to_string())
}

pub(crate) fn invalid_semantic_tool_argument(
    name: &str,
    arguments: &serde_json::Value,
) -> Option<&'static str> {
    fn placeholder(value: Option<&serde_json::Value>) -> bool {
        let Some(text) = value.and_then(serde_json::Value::as_str) else {
            return true;
        };
        matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "" | "..." | "…" | "<prompt>" | "<message>" | "placeholder"
        )
    }

    match name.to_ascii_lowercase().as_str() {
        "agent" if placeholder(arguments.get("prompt")) => Some("prompt"),
        "sendmessage" if placeholder(arguments.get("message")) => Some("message"),
        _ => None,
    }
}

pub(crate) fn normalize_dsml_arguments(
    name: &str,
    arguments: serde_json::Value,
    payload: &MessagesRequest,
) -> serde_json::Value {
    let Some(tool) = payload.tools.as_ref().and_then(|tools| {
        tools
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(name))
    }) else {
        return arguments;
    };

    coerce_value_to_schema(arguments, &tool.input_schema)
}

fn coerce_value_to_schema(
    value: serde_json::Value,
    schema: &serde_json::Value,
) -> serde_json::Value {
    let expected_type = schema
        .get("type")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            schema
                .get("anyOf")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        item.get("type")
                            .and_then(serde_json::Value::as_str)
                            .filter(|kind| *kind != "null")
                    })
                })
        });

    match (expected_type, value) {
        (Some("string"), serde_json::Value::String(text)) => serde_json::Value::String(text),
        (Some("string"), other) => serde_json::Value::String(match other {
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Bool(value) => value.to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                serde_json::to_string(&other).unwrap_or_default()
            }
            serde_json::Value::String(_) => unreachable!(),
        }),
        (Some("boolean"), serde_json::Value::String(text)) => {
            match text.trim().to_ascii_lowercase().as_str() {
                "true" => serde_json::Value::Bool(true),
                "false" => serde_json::Value::Bool(false),
                _ => serde_json::Value::String(text),
            }
        }
        (Some("integer"), serde_json::Value::String(text)) => text
            .trim()
            .parse::<i64>()
            .map(serde_json::Number::from)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::String(text)),
        (Some("number"), serde_json::Value::String(text)) => text
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::String(text)),
        (Some("null"), serde_json::Value::String(text)) if text.trim() == "null" => {
            serde_json::Value::Null
        }
        (Some("array"), serde_json::Value::String(text)) => {
            match serde_json::from_str::<serde_json::Value>(text.trim()) {
                Ok(serde_json::Value::Array(items)) => {
                    let item_schema = schema.get("items").unwrap_or(&serde_json::Value::Null);
                    serde_json::Value::Array(
                        items
                            .into_iter()
                            .map(|item| coerce_value_to_schema(item, item_schema))
                            .collect(),
                    )
                }
                _ => serde_json::Value::String(text),
            }
        }
        (Some("array"), serde_json::Value::Array(items)) => {
            let item_schema = schema.get("items").unwrap_or(&serde_json::Value::Null);
            serde_json::Value::Array(
                items
                    .into_iter()
                    .map(|item| coerce_value_to_schema(item, item_schema))
                    .collect(),
            )
        }
        (Some("object"), serde_json::Value::String(text)) => {
            match serde_json::from_str::<serde_json::Value>(text.trim()) {
                Ok(serde_json::Value::Object(map)) => coerce_object(map, schema),
                _ => serde_json::Value::String(text),
            }
        }
        (Some("object"), serde_json::Value::Object(map)) => coerce_object(map, schema),
        (_, other) => other,
    }
}

fn coerce_object(
    mut map: serde_json::Map<String, serde_json::Value>,
    schema: &serde_json::Value,
) -> serde_json::Value {
    if let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
        for (key, value) in &mut map {
            if let Some(property_schema) = properties.get(key) {
                *value = coerce_value_to_schema(std::mem::take(value), property_schema);
            }
        }
    }
    serde_json::Value::Object(map)
}

/// Parse a bracket-delimited, comma-separated sequence of JSON argument
/// objects. Returns the parsed values and the byte offset past the closing
/// bracket.
pub(crate) fn parse_compat_argument_sequence(
    text: &str,
    arguments_start: usize,
    allow_missing_closing_bracket: bool,
) -> Option<(Vec<serde_json::Value>, usize)> {
    let mut cursor = arguments_start;
    let mut values = Vec::new();

    loop {
        cursor = skip_compat_whitespace(text, cursor);
        let relative_end = scan_json_value_end(text.get(cursor..)?)?;
        let arguments_end = cursor + relative_end;
        if arguments_end.saturating_sub(arguments_start) > MAX_COMPAT_ARGUMENT_BYTES
            || values.len() >= MAX_COMPAT_BATCH_ITEMS
        {
            return None;
        }
        let normalized = normalize_compat_json_arguments(&text[cursor..arguments_end])?;
        let arguments = serde_json::from_str::<serde_json::Value>(&normalized).ok()?;
        if !arguments.is_object() {
            return None;
        }
        values.push(arguments);
        cursor = skip_compat_whitespace(text, arguments_end);

        if text.get(cursor..).is_some_and(|rest| rest.starts_with(',')) {
            cursor += 1;
            continue;
        }
        let complete = text.get(cursor..).is_some_and(|rest| rest.starts_with(']'));
        let eof_complete = allow_missing_closing_bracket && cursor == text.len();
        if !complete && !eof_complete {
            return None;
        }
        if values.len() > 1 && !values.iter().all(serde_json::Value::is_object) {
            return None;
        }
        return Some((values, if complete { cursor + 1 } else { cursor }));
    }
}

/// Heuristic: does the raw text look like a comma-separated batch of
/// objects (i.e. `{...}, `{...}`)? Used to distinguish batch arguments from
/// nested JSON objects.
pub(crate) fn looks_like_object_batch_sequence(raw: &str) -> bool {
    let Some(first_end) = scan_json_value_end(raw) else {
        return false;
    };
    raw[first_end..]
        .trim_start()
        .strip_prefix(',')
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('{'))
}
