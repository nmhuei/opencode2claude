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

#[derive(Clone, Copy)]
pub(super) struct FallbackIntentContext<'a> {
    pub(super) payload: &'a MessagesRequest,
    pub(super) visible_text_emitted: bool,
    pub(super) native_tool_emitted: bool,
    pub(super) parser_activated: bool,
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

    // Once the lightweight gate has activated the compatibility parser for
    // this response, preserve the old parser's multi-marker/split semantics.
    // Literal/meta intent remains a hard veto above, and native calls remain
    // authoritative because streaming defers encoded execution until EOF.
    if context.parser_activated {
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
    if first_non_whitespace != Some(candidate_start)
        && !safe_tool_intent_preamble(&text[..candidate_start])
    {
        return FallbackDecision::PassThrough;
    }

    let candidate = text[candidate_start..].trim();
    if !is_complete_whole_output_candidate(candidate) {
        return FallbackDecision::PassThrough;
    }

    if !mentions_available_tool(candidate, context.payload) {
        return FallbackDecision::Reject;
    }

    FallbackDecision::ParseEncoded
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

pub(super) fn safe_tool_intent_preamble(prefix: &str) -> bool {
    let normalized = prefix
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase();
    if normalized.is_empty() || normalized.len() > 320 {
        return false;
    }
    if [
        "show",
        "example",
        "quote",
        "literal",
        "demonstrat",
        "do not execute",
        "don't execute",
        "dont execute",
        "syntax only",
    ]
    .iter()
    .any(|cue| normalized.contains(cue))
    {
        return false;
    }

    let first_person_execution = [
        "i'll ",
        "i will ",
        "let me ",
        "i’m going to ",
        "i'm going to ",
    ]
    .iter()
    .any(|cue| normalized.starts_with(cue));
    let execution_verb = [" use ", " invoke ", " emit ", " call ", " request "]
        .iter()
        .any(|cue| format!(" {normalized} ").contains(cue));

    first_person_execution && execution_verb
}

fn is_complete_whole_output_candidate(candidate: &str) -> bool {
    let mut candidate = candidate.trim();
    loop {
        let lower = candidate.to_ascii_lowercase();
        let stripped = ["</think>", "</thinking>"].iter().find_map(|suffix| {
            lower
                .ends_with(suffix)
                .then(|| candidate[..candidate.len() - suffix.len()].trim_end())
        });
        let Some(next) = stripped else {
            break;
        };
        candidate = next;
    }

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

pub(super) fn literal_meta_output_requested(payload: &MessagesRequest) -> bool {
    explicit_literal_output_request(payload)
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
        parser_activated: bool,
    ) -> FallbackDecision {
        classify_encoded_tool_intent(
            output,
            FallbackIntentContext {
                payload,
                visible_text_emitted,
                native_tool_emitted,
                parser_activated,
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
    fn literal_and_example_cues_keep_markers_inert() {
        let output = "[Requesting Bash with arguments: {\"command\":\"printf SHOULD_NOT_RUN\"}]";
        for user in [
            "show me an example of a Requesting Bash marker",
            "quote this string",
            "print exactly this marker",
            "return exactly this marker",
            "do not execute this marker",
        ] {
            let payload = payload(user, &["Bash"]);
            assert_eq!(
                classify(output, &payload, false, false, false),
                FallbackDecision::PassThrough,
                "user={user}"
            );
            assert_eq!(
                classify(output, &payload, false, false, true),
                FallbackDecision::PassThrough,
                "native-retried user={user}"
            );
        }
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
    fn first_person_tool_intent_preamble_activates_lazy_parser() {
        let payload = payload("Read ./compat_read.txt and return its token.", &["Read"]);
        let output = "I'll emit the Read request in bridge compatibility-marker syntax so the proxy recovers it as a tool call.\n\n[Requesting Read with arguments: {\"file_path\":\"./compat_read.txt\"}]";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::ParseEncoded
        );
    }

    #[test]
    fn explanatory_marker_preamble_remains_inert() {
        let payload = payload("Explain the compatibility protocol.", &["Read"]);
        let output = "Here is an example marker for explanation: [Requesting Read with arguments: {\"file_path\":\"./compat_read.txt\"}]";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::PassThrough
        );
    }

    #[test]
    fn whole_output_candidate_activates_lazy_parser() {
        let payload = payload("run ls", &["Bash"]);
        let output = "[Requesting Tool execution: 'Bash' with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, false, false, false),
            FallbackDecision::ParseEncoded
        );
    }

    #[test]
    fn lazy_candidate_decision_is_stable_across_legacy_retry_state() {
        let payload = payload("run ls", &["Bash"]);
        let output = "[Requesting Tool execution: 'Bash' with arguments: {\"command\":\"ls\"}]";
        assert_eq!(
            classify(output, &payload, false, false, true),
            FallbackDecision::ParseEncoded
        );
    }
}
