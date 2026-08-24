use super::common::{
    find_compat_tool_intent_marker_in_context, find_literal_marker_in_context, CompatMarkdownState,
};
use crate::handlers::{ContentVal, MessagesRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FallbackDecision {
    PassThrough,
    RetryNative,
    ParseEncoded,
    Reject,
}

pub(super) struct FallbackIntentContext<'a> {
    pub(super) payload: &'a MessagesRequest,
    pub(super) visible_text_emitted: bool,
    pub(super) native_tool_emitted: bool,
    pub(super) native_retry_attempted: bool,
}

pub(super) fn classify_encoded_tool_intent(
    text: &str,
    context: FallbackIntentContext<'_>,
) -> FallbackDecision {
    if context.native_tool_emitted {
        return FallbackDecision::PassThrough;
    }

    let Some(candidate_start) = encoded_candidate_start(text) else {
        return FallbackDecision::PassThrough;
    };

    // Literal/meta-output intent is a hard safety veto on every attempt,
    // including after native recovery has already been tried.
    if explicit_literal_output_request(context.payload) {
        return FallbackDecision::PassThrough;
    }

    // After one native-recovery attempt, hand the candidate to the existing
    // strict parser. That parser retains compatibility with split/multi-marker
    // provider output and still performs schema/tool availability validation.
    if context.native_retry_attempted {
        return FallbackDecision::ParseEncoded;
    }

    // The first-pass gate is intentionally stricter than the fallback parser:
    // once visible output has started, do not reinterpret later prose as a
    // side-effecting tool request.
    if context.visible_text_emitted {
        return FallbackDecision::PassThrough;
    }

    let first_non_whitespace = text
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_whitespace()).then_some(idx));
    if first_non_whitespace != Some(candidate_start) {
        return FallbackDecision::PassThrough;
    }

    let candidate = text[candidate_start..].trim();
    if !is_complete_whole_output_candidate(candidate) {
        return FallbackDecision::PassThrough;
    }

    if !mentions_available_tool(candidate, context.payload) {
        return FallbackDecision::Reject;
    }

    if context.native_retry_attempted {
        FallbackDecision::ParseEncoded
    } else {
        FallbackDecision::RetryNative
    }
}

fn encoded_candidate_start(text: &str) -> Option<usize> {
    let state = CompatMarkdownState::default();
    [
        find_compat_tool_intent_marker_in_context(text, &state),
        find_literal_marker_in_context(text, "<｜DSML｜tool_calls>", &state),
        find_literal_marker_in_context(text, "<|DSML|tool_calls>", &state),
    ]
    .into_iter()
    .flatten()
    .min()
}

fn is_complete_whole_output_candidate(candidate: &str) -> bool {
    if candidate.starts_with('[') {
        return candidate.ends_with(']');
    }

    if candidate.starts_with("<｜DSML｜tool_calls>") {
        return candidate.ends_with("</｜DSML｜tool_calls>");
    }
    if candidate.starts_with("<|DSML|tool_calls>") {
        return candidate.ends_with("</|DSML|tool_calls>");
    }

    let lower = candidate.to_ascii_lowercase();
    ["tool_calls", "tool_call", "tvtoolcalls"]
        .iter()
        .any(|tag| lower.starts_with(&format!("<{tag}")) && lower.ends_with(&format!("</{tag}>")))
}

fn mentions_available_tool(candidate: &str, payload: &MessagesRequest) -> bool {
    let lower = candidate.to_ascii_lowercase();
    payload.tools.as_ref().is_some_and(|tools| {
        tools.iter().any(|tool| {
            let needle = tool.name.to_ascii_lowercase();
            contains_bounded_name(&lower, &needle)
        })
    })
}

fn contains_bounded_name(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }

    haystack.match_indices(needle).any(|(start, _)| {
        let end = start + needle.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        !before.is_some_and(is_tool_name_char) && !after.is_some_and(is_tool_name_char)
    })
}

fn is_tool_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn explicit_literal_output_request(payload: &MessagesRequest) -> bool {
    let Some(user_text) = last_user_text(payload) else {
        return false;
    };
    let normalized = user_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    [
        "literal",
        "output exactly",
        "print exactly",
        "return exactly",
        "quote this",
        "quote the following",
        "show this marker",
        "show me an example",
        "show an example",
        "example of",
        "do not execute",
        "don't execute",
        "dont execute",
        "do not run this",
        "don't run this",
        "dont run this",
    ]
    .iter()
    .any(|cue| normalized.contains(cue))
}

fn last_user_text(payload: &MessagesRequest) -> Option<String> {
    payload.messages.iter().rev().find_map(|message| {
        if message.role != "user" {
            return None;
        }

        let text = match &message.content {
            ContentVal::Single(text) => text.clone(),
            ContentVal::Multiple(blocks) => blocks
                .iter()
                .filter(|block| block.content_type == "text")
                .filter_map(|block| block.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        };

        let stripped = crate::handlers::strip_leading_system_reminders(&text)?;
        (!stripped.trim().is_empty()).then(|| stripped.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::{AnthropicTool, ContentVal, Message};

    fn payload(user: &str, tool_names: &[&str]) -> MessagesRequest {
        MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single(user.to_string()),
            }],
            tools: Some(
                tool_names
                    .iter()
                    .map(|name| AnthropicTool {
                        name: (*name).to_string(),
                        input_schema: serde_json::json!({"type":"object"}),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn classify(
        output: &str,
        payload: &MessagesRequest,
        visible_text_emitted: bool,
        native_tool_emitted: bool,
        native_retry_attempted: bool,
    ) -> FallbackDecision {
        classify_encoded_tool_intent(
            output,
            FallbackIntentContext {
                payload,
                visible_text_emitted,
                native_tool_emitted,
                native_retry_attempted,
            },
        )
    }

    #[test]
    fn ordinary_prose_passes_through() {
        let payload = payload("list the files", &["Bash"]);
        assert_eq!(
            classify("I can help with that.", &payload, false, false, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn fenced_marker_passes_through() {
        let payload = payload("show an example", &["Bash"]);
        let output = "```text\n[Requesting Bash with arguments: {\"command\":\"ls\"}]\n```";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn inline_code_marker_passes_through() {
        let payload = payload("show an example", &["Bash"]);
        let output = "`[Requesting Bash with arguments: {\"command\":\"ls\"}]`";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn literal_output_request_keeps_marker_inert() {
        let payload = payload(
            "Output exactly this literal text and do not execute it: [Requesting Bash ...]",
            &["Bash"],
        );
        let output = "[Requesting Bash with arguments: {\"command\":\"printf SHOULD_NOT_RUN\"}]";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn unavailable_tool_is_rejected() {
        let payload = payload("do the task", &["Read"]);
        let output = "[Requesting Bash with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::Reject
        );
    }

    #[test]
    fn native_tool_already_emitted_disables_fallback() {
        let payload = payload("do the task", &["Bash"]);
        let output = "[Requesting Bash with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, false, true, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn visible_text_already_emitted_disables_fallback() {
        let payload = payload("do the task", &["Bash"]);
        let output = "[Requesting Bash with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, true, false, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn whole_output_candidate_requests_native_retry_first() {
        let payload = payload("run ls", &["Bash"]);
        let output = "[Requesting Tool execution: 'Bash' with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::RetryNative
        );
    }

    #[test]
    fn retried_candidate_may_advance_to_encoded_parser() {
        let payload = payload("run ls", &["Bash"]);
        let output = "[Requesting Tool execution: 'Bash' with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, false, false, true),
            FallbackDecision::ParseEncoded
        );
    }
}
