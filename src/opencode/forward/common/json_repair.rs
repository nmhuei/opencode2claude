//! JSON normalization and repair for compatibility tool argument parsing.
//!
//! Free models frequently emit JSON that is not strictly valid — unescaped
//! backslashes, raw control characters, or detached object fragments after
//! the main argument object. These functions repair common malformations
//! without rejecting correct input.

pub(super) fn normalize_compat_json_arguments(raw: &str) -> Option<String> {
    let escaped_controls = escape_json_control_chars(raw);
    if let Ok(strict) = serde_json::from_str::<serde_json::Value>(&escaped_controls) {
        return serde_json::to_string(&strict).ok();
    }

    let repaired_quotes = repair_unescaped_json_string_quotes(raw);
    let repaired_escapes = repair_invalid_json_string_escapes(&repaired_quotes);
    let repaired = escape_json_control_chars(&repaired_escapes);

    let mut candidates = Vec::<serde_json::Value>::new();
    for candidate in [
        normalize_detached_object_fields(&escaped_controls),
        Some(repaired.clone()),
        normalize_detached_object_fields(&repaired),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate) {
            if !candidates.iter().any(|existing| existing == &value) {
                candidates.push(value);
            }
        }
    }

    if candidates.len() != 1 {
        return None;
    }
    serde_json::to_string(&candidates.remove(0)).ok()
}

/// Recover a model-specific shape where additional fields are emitted after
/// the argument object, for example:
///
/// `{"command":"...","description":"useful"}, "description": null}`
///
/// The detached suffix must itself parse as an object fragment. Existing
/// non-null fields in the real argument object win, so a stray duplicate null
/// cannot erase a useful command description.
fn normalize_detached_object_fields(text: &str) -> Option<String> {
    let first_end = scan_json_value_end(text)?;
    let mut base = serde_json::from_str::<serde_json::Value>(&text[..first_end]).ok()?;
    let base_fields = base.as_object_mut()?;

    let suffix = text[first_end..].trim();
    let detached = suffix.strip_prefix(',')?.trim_start();
    let detached_json = format!("{{{detached}");
    let detached_value = serde_json::from_str::<serde_json::Value>(&detached_json).ok()?;
    let detached_fields = detached_value.as_object()?;

    for (key, value) in detached_fields {
        match base_fields.get(key) {
            Some(existing) if !existing.is_null() => {}
            _ => {
                base_fields.insert(key.clone(), value.clone());
            }
        }
    }

    serde_json::to_string(&base).ok()
}

/// Preserve shell and regex backslashes that are not valid JSON escapes.
///
/// Free models commonly emit commands such as `grep 'a\|b'`, `sed '\.bak$'`,
/// or Windows-style paths directly inside a JSON string. JSON only permits
/// `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, and `\uXXXX`; every
/// other backslash must itself be escaped on the wire. This pass doubles only
/// invalid string escapes so serde decodes the original command unchanged.
fn repair_invalid_json_string_escapes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut in_string = false;
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];
        if !in_string {
            output.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            output.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        if ch != '\\' {
            output.push(ch);
            index += 1;
            continue;
        }

        let Some(next) = chars.get(index + 1).copied() else {
            output.push_str("\\\\");
            index += 1;
            continue;
        };

        let valid_simple = matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't');
        let valid_unicode = next == 'u'
            && chars.get(index + 2..index + 6).is_some_and(|digits| {
                digits.len() == 4 && digits.iter().all(|digit| digit.is_ascii_hexdigit())
            });

        if valid_simple || valid_unicode {
            output.push('\\');
        } else {
            output.push_str("\\\\");
        }
        output.push(next);
        index += 2;
    }

    output
}

/// Repair quotes that a free model placed literally inside a JSON string.
///
/// A quote closes a JSON string only when the next non-whitespace character is
/// a JSON delimiter. Other unescaped quotes are treated as literal string
/// content and escaped. Correct JSON bypasses this function entirely.
fn repair_unescaped_json_string_quotes(text: &str) -> String {
    #[derive(Clone, Copy)]
    enum Container {
        Object { expecting_key: bool },
        Array,
    }

    let mut output = String::with_capacity(text.len());
    let mut containers = Vec::<Container>::new();
    let mut in_string = false;
    let mut string_is_key = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if !in_string {
            match ch {
                '{' => containers.push(Container::Object {
                    expecting_key: true,
                }),
                '[' => containers.push(Container::Array),
                '}' | ']' => {
                    containers.pop();
                }
                ':' => {
                    if let Some(Container::Object { expecting_key }) = containers.last_mut() {
                        *expecting_key = false;
                    }
                }
                ',' => {
                    if let Some(Container::Object { expecting_key }) = containers.last_mut() {
                        *expecting_key = true;
                    }
                }
                '"' => {
                    string_is_key = matches!(
                        containers.last(),
                        Some(Container::Object {
                            expecting_key: true
                        })
                    );
                    in_string = true;
                }
                _ => {}
            }
            output.push(ch);
            continue;
        }

        if escaped {
            output.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                output.push(ch);
                escaped = true;
            }
            '"' => {
                let next = text[index + ch.len_utf8()..]
                    .chars()
                    .find(|candidate| !candidate.is_whitespace());
                let closes_string = if string_is_key {
                    next == Some(':')
                } else {
                    matches!(next, None | Some(',') | Some('}') | Some(']'))
                };
                if closes_string {
                    output.push(ch);
                    in_string = false;
                } else {
                    output.push_str("\\\"");
                }
            }
            _ => output.push(ch),
        }
    }

    output
}

/// Locate the end of one JSON value while tolerating raw control characters
/// inside quoted strings. Some free models emit multiline tool prompts without
/// escaping their newlines, which is invalid strict JSON but otherwise
/// unambiguous on the compatibility wire format.
pub(super) fn scan_json_value_end(text: &str) -> Option<usize> {
    let first = text.chars().next()?;
    match first {
        '{' | '[' => {
            let mut stack = Vec::new();
            let mut in_string = false;
            let mut escaped = false;
            for (index, ch) in text.char_indices() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_string = false;
                    }
                    continue;
                }

                match ch {
                    '"' => in_string = true,
                    '{' => stack.push('}'),
                    '[' => stack.push(']'),
                    '}' | ']' => {
                        if stack.pop()? != ch {
                            return None;
                        }
                        if stack.is_empty() {
                            return Some(index + ch.len_utf8());
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        '"' => {
            let mut escaped = false;
            for (index, ch) in text.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    return Some(index + ch.len_utf8());
                }
            }
            None
        }
        _ => text
            .char_indices()
            .find_map(|(index, ch)| (ch.is_whitespace() || ch == ']').then_some(index))
            .or(Some(text.len())),
    }
}

fn escape_json_control_chars(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if !in_string {
            output.push(ch);
            if ch == '"' {
                in_string = true;
            }
            continue;
        }

        if escaped {
            match ch {
                '\n' => output.push('n'),
                '\r' => output.push('r'),
                '\t' => output.push('t'),
                value if value <= '\u{001f}' => {
                    output.pop();
                    output.push_str(&format!("\\u{:04x}", value as u32));
                }
                _ => output.push(ch),
            }
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                output.push(ch);
                escaped = true;
            }
            '"' => {
                output.push(ch);
                in_string = false;
            }
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            value if value <= '\u{001f}' => {
                output.push_str(&format!("\\u{:04x}", value as u32));
            }
            _ => output.push(ch),
        }
    }
    output
}
