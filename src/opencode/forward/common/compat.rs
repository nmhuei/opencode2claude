//! Compat markdown markers, TV/XML marker parsing, and marker extraction
//! for text-encoded compatibility tool calls.
//!
//! Free models frequently emit tool calls as text-encoded markers inside
//! markdown rather than using the native API tool-calling protocol. This
//! module detects, parses, and extracts those markers.

use super::header_parse::{
    consume_ascii_case_insensitive, parse_compat_arguments_clause_at,
    parse_compat_tool_direct_header_at, parse_compat_tool_intent_header_at,
    parse_compat_tool_marker_header_at, parse_compat_tool_shorthand_header_at,
    skip_compat_whitespace,
};
use super::json_repair::normalize_compat_json_arguments;
use super::schema::{looks_like_object_batch_sequence, parse_compat_argument_sequence};
use super::{MAX_COMPAT_ARGUMENT_BYTES, MAX_COMPAT_CALLS_PER_RESPONSE};
use crate::handlers::MessagesRequest;
use memchr::memchr_iter;

const XML_TOOLCALL_WRAPPER_TAGS: [&str; 3] = ["tvToolcalls", "tool_calls", "tool_call"];
const XML_INVOKE_TAGS: [&str; 2] = ["tvInvoke", "invoke"];
const XML_PARAMETER_TAGS: [&str; 2] = ["tvParameter", "parameter"];

type XmlAttributes = Vec<(String, String)>;
type ParsedXmlOpenTag = (usize, XmlAttributes, usize);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompatToolCall {
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedCompatMarker {
    pub(crate) prefix: String,
    pub(crate) calls: Vec<CompatToolCall>,
    pub(crate) consumed: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompatExtraction {
    pub(crate) cleaned_text: String,
    pub(crate) calls: Vec<(String, String)>,
    pub(crate) malformed_intent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompatMarkdownState {
    fence: Option<(char, usize)>,
    inline_ticks: Option<usize>,
    double_quoted: bool,
    escaped: bool,
    line_only_whitespace: bool,
    quoted_line: bool,
}

impl Default for CompatMarkdownState {
    fn default() -> Self {
        Self {
            fence: None,
            inline_ticks: None,
            double_quoted: false,
            escaped: false,
            line_only_whitespace: true,
            quoted_line: false,
        }
    }
}

impl CompatMarkdownState {
    pub(crate) fn is_executable_context(&self) -> bool {
        self.fence.is_none()
            && self.inline_ticks.is_none()
            && !self.double_quoted
            && !self.quoted_line
    }

    pub(crate) fn advance(&mut self, text: &str) {
        let mut cursor = 0;
        while cursor < text.len() {
            let ch = text[cursor..].chars().next().expect("valid UTF-8 boundary");
            let ch_len = ch.len_utf8();

            if ch == '\n' {
                self.line_only_whitespace = true;
                self.quoted_line = false;
                self.escaped = false;
                cursor += ch_len;
                continue;
            }

            if self.line_only_whitespace {
                if ch.is_whitespace() {
                    cursor += ch_len;
                    continue;
                }
                if ch == '>' {
                    self.quoted_line = true;
                }
                self.line_only_whitespace = false;
            }

            if let Some((delimiter, minimum)) = self.fence {
                if ch == delimiter {
                    let count = count_repeated_char(&text[cursor..], delimiter);
                    if count >= minimum {
                        self.fence = None;
                    }
                    cursor += count * delimiter.len_utf8();
                } else {
                    cursor += ch_len;
                }
                continue;
            }

            if self.inline_ticks.is_some() {
                if ch == '`' {
                    let count = count_repeated_char(&text[cursor..], '`');
                    if count >= self.inline_ticks.unwrap_or(usize::MAX) {
                        self.inline_ticks = None;
                    }
                    cursor += count;
                } else {
                    cursor += ch_len;
                }
                continue;
            }

            if self.double_quoted {
                if self.escaped {
                    self.escaped = false;
                } else if ch == '\\' {
                    self.escaped = true;
                } else if ch == '"' {
                    self.double_quoted = false;
                }
                cursor += ch_len;
                continue;
            }

            if ch == '"' {
                self.double_quoted = true;
                self.escaped = false;
                cursor += ch_len;
                continue;
            }

            if ch == '`' || ch == '~' {
                let count = count_repeated_char(&text[cursor..], ch);
                if count >= 3 {
                    self.fence = Some((ch, count));
                } else if ch == '`' {
                    self.inline_ticks = Some(count);
                }
                cursor += count * ch.len_utf8();
                continue;
            }

            cursor += ch_len;
        }
    }
}

fn count_repeated_char(text: &str, target: char) -> usize {
    text.chars().take_while(|ch| *ch == target).count()
}

#[cfg(test)]
pub(crate) fn parse_compat_tool_request(text: &str) -> Option<(String, String, String)> {
    parse_compat_tool_request_with_consumed(text)
        .map(|(name, arguments, prefix, _)| (name, arguments, prefix))
}

pub(crate) fn find_compat_tool_marker(text: &str) -> Option<usize> {
    let legacy = memchr_iter(b'[', text.as_bytes()).find_map(|start| {
        parse_compat_tool_marker_header_at(text, start)
            .map(|_| start)
            .or_else(|| parse_compat_tool_shorthand_header_at(text, start).map(|_| start))
            .or_else(|| parse_compat_tool_direct_header_at(text, start).map(|_| start))
    });
    let tv = find_tv_toolcalls_marker(text);
    match (legacy, tv) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(position), None) | (None, Some(position)) => Some(position),
        (None, None) => None,
    }
}

/// Find either a parseable execution marker or an instruction-shaped tool
/// intent marker that must be retained for EOF recovery/retry. Models have
/// emitted variants such as `[Requesting Tool invocation: Write file at ...]`.
pub(crate) fn find_compat_tool_marker_in_context(
    text: &str,
    initial_state: &CompatMarkdownState,
) -> Option<usize> {
    find_compat_marker_in_context(text, initial_state, false)
}

pub(crate) fn find_literal_marker_in_context(
    text: &str,
    marker: &str,
    initial_state: &CompatMarkdownState,
) -> Option<usize> {
    let mut state = initial_state.clone();
    let mut cursor = 0;
    while cursor < text.len() {
        if state.is_executable_context()
            && text[cursor..].starts_with(marker)
            && !is_escaped_marker(text, cursor)
        {
            return Some(cursor);
        }
        let ch = text[cursor..].chars().next().expect("valid UTF-8 boundary");
        let next = if ch == '`' || ch == '~' {
            cursor + count_repeated_char(&text[cursor..], ch) * ch.len_utf8()
        } else {
            cursor + ch.len_utf8()
        };
        state.advance(&text[cursor..next]);
        cursor = next;
    }
    None
}

pub(crate) fn find_compat_tool_intent_marker_in_context(
    text: &str,
    initial_state: &CompatMarkdownState,
) -> Option<usize> {
    find_compat_marker_in_context(text, initial_state, true)
}

fn find_compat_marker_in_context(
    text: &str,
    initial_state: &CompatMarkdownState,
    intent: bool,
) -> Option<usize> {
    let mut state = initial_state.clone();
    let mut cursor = 0;

    while cursor < text.len() {
        let ch = text[cursor..].chars().next().expect("valid UTF-8 boundary");
        if state.is_executable_context() && !is_escaped_marker(text, cursor) {
            let recognized = match ch {
                '[' if intent => {
                    parse_compat_tool_intent_header_at(text, cursor).is_some()
                        || parse_compat_tool_shorthand_header_at(text, cursor).is_some()
                        || parse_compat_tool_direct_header_at(text, cursor).is_some()
                }
                '[' => {
                    parse_compat_tool_marker_header_at(text, cursor).is_some()
                        || parse_compat_tool_shorthand_header_at(text, cursor).is_some()
                        || parse_compat_tool_direct_header_at(text, cursor).is_some()
                }
                '<' => {
                    parse_xml_open_tag_family_at(text, cursor, &XML_TOOLCALL_WRAPPER_TAGS).is_some()
                }
                _ => false,
            };
            if recognized {
                return Some(cursor);
            }
        }

        let next = if ch == '`' || ch == '~' {
            cursor + count_repeated_char(&text[cursor..], ch) * ch.len_utf8()
        } else {
            cursor + ch.len_utf8()
        };
        state.advance(&text[cursor..next]);
        cursor = next;
    }
    None
}

fn is_escaped_marker(text: &str, start: usize) -> bool {
    let slash_count = text[..start]
        .chars()
        .rev()
        .take_while(|ch| *ch == '\\')
        .count();
    slash_count % 2 == 1
}

/// Return the byte length of a suffix that could become a compatibility marker
/// after another streamed chunk arrives.
pub(crate) fn compat_tool_marker_pending_suffix_len(
    text: &str,
    payload: &MessagesRequest,
) -> usize {
    const NORMALIZED_HEADERS: [&str; 12] = [
        "[requestingtoolexecution:",
        "[requestingexecution:",
        "[requestingtoolinvocation:",
        "[requestinginvocation:",
        "[requestingtoolcall:",
        "[requestingtoolcalls:",
        "[requestingcalls:",
        "[requestingtoolcallfor",
        "[requestingtoolcallsfor",
        "[requestingcallfor",
        "[requestingcallsfor",
        "[creating",
    ];
    const ARGUMENT_TAILS: [&str; 3] = ["witharguments:", "withargument:", "withargs:"];

    let available_tools = payload
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    tool.name
                        .chars()
                        .filter(|ch| !ch.is_whitespace())
                        .map(|ch| ch.to_ascii_lowercase())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut longest = 0;
    for start in memchr_iter(b'[', text.as_bytes()) {
        let suffix = &text[start..];
        if suffix.len() > 96 {
            continue;
        }
        let normalized = suffix
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>();
        if normalized.is_empty() || normalized.contains(']') {
            continue;
        }
        let is_known_prefix = NORMALIZED_HEADERS
            .iter()
            .any(|header| header.starts_with(&normalized));
        let action_rest = normalized
            .strip_prefix("[requesting")
            .or_else(|| normalized.strip_prefix("[creating"));
        let is_direct_prefix = action_rest.is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .trim_end_matches(':')
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        });
        let is_shorthand_prefix = normalized.strip_prefix("[requesting").is_some_and(|rest| {
            rest.is_empty()
                || available_tools.iter().any(|tool| {
                    tool.starts_with(rest)
                        || rest.strip_prefix(tool).is_some_and(|tail| {
                            tail.is_empty()
                                || ":".starts_with(tail)
                                || tail.starts_with(':')
                                || ARGUMENT_TAILS.iter().any(|expected| {
                                    expected.starts_with(tail) || tail.starts_with(expected)
                                })
                        })
                })
        });
        if is_known_prefix || is_direct_prefix || is_shorthand_prefix {
            longest = longest.max(suffix.len());
        }
    }
    const XML_PREFIXES: [&str; 3] = ["<tvtoolcalls>", "<tool_calls>", "<tool_call>"];
    let max_xml_prefix_len = XML_PREFIXES
        .iter()
        .map(|prefix| prefix.len())
        .max()
        .unwrap_or(0);
    for start in memchr_iter(b'<', text.as_bytes()) {
        let suffix = &text[start..];
        if suffix.len() > max_xml_prefix_len || suffix.contains('>') {
            continue;
        }
        let normalized = suffix
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>();
        if !normalized.is_empty()
            && XML_PREFIXES
                .iter()
                .any(|prefix| prefix.starts_with(&normalized))
        {
            longest = longest.max(suffix.len());
        }
    }

    longest
}

fn find_tv_toolcalls_marker(text: &str) -> Option<usize> {
    memchr_iter(b'<', text.as_bytes()).find(|start| {
        parse_xml_open_tag_family_at(text, *start, &XML_TOOLCALL_WRAPPER_TAGS).is_some()
    })
}

fn parse_tv_toolcalls_marker(text: &str, start: usize) -> Option<ParsedCompatMarker> {
    let (body_start, wrapper_attributes, wrapper_tag_index) =
        parse_xml_open_tag_family_at(text, start, &XML_TOOLCALL_WRAPPER_TAGS)?;
    if !wrapper_attributes.is_empty() {
        return None;
    }
    let wrapper_tag = XML_TOOLCALL_WRAPPER_TAGS[wrapper_tag_index];
    let (body_end, consumed) = find_tv_close_tag(text, body_start, wrapper_tag)?;
    if body_end.saturating_sub(body_start) > MAX_COMPAT_ARGUMENT_BYTES {
        return None;
    }

    let mut calls = Vec::new();
    let mut cursor = body_start;
    while cursor < body_end {
        cursor = skip_compat_whitespace(text, cursor);
        if cursor >= body_end {
            break;
        }
        let (parameters_start, invoke_attributes, invoke_tag_index) =
            parse_xml_open_tag_family_at(text, cursor, &XML_INVOKE_TAGS)?;
        let name = tv_attribute(&invoke_attributes, "name")?.trim().to_string();
        if name.is_empty()
            || !invoke_attributes
                .iter()
                .all(|(attribute, _)| attribute.eq_ignore_ascii_case("name"))
        {
            return None;
        }
        let invoke_tag = XML_INVOKE_TAGS[invoke_tag_index];
        let (parameters_end, invoke_end) = find_tv_close_tag(text, parameters_start, invoke_tag)?;
        if parameters_end > body_end || invoke_end > body_end {
            return None;
        }
        let arguments = parse_tv_parameters(text, parameters_start, parameters_end)?;
        calls.push(CompatToolCall { name, arguments });
        if calls.len() > MAX_COMPAT_CALLS_PER_RESPONSE {
            return None;
        }
        cursor = invoke_end;
    }

    if calls.is_empty() || skip_compat_whitespace(text, cursor) != body_end {
        return None;
    }
    Some(ParsedCompatMarker {
        prefix: text[..start].trim().to_string(),
        calls,
        consumed,
    })
}

fn parse_tv_parameters(text: &str, start: usize, end: usize) -> Option<serde_json::Value> {
    let mut fields = serde_json::Map::new();
    let mut cursor = start;
    while cursor < end {
        cursor = skip_compat_whitespace(text, cursor);
        if cursor >= end {
            break;
        }
        let (value_start, attributes, parameter_tag_index) =
            parse_xml_open_tag_family_at(text, cursor, &XML_PARAMETER_TAGS)?;
        let name = tv_attribute(&attributes, "name")?.trim().to_string();
        if name.is_empty()
            || fields
                .keys()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        if !attributes.iter().all(|(attribute, _)| {
            attribute.eq_ignore_ascii_case("name") || attribute.eq_ignore_ascii_case("string")
        }) {
            return None;
        }
        let parameter_tag = XML_PARAMETER_TAGS[parameter_tag_index];
        let (value_end, parameter_end) = find_tv_close_tag(text, value_start, parameter_tag)?;
        if value_end > end || parameter_end > end {
            return None;
        }
        let decoded = decode_tv_parameter_text(&text[value_start..value_end])?;
        let value = match tv_attribute(&attributes, "string") {
            Some(raw) if raw.eq_ignore_ascii_case("true") => serde_json::Value::String(decoded),
            Some(raw) if raw.eq_ignore_ascii_case("false") => {
                serde_json::from_str(decoded.trim()).ok()?
            }
            Some(_) => return None,
            None => serde_json::Value::String(decoded),
        };
        fields.insert(name, value);
        cursor = parameter_end;
    }
    (skip_compat_whitespace(text, cursor) == end).then_some(serde_json::Value::Object(fields))
}

fn parse_xml_open_tag_family_at(
    text: &str,
    start: usize,
    tags: &[&str],
) -> Option<ParsedXmlOpenTag> {
    tags.iter().enumerate().find_map(|(index, tag)| {
        parse_tv_open_tag_at(text, start, tag)
            .map(|(body_start, attributes)| (body_start, attributes, index))
    })
}

fn parse_tv_open_tag_at(text: &str, start: usize, tag: &str) -> Option<(usize, XmlAttributes)> {
    let mut cursor = start;
    if !text.get(cursor..)?.starts_with('<') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    cursor = consume_ascii_case_insensitive(text, cursor, tag)?;
    if text
        .get(cursor..)?
        .chars()
        .next()
        .is_some_and(|ch| !ch.is_whitespace() && ch != '>')
    {
        return None;
    }
    let attributes_start = cursor;
    let mut quote = None;
    while cursor < text.len() {
        let ch = text[cursor..].chars().next()?;
        if let Some(expected) = quote {
            if ch == expected {
                quote = None;
            }
        } else {
            match ch {
                '\'' | '"' => quote = Some(ch),
                '>' => {
                    let attributes = parse_tv_attributes(&text[attributes_start..cursor])?;
                    return Some((cursor + 1, attributes));
                }
                '<' => return None,
                _ => {}
            }
        }
        cursor += ch.len_utf8();
    }
    None
}

fn parse_tv_attributes(raw: &str) -> Option<XmlAttributes> {
    let mut attributes = Vec::new();
    let mut cursor = 0;
    loop {
        cursor = skip_compat_whitespace(raw, cursor);
        if cursor == raw.len() {
            return Some(attributes);
        }
        if raw.get(cursor..)?.starts_with('/') {
            return None;
        }
        let name_start = cursor;
        while cursor < raw.len() {
            let ch = raw[cursor..].chars().next()?;
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')) {
                break;
            }
            cursor += ch.len_utf8();
        }
        if cursor == name_start {
            return None;
        }
        let name = raw[name_start..cursor].to_string();
        if attributes
            .iter()
            .any(|(existing, _): &(String, String)| existing.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        cursor = skip_compat_whitespace(raw, cursor);
        if !raw.get(cursor..)?.starts_with('=') {
            return None;
        }
        cursor += 1;
        cursor = skip_compat_whitespace(raw, cursor);
        let quote = raw.get(cursor..)?.chars().next()?;
        if !matches!(quote, '\'' | '"') {
            return None;
        }
        cursor += quote.len_utf8();
        let value_end = raw[cursor..].find(quote)?;
        let value = raw[cursor..cursor + value_end].to_string();
        cursor += value_end + quote.len_utf8();
        attributes.push((name, value));
    }
}

fn tv_attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(attribute, _)| attribute.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn find_tv_close_tag(text: &str, start: usize, tag: &str) -> Option<(usize, usize)> {
    for relative in memchr_iter(b'<', text.get(start..)?.as_bytes()) {
        let position = start + relative;
        if let Some(end) = parse_tv_close_tag_at(text, position, tag) {
            return Some((position, end));
        }
    }
    None
}

fn parse_tv_close_tag_at(text: &str, start: usize, tag: &str) -> Option<usize> {
    let mut cursor = start;
    if !text.get(cursor..)?.starts_with('<') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    if !text.get(cursor..)?.starts_with('/') {
        return None;
    }
    cursor += 1;
    cursor = skip_compat_whitespace(text, cursor);
    cursor = consume_ascii_case_insensitive(text, cursor, tag)?;
    cursor = skip_compat_whitespace(text, cursor);
    text.get(cursor..)?.starts_with('>').then_some(cursor + 1)
}

fn decode_tv_parameter_text(raw: &str) -> Option<String> {
    let mut output = String::with_capacity(raw.len());
    let mut cursor = 0;
    while let Some(relative_ampersand) = raw[cursor..].find('&') {
        let ampersand = cursor + relative_ampersand;
        output.push_str(&raw[cursor..ampersand]);
        let remainder = &raw[ampersand..];
        let named = [
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&amp;", '&'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ];
        if let Some((entity, value)) = named
            .iter()
            .find(|(entity, _)| remainder.starts_with(entity))
        {
            output.push(*value);
            cursor = ampersand + entity.len();
            continue;
        }
        let semicolon = remainder.find(';')?;
        let entity = &remainder[1..semicolon];
        let codepoint = entity
            .strip_prefix("#x")
            .or_else(|| entity.strip_prefix("#X"))
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .or_else(|| {
                entity
                    .strip_prefix('#')
                    .and_then(|digits| digits.parse::<u32>().ok())
            })?;
        output.push(char::from_u32(codepoint)?);
        cursor = ampersand + semicolon + 1;
    }
    output.push_str(&raw[cursor..]);
    Some(output)
}

/// Remove every complete compatibility tool marker from model text and return
/// parsed calls in their original order. Malformed markers remain visible, but
/// they no longer prevent a later valid marker from being recovered.
#[cfg(test)]
pub(crate) fn extract_compat_tool_requests(text: &str) -> (String, Vec<(String, String)>) {
    let extraction = extract_compat_tool_requests_detailed(text);
    (extraction.cleaned_text, extraction.calls)
}

pub(crate) fn extract_compat_tool_requests_detailed(text: &str) -> CompatExtraction {
    let mut cleaned = String::with_capacity(text.len());
    let mut calls = Vec::new();
    let mut offset = 0;
    let mut markdown_state = CompatMarkdownState::default();
    let mut malformed_intent = false;

    while let Some(relative_marker) =
        find_compat_tool_marker_in_context(&text[offset..], &markdown_state)
    {
        let marker_pos = offset + relative_marker;
        let safe_prefix = &text[offset..marker_pos];
        cleaned.push_str(safe_prefix);
        markdown_state.advance(safe_prefix);

        let marker_text = &text[marker_pos..];
        if let Some(parsed) = parse_compat_tool_requests_with_consumed(marker_text) {
            if !parsed.prefix.is_empty() {
                cleaned.push_str(&parsed.prefix);
                markdown_state.advance(&parsed.prefix);
            }
            if calls.len().saturating_add(parsed.calls.len()) > MAX_COMPAT_CALLS_PER_RESPONSE {
                malformed_intent = true;
                cleaned.push_str(marker_text);
                offset = text.len();
                break;
            }
            calls.extend(parsed.calls.into_iter().filter_map(|call| {
                serde_json::to_string(&call.arguments)
                    .ok()
                    .map(|arguments| (call.name, arguments))
            }));
            offset = marker_pos + parsed.consumed;
        } else {
            malformed_intent = true;
            // Preserve malformed input and advance by one valid UTF-8 character
            // so a later valid marker can still be recovered.
            let first = marker_text
                .chars()
                .next()
                .expect("marker starts with a character");
            let end = marker_pos + first.len_utf8();
            cleaned.push(first);
            markdown_state.advance(&text[marker_pos..end]);
            offset = end;
        }
    }

    cleaned.push_str(&text[offset..]);
    if find_compat_tool_intent_marker_in_context(&cleaned, &CompatMarkdownState::default())
        .is_some()
    {
        malformed_intent = true;
    }

    CompatExtraction {
        cleaned_text: cleaned,
        calls,
        malformed_intent,
    }
}

pub(crate) fn parse_compat_tool_requests_with_consumed(text: &str) -> Option<ParsedCompatMarker> {
    parse_compat_tool_requests_impl(text, false)
}

pub(crate) fn parse_compat_tool_requests_at_eof(text: &str) -> Option<ParsedCompatMarker> {
    parse_compat_tool_requests_impl(text, true)
}

#[cfg(test)]
pub(crate) fn parse_compat_tool_request_with_consumed(
    text: &str,
) -> Option<(String, String, String, usize)> {
    let parsed = parse_compat_tool_requests_with_consumed(text)?;
    if parsed.calls.len() != 1 {
        return None;
    }
    let call = parsed.calls.into_iter().next()?;
    Some((
        call.name,
        serde_json::to_string(&call.arguments).ok()?,
        parsed.prefix,
        parsed.consumed,
    ))
}

/// Legacy single-call EOF view retained for focused parser tests.
#[cfg(test)]
pub(crate) fn parse_compat_tool_request_at_eof(
    text: &str,
) -> Option<(String, String, String, usize)> {
    let parsed = parse_compat_tool_requests_at_eof(text)?;
    if parsed.calls.len() != 1 {
        return None;
    }
    let call = parsed.calls.into_iter().next()?;
    Some((
        call.name,
        serde_json::to_string(&call.arguments).ok()?,
        parsed.prefix,
        parsed.consumed,
    ))
}

fn parse_compat_tool_requests_impl(
    text: &str,
    allow_missing_closing_bracket: bool,
) -> Option<ParsedCompatMarker> {
    let start = find_compat_tool_marker(text)?;
    if parse_xml_open_tag_family_at(text, start, &XML_TOOLCALL_WRAPPER_TAGS).is_some() {
        return parse_tv_toolcalls_marker(text, start);
    }
    let (name, arguments_start) = if let Some((name, arguments_start)) =
        parse_compat_tool_shorthand_header_at(text, start)
    {
        (name, arguments_start)
    } else if let Some((name, arguments_start)) = parse_compat_tool_direct_header_at(text, start) {
        (name, arguments_start)
    } else {
        let mut cursor = parse_compat_tool_marker_header_at(text, start)?;
        cursor = skip_compat_whitespace(text, cursor);

        let quote = text.get(cursor..)?.chars().next()?;
        if !matches!(quote, '\'' | '"' | '`') {
            return None;
        }
        cursor += quote.len_utf8();

        let name_end = text[cursor..].find(quote)?;
        let name = text[cursor..cursor + name_end].trim().to_string();
        cursor += name_end + quote.len_utf8();
        cursor = parse_compat_arguments_clause_at(text, cursor)?;
        (name, skip_compat_whitespace(text, cursor))
    };
    let prefix = text[..start].trim().to_string();
    if name.is_empty() {
        return None;
    }

    if let Some((values, consumed)) =
        parse_compat_argument_sequence(text, arguments_start, allow_missing_closing_bracket)
    {
        let calls = values
            .into_iter()
            .map(|arguments| CompatToolCall {
                name: name.clone(),
                arguments,
            })
            .collect();
        return Some(ParsedCompatMarker {
            prefix,
            calls,
            consumed,
        });
    }

    // Recovery is deliberately single-call only. A malformed comma-separated
    // batch is fail-closed because executing a valid prefix before retry could
    // duplicate side effects.
    for (relative_end, ch) in text[arguments_start..].char_indices() {
        if ch != ']' {
            continue;
        }
        let raw_end = arguments_start + relative_end;
        if raw_end.saturating_sub(arguments_start) > MAX_COMPAT_ARGUMENT_BYTES {
            break;
        }
        let raw = text[arguments_start..raw_end].trim_end();
        if looks_like_object_batch_sequence(raw)
            || find_compat_tool_marker_in_context(raw, &CompatMarkdownState::default()).is_some()
        {
            continue;
        }
        if let Some(arguments) = normalize_compat_json_arguments(raw)
            .and_then(|normalized| serde_json::from_str::<serde_json::Value>(&normalized).ok())
            .filter(serde_json::Value::is_object)
        {
            return Some(ParsedCompatMarker {
                prefix,
                calls: vec![CompatToolCall { name, arguments }],
                consumed: arguments_start + relative_end + ch.len_utf8(),
            });
        }
    }

    if allow_missing_closing_bracket {
        let raw = text[arguments_start..].trim_end();
        if !looks_like_object_batch_sequence(raw)
            && find_compat_tool_marker_in_context(raw, &CompatMarkdownState::default()).is_none()
        {
            if let Some(arguments) = normalize_compat_json_arguments(raw)
                .and_then(|normalized| serde_json::from_str::<serde_json::Value>(&normalized).ok())
                .filter(serde_json::Value::is_object)
            {
                return Some(ParsedCompatMarker {
                    prefix,
                    calls: vec![CompatToolCall { name, arguments }],
                    consumed: text.len(),
                });
            }
        }
    }

    None
}
