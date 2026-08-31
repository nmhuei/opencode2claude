//! ASCII-insensitive header parsers and whitespace helpers for compatibility
//! tool markers.

pub(super) fn parse_compat_tool_intent_header_at(text: &str, start: usize) -> Option<usize> {
    let mut cursor = start;
    if !text.get(cursor..)?.starts_with('[') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    cursor = consume_ascii_case_insensitive(text, cursor, "requesting")?;
    cursor = consume_required_compat_whitespace(text, cursor)?;

    if let Some(after_tool) = consume_ascii_case_insensitive(text, cursor, "tool") {
        cursor = consume_required_compat_whitespace(text, after_tool)?;
    }

    let (next, allow_plural) =
        if let Some(next) = consume_ascii_case_insensitive(text, cursor, "execution") {
            (next, false)
        } else if let Some(next) = consume_ascii_case_insensitive(text, cursor, "invocation") {
            (next, false)
        } else {
            (consume_ascii_case_insensitive(text, cursor, "call")?, true)
        };
    cursor = next;
    if allow_plural {
        if let Some(after_plural) = consume_ascii_case_insensitive(text, cursor, "s") {
            cursor = after_plural;
        }
    }
    cursor = skip_compat_whitespace(text, cursor);
    if text.get(cursor..)?.starts_with(':') {
        return Some(cursor + 1);
    }

    // Some model variants use prose-shaped forms such as
    // `[Requesting tool call for Bash with parameters: {...}]` or
    // `[Requesting tool calls for 'Glob' with pattern "..."]:`. These do
    // not provide the canonical JSON contract, so recognize only the intent
    // and let the streaming layer request a safe retry instead of guessing.
    let after_for = consume_ascii_case_insensitive(text, cursor, "for")?;
    consume_required_compat_whitespace(text, after_for)
}

pub(super) fn parse_compat_tool_marker_header_at(text: &str, start: usize) -> Option<usize> {
    let mut cursor = start;
    if !text.get(cursor..)?.starts_with('[') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    cursor = consume_ascii_case_insensitive(text, cursor, "requesting")?;
    cursor = consume_required_compat_whitespace(text, cursor)?;

    if let Some(after_tool) = consume_ascii_case_insensitive(text, cursor, "tool") {
        cursor = consume_required_compat_whitespace(text, after_tool)?;
        cursor = consume_ascii_case_insensitive(text, cursor, "execution")?;
    } else {
        cursor = consume_ascii_case_insensitive(text, cursor, "execution")?;
    }

    cursor = skip_compat_whitespace(text, cursor);
    text.get(cursor..)?.starts_with(':').then_some(cursor + 1)
}

/// Parse the shorthand marker grammar emitted by some free models:
/// `[Requesting Read with arguments: {...}]`.
///
/// Unlike the canonical compatibility grammar, the tool name is unquoted and
/// there is no `Tool execution:` phrase. Requiring the complete `with
/// arguments:` clause keeps ordinary prose such as `[Requesting help]` from
/// being treated as a tool call.
pub(super) fn parse_compat_tool_shorthand_header_at(
    text: &str,
    start: usize,
) -> Option<(String, usize)> {
    let mut cursor = start;
    if !text.get(cursor..)?.starts_with('[') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    cursor = consume_ascii_case_insensitive(text, cursor, "requesting")?;
    cursor = consume_required_compat_whitespace(text, cursor)?;

    let name_start = cursor;
    for (offset, ch) in text[cursor..].char_indices() {
        if ch.is_whitespace() {
            cursor += offset;
            break;
        }
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')) {
            return None;
        }
    }
    if cursor == name_start || cursor >= text.len() {
        return None;
    }
    let name = text[name_start..cursor].trim().to_string();
    if name.is_empty()
        || ["tool", "execution", "invocation", "call", "calls"]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return None;
    }
    cursor = parse_compat_arguments_clause_at(text, cursor)?;
    Some((name, skip_compat_whitespace(text, cursor)))
}

/// Parse the compact marker grammar emitted by some clients/models:
/// `[Requesting CronCreate: {"cron":"*/30 * * * *",...}]`.
///
/// The immediate colon is mandatory. This keeps ordinary prose such as
/// `[Requesting approval from the user]` inert while supporting every tool
/// name present in the current request without hard-coding CronCreate.
pub(super) fn parse_compat_tool_direct_header_at(
    text: &str,
    start: usize,
) -> Option<(String, usize)> {
    let mut cursor = start;
    if !text.get(cursor..)?.starts_with('[') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    cursor = consume_ascii_case_insensitive(text, cursor, "requesting")
        .or_else(|| consume_ascii_case_insensitive(text, cursor, "creating"))?;
    cursor = consume_required_compat_whitespace(text, cursor)?;

    let name_start = cursor;
    while cursor < text.len() {
        let ch = text[cursor..].chars().next()?;
        if ch == ':' || ch.is_whitespace() {
            break;
        }
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')) {
            return None;
        }
        cursor += ch.len_utf8();
    }
    let name = text.get(name_start..cursor)?.trim().to_string();
    if name.is_empty()
        || ["tool", "execution", "invocation", "call", "calls"]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return None;
    }

    cursor = skip_compat_whitespace(text, cursor);
    if !text.get(cursor..)?.starts_with(':') {
        return None;
    }
    Some((name, skip_compat_whitespace(text, cursor + 1)))
}

pub(super) fn parse_compat_arguments_clause_at(text: &str, start: usize) -> Option<usize> {
    let mut cursor = skip_compat_whitespace(text, start);
    cursor = consume_ascii_case_insensitive(text, cursor, "with")?;
    cursor = consume_required_compat_whitespace(text, cursor)?;
    cursor = consume_ascii_case_insensitive(text, cursor, "arguments")
        .or_else(|| consume_ascii_case_insensitive(text, cursor, "argument"))
        .or_else(|| consume_ascii_case_insensitive(text, cursor, "args"))?;
    cursor = skip_compat_whitespace(text, cursor);
    text.get(cursor..)?.starts_with(':').then_some(cursor + 1)
}

pub(super) fn skip_compat_whitespace(text: &str, start: usize) -> usize {
    for (offset, ch) in text[start..].char_indices() {
        if !ch.is_whitespace() {
            return start + offset;
        }
    }
    text.len()
}

pub(super) fn consume_required_compat_whitespace(text: &str, start: usize) -> Option<usize> {
    let next = skip_compat_whitespace(text, start);
    (next > start).then_some(next)
}

pub(super) fn consume_ascii_case_insensitive(
    text: &str,
    start: usize,
    token: &str,
) -> Option<usize> {
    let end = start.checked_add(token.len())?;
    text.get(start..end)?
        .eq_ignore_ascii_case(token)
        .then_some(end)
}
