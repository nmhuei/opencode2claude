//! Helpers shared by synchronous and streaming forwarding paths.

use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::mapper::{extract_search_query, is_web_search_tool};
use memchr::memchr_iter;
use reqwest::Client;

const MAX_COMPAT_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_COMPAT_BATCH_ITEMS: usize = 32;
const MAX_COMPAT_CALLS_PER_RESPONSE: usize = 128;
const XML_TOOLCALL_WRAPPER_TAGS: [&str; 3] = ["tvToolcalls", "tool_calls", "tool_call"];
const XML_INVOKE_TAGS: [&str; 2] = ["tvInvoke", "invoke"];
const XML_PARAMETER_TAGS: [&str; 2] = ["tvParameter", "parameter"];

type XmlAttributes = Vec<(String, String)>;
type ParsedXmlOpenTag = (usize, XmlAttributes, usize);

/// Check if the OpenCode daemon is running and reachable.
pub async fn check_daemon(client: &Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/doc", port);
    client
        .get(&url)
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .is_ok()
}

/// Inject search results into the conversation history (both sync and stream paths).
///
/// Appends an assistant turn (with thinking, text, and tool_use blocks) followed by
/// a tool_result turn. Used by `forward_to_llm_sync` and `forward_to_llm_stream`
/// after intercepting a web search tool call.
pub(super) fn inject_search_results(
    payload: &mut MessagesRequest,
    search_results: &str,
    thinking: &str,
    text: &str,
    search_tc_id: &str,
    search_tc_name: &str,
    search_tc_input: &serde_json::Value,
) {
    let mut assistant_content = Vec::new();

    if !thinking.is_empty() {
        assistant_content.push(
            serde_json::from_value(serde_json::json!({
                "type": "text",
                "text": format!("<thinking>{}</thinking>", thinking)
            }))
            .unwrap(),
        );
    }
    if !text.is_empty() {
        assistant_content.push(
            serde_json::from_value(serde_json::json!({
                "type": "text",
                "text": text
            }))
            .unwrap(),
        );
    }
    assistant_content.push(
        serde_json::from_value(serde_json::json!({
            "type": "tool_use",
            "id": search_tc_id,
            "name": search_tc_name,
            "input": search_tc_input
        }))
        .unwrap(),
    );

    payload.messages.push(crate::handlers::Message {
        role: "assistant".to_string(),
        content: ContentVal::Multiple(assistant_content),
    });

    // Append tool response turn
    let tool_result_content = vec![serde_json::from_value(serde_json::json!({
        "type": "tool_result",
        "tool_use_id": search_tc_id,
        "name": search_tc_name,
        "content": search_results
    }))
    .unwrap()];
    payload.messages.push(crate::handlers::Message {
        role: "user".to_string(),
        content: ContentVal::Multiple(tool_result_content),
    });
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CompatToolCall {
    pub(super) name: String,
    pub(super) arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ParsedCompatMarker {
    pub(super) prefix: String,
    pub(super) calls: Vec<CompatToolCall>,
    pub(super) consumed: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CompatExtraction {
    pub(super) cleaned_text: String,
    pub(super) calls: Vec<(String, String)>,
    pub(super) malformed_intent: bool,
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
pub(super) fn parse_compat_tool_request(text: &str) -> Option<(String, String, String)> {
    parse_compat_tool_request_with_consumed(text)
        .map(|(name, arguments, prefix, _)| (name, arguments, prefix))
}

/// Find the next text-encoded compatibility tool marker.
///
/// Free models do not reproduce the bridge hint byte-for-byte reliably. Real
/// responses vary capitalization and whitespace, and some omit the word
/// `Tool` entirely (`[Requesting Execution: ...]`). Detection therefore uses a
/// small ASCII-insensitive grammar instead of one hard-coded string.
pub(super) fn find_compat_tool_marker(text: &str) -> Option<usize> {
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
pub(super) fn find_compat_tool_marker_in_context(
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

pub(super) fn find_compat_tool_intent_marker_in_context(
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
pub(super) fn compat_tool_marker_pending_suffix_len(
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

fn parse_compat_tool_intent_header_at(text: &str, start: usize) -> Option<usize> {
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

fn parse_compat_tool_marker_header_at(text: &str, start: usize) -> Option<usize> {
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
fn parse_compat_tool_shorthand_header_at(text: &str, start: usize) -> Option<(String, usize)> {
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
fn parse_compat_tool_direct_header_at(text: &str, start: usize) -> Option<(String, usize)> {
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

fn parse_compat_arguments_clause_at(text: &str, start: usize) -> Option<usize> {
    let mut cursor = skip_compat_whitespace(text, start);
    cursor = consume_ascii_case_insensitive(text, cursor, "with")?;
    cursor = consume_required_compat_whitespace(text, cursor)?;
    cursor = consume_ascii_case_insensitive(text, cursor, "arguments")
        .or_else(|| consume_ascii_case_insensitive(text, cursor, "argument"))
        .or_else(|| consume_ascii_case_insensitive(text, cursor, "args"))?;
    cursor = skip_compat_whitespace(text, cursor);
    text.get(cursor..)?.starts_with(':').then_some(cursor + 1)
}

fn skip_compat_whitespace(text: &str, start: usize) -> usize {
    for (offset, ch) in text[start..].char_indices() {
        if !ch.is_whitespace() {
            return start + offset;
        }
    }
    text.len()
}

fn consume_required_compat_whitespace(text: &str, start: usize) -> Option<usize> {
    let next = skip_compat_whitespace(text, start);
    (next > start).then_some(next)
}

fn consume_ascii_case_insensitive(text: &str, start: usize, token: &str) -> Option<usize> {
    let end = start.checked_add(token.len())?;
    text.get(start..end)?
        .eq_ignore_ascii_case(token)
        .then_some(end)
}

/// Remove every complete compatibility tool marker from model text and return
/// parsed calls in their original order. Malformed markers remain visible, but
/// they no longer prevent a later valid marker from being recovered.
#[cfg(test)]
pub(super) fn extract_compat_tool_requests(text: &str) -> (String, Vec<(String, String)>) {
    let extraction = extract_compat_tool_requests_detailed(text);
    (extraction.cleaned_text, extraction.calls)
}

pub(super) fn extract_compat_tool_requests_detailed(text: &str) -> CompatExtraction {
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

pub(super) fn parse_compat_tool_requests_with_consumed(text: &str) -> Option<ParsedCompatMarker> {
    parse_compat_tool_requests_impl(text, false)
}

pub(super) fn parse_compat_tool_requests_at_eof(text: &str) -> Option<ParsedCompatMarker> {
    parse_compat_tool_requests_impl(text, true)
}

/// Legacy single-call view retained for focused parser tests. Batch-aware
/// production consumers use `parse_compat_tool_requests_*` directly.
#[cfg(test)]
pub(super) fn parse_compat_tool_request_with_consumed(
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
pub(super) fn parse_compat_tool_request_at_eof(
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

fn parse_compat_argument_sequence(
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
        values.push(serde_json::from_str::<serde_json::Value>(&normalized).ok()?);
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

fn looks_like_object_batch_sequence(raw: &str) -> bool {
    let Some(first_end) = scan_json_value_end(raw) else {
        return false;
    };
    raw[first_end..]
        .trim_start()
        .strip_prefix(',')
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('{'))
}

fn normalize_compat_json_arguments(raw: &str) -> Option<String> {
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
fn scan_json_value_end(text: &str) -> Option<usize> {
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

pub(super) fn resolve_search_query(tool_args: &str, payload: &MessagesRequest) -> (String, bool) {
    let extracted = extract_search_query(tool_args);
    if !extracted.trim().is_empty() {
        return (bound_search_query(&extracted), false);
    }
    let fallback = latest_user_text(payload)
        .map(|text| bound_search_query(&text))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "current user request".to_string());
    (fallback, true)
}

pub(super) fn normalize_search_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(super) fn prepare_compat_tool_retry(payload: &mut MessagesRequest) {
    let available_tools = payload
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| "none".to_string());
    append_system_instruction(
        payload,
        &format!(
            "Your previous response attempted a tool call using an unsupported, incomplete, ambiguous, batched-search, or prose-only marker. Re-evaluate the current task and reissue every intended tool call using the exact compatibility form `[Requesting Tool execution: 'ToolName' with arguments: {{complete JSON object}}]`. Emit exactly one marker per invocation; never place comma-separated argument objects in one marker. Use only tools available in this request: {available_tools}. Do not use `Requesting Tool invocation`, `Requesting tool call(s) for ...`, `with parameters`, `Write file at`, positional function syntax, prose placeholders, omitted arguments, or tool markers inside code blocks. For Write, include both `file_path` and the complete `content`. Emit real tool calls now instead of describing them."
        ),
    );
}

pub(super) fn prepare_final_search_synthesis(payload: &mut MessagesRequest, reason: &str) {
    if let Some(tools) = payload.tools.as_mut() {
        tools.retain(|tool| !is_web_search_tool(&tool.name));
        if tools.is_empty() {
            payload.tools = None;
        }
    }
    payload.tool_choice = None;
    append_system_instruction(
        payload,
        &format!(
            "Web research is complete ({reason}). Do not call WebSearch or WebFetch again. Use the search tool results already present in the conversation and provide the best complete final answer now. If some evidence is missing, state the limitation instead of requesting another search."
        ),
    );
}

pub(super) fn search_results_with_instruction(results: &str, final_turn: bool) -> String {
    if final_turn {
        format!(
            "{results}\n\n[Bridge instruction: Search budget is complete. Synthesize the final answer from these and all earlier results; do not call WebSearch or WebFetch again.]"
        )
    } else {
        results.to_string()
    }
}

fn latest_user_text(payload: &MessagesRequest) -> Option<String> {
    payload.messages.iter().rev().find_map(|message| {
        if message.role != "user" {
            return None;
        }
        match &message.content {
            ContentVal::Single(text) => non_empty_text(text),
            ContentVal::Multiple(blocks) => {
                let text = blocks
                    .iter()
                    .filter(|block| block.content_type == "text")
                    .filter_map(|block| block.text.as_deref())
                    .collect::<Vec<_>>()
                    .join(" ");
                non_empty_text(&text)
            }
        }
    })
}

fn non_empty_text(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn bound_search_query(query: &str) -> String {
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.chars().take(512).collect()
}

fn append_system_instruction(payload: &mut MessagesRequest, instruction: &str) {
    match payload.system.as_mut() {
        Some(serde_json::Value::String(existing)) => {
            if !existing.is_empty() {
                existing.push_str("\n\n");
            }
            existing.push_str(instruction);
        }
        Some(serde_json::Value::Array(parts)) => parts.push(serde_json::json!({
            "type": "text",
            "text": instruction
        })),
        Some(other) => {
            let previous = other.clone();
            *other = serde_json::json!([
                {"type":"text","text":previous.to_string()},
                {"type":"text","text":instruction}
            ]);
        }
        None => payload.system = Some(serde_json::Value::String(instruction.to_string())),
    }
}

pub(super) fn matching_tool_name(name: &str, payload: &MessagesRequest) -> Option<String> {
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
pub(super) fn tool_call_fingerprint(name: &str, arguments: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(arguments).unwrap_or_else(|_| "null".to_string());
    format!("{}\u{0}{serialized}", name.to_ascii_lowercase())
}

/// Split visible narration that arrived in the same upstream response as a
/// tool call. Some free providers stop the content field in the middle of a
/// word or sentence when switching to `tool_calls`. Anthropic responses are
/// append-only, so emit only through the last trustworthy sentence boundary
/// and omit the unfinished tail rather than exposing corrupted narration.
pub(super) fn split_completed_pre_tool_text(text: &str) -> (&str, &str) {
    fn is_closing_punctuation(ch: char) -> bool {
        matches!(ch, '"' | '\'' | ')' | ']' | '}' | '»' | '”' | '’')
    }

    let mut completed = 0usize;
    let mut cursor = 0usize;
    while cursor < text.len() {
        let ch = text[cursor..]
            .chars()
            .next()
            .expect("cursor remains on a UTF-8 boundary");
        let next = cursor + ch.len_utf8();

        if ch == '\n' {
            completed = next;
            cursor = next;
            continue;
        }

        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？') {
            let mut boundary = next;
            while boundary < text.len() {
                let trailing = text[boundary..]
                    .chars()
                    .next()
                    .expect("boundary remains on a UTF-8 boundary");
                if is_closing_punctuation(trailing) {
                    boundary += trailing.len_utf8();
                } else {
                    break;
                }
            }

            if boundary == text.len()
                || text[boundary..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
            {
                while boundary < text.len() {
                    let trailing = text[boundary..]
                        .chars()
                        .next()
                        .expect("boundary remains on a UTF-8 boundary");
                    if trailing.is_whitespace() {
                        boundary += trailing.len_utf8();
                    } else {
                        break;
                    }
                }
                completed = boundary;
            }
        }

        cursor = next;
    }

    text.split_at(completed)
}

/// Detect short claims that assert a side effect already succeeded before the
/// client has returned a tool_result. Such text is held while streaming and
/// omitted from a tool-use turn; the model can report success on the next turn
/// only after observing the actual result.
pub(super) fn looks_like_unverified_tool_success(text: &str) -> bool {
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
pub(super) fn get_correct_tool_name(name: &str, payload: &MessagesRequest) -> String {
    matching_tool_name(name, payload).unwrap_or_else(|| name.to_string())
}

/// Coerce DSML parameter strings to the types declared by the matching
/// Anthropic tool schema. The DSML wire format itself is text-first, so this
/// final schema-aware step avoids both `"true"`-instead-of-`true` bugs and the
/// opposite problem where string commands that look like JSON are over-parsed.
pub(super) fn invalid_semantic_tool_argument(
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

pub(super) fn normalize_dsml_arguments(
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

pub fn estimate_string_tokens(text: &str) -> u32 {
    let mut tokens: f32 = 0.0;
    let mut in_word = false;

    for c in text.chars() {
        if c.is_whitespace() {
            tokens += 0.25;
            in_word = false;
        } else if c.is_ascii_alphanumeric() {
            if !in_word {
                tokens += 1.0;
                in_word = true;
            } else {
                tokens += 0.22;
            }
        } else {
            tokens += 0.5;
            in_word = false;
        }
    }
    tokens.round() as u32
}

pub fn estimate_input_tokens(payload: &MessagesRequest) -> u32 {
    let mut total_tokens = 0;
    if let Some(ref sys) = payload.system {
        total_tokens += estimate_string_tokens(&sys.to_string());
    }
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => total_tokens += estimate_string_tokens(text),
            ContentVal::Multiple(blocks) => {
                for b in blocks {
                    if let Some(ref text) = b.text {
                        total_tokens += estimate_string_tokens(text);
                    }
                    if let Some(ref input) = b.input {
                        total_tokens += estimate_string_tokens(&input.to_string());
                    }
                    if let Some(ref content) = b.content {
                        total_tokens += estimate_string_tokens(&content.to_string());
                    }
                }
            }
        }
    }
    if total_tokens == 0 {
        100
    } else {
        total_tokens
    }
}

/// Read an upstream response body without allowing unbounded allocation.
pub(super) async fn read_bounded_body(
    response: crate::opencode::retry::LeasedResponse,
    max_bytes: usize,
) -> Result<Vec<u8>, crate::error::BridgeError> {
    use futures_util::StreamExt;

    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            crate::error::BridgeError::UpstreamError(format!(
                "Failed reading upstream response: {error}"
            ))
        })?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(crate::error::BridgeError::UpstreamError(format!(
                "Upstream response exceeded configured limit of {max_bytes} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod dsml_argument_tests {
    use super::*;
    use crate::handlers::AnthropicTool;

    #[test]
    fn coerces_dsml_values_from_the_matching_tool_schema() {
        let payload = MessagesRequest {
            tools: Some(vec![AnthropicTool {
                name: "Edit".to_string(),
                description: "edit a file".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "replace_all": {"type": "boolean"},
                        "timeout": {"type": "integer"},
                        "items": {"type": "array", "items": {"type": "integer"}}
                    }
                }),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let arguments = serde_json::json!({
            "command": {"key": "value"},
            "replace_all": "true",
            "timeout": "1500",
            "items": "[1,\"2\"]"
        });

        let normalized = normalize_dsml_arguments("edit", arguments, &payload);
        assert_eq!(normalized["command"], "{\"key\":\"value\"}");
        assert_eq!(normalized["replace_all"], true);
        assert_eq!(normalized["timeout"], 1500);
        assert_eq!(normalized["items"], serde_json::json!([1, 2]));
    }

    #[test]
    fn leaves_arguments_unchanged_without_a_matching_schema() {
        let payload = MessagesRequest::default();
        let arguments = serde_json::json!({"flag": "true"});
        assert_eq!(
            normalize_dsml_arguments("Unknown", arguments.clone(), &payload),
            arguments
        );
    }
}

#[cfg(test)]
mod compat_parser_invariant_tests {
    use super::*;

    #[test]
    fn strict_valid_json_is_semantically_preserved() {
        let values = [
            serde_json::json!({
                "command": "printf 'a\\|b'",
                "nested": {"x": [1, true, null]}
            }),
            serde_json::json!([{"path": "a"}, {"path": "b"}]),
            serde_json::json!({
                "unicode": "Tiếng Việt 日本語 🦀",
                "quote": "a \" b"
            }),
        ];

        for value in values {
            let marker = format!(
                "[Requesting Tool execution: 'TestTool' with arguments: {}]",
                serde_json::to_string(&value).unwrap()
            );
            let parsed = parse_compat_tool_requests_with_consumed(&marker)
                .expect("strict JSON marker should parse");
            assert_eq!(parsed.calls.len(), 1);
            assert_eq!(parsed.calls[0].arguments, value);
            assert_eq!(parsed.consumed, marker.len());
            assert!(marker.is_char_boundary(parsed.consumed));
        }
    }

    #[test]
    fn deterministic_marker_mutations_never_panic_or_violate_offsets() {
        let seeds = [
            r#"[Requesting Read with arguments: {"file_path":"/tmp/a"}]"#,
            r#"[Requesting TaskUpdate with arguments: {"taskId":"1"},{"taskId":"2"}]"#,
            r#"prefix ```text
[Requesting Read with arguments: {"file_path":"secret"}]
``` suffix"#,
            r#"[Requesting Bash with arguments: {"command":"grep 'a\|b' file"}]"#,
        ];
        let insertions = ['[', ']', '{', '}', '"', '\\', ',', '\n', '🦀'];

        for seed in seeds {
            let boundaries = seed
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(seed.len()))
                .collect::<Vec<_>>();
            let mut cases = vec![seed.to_string()];
            for window in boundaries.windows(2) {
                let start = window[0];
                let end = window[1];
                let mut deleted = seed.to_string();
                deleted.replace_range(start..end, "");
                cases.push(deleted);
                for insertion in insertions {
                    let mut inserted = seed.to_string();
                    inserted.insert(start, insertion);
                    cases.push(inserted);
                }
            }

            for case in cases {
                let outcome = std::panic::catch_unwind(|| {
                    let parsed = parse_compat_tool_requests_with_consumed(&case);
                    let extraction = extract_compat_tool_requests_detailed(&case);
                    (parsed, extraction)
                });
                let (parsed, extraction) = outcome.expect("parser must never panic");
                if let Some(parsed) = parsed {
                    assert!(parsed.consumed <= case.len());
                    assert!(case.is_char_boundary(parsed.consumed));
                    assert!(!parsed.calls.is_empty());
                    assert!(parsed.calls.len() <= MAX_COMPAT_BATCH_ITEMS);
                }
                assert!(extraction.calls.len() <= MAX_COMPAT_CALLS_PER_RESPONSE);
                assert!(
                    extraction.cleaned_text.len()
                        <= case.len().saturating_mul(3).saturating_add(32)
                );
            }
        }
    }

    #[test]
    fn compatibility_parser_enforces_argument_and_batch_limits() {
        let oversized = format!(
            "[Requesting Write with arguments: {{\"content\":\"{}\"}}]",
            "x".repeat(MAX_COMPAT_ARGUMENT_BYTES + 1)
        );
        assert!(parse_compat_tool_requests_with_consumed(&oversized).is_none());

        let batch = (0..=MAX_COMPAT_BATCH_ITEMS)
            .map(|index| format!("{{\"index\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",");
        let marker = format!("[Requesting TaskUpdate with arguments: {batch}]");
        assert!(parse_compat_tool_requests_with_consumed(&marker).is_none());
        let extraction = extract_compat_tool_requests_detailed(&marker);
        assert!(extraction.calls.is_empty());
        assert!(extraction.malformed_intent);
    }
}
