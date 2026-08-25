//! Non-streaming upstream forwarding.

use super::common::{
    extract_compat_tool_requests_detailed, inject_search_results, invalid_semantic_tool_argument,
    looks_like_unverified_tool_success, matching_tool_name, normalize_dsml_arguments,
    normalize_search_query, prepare_compat_tool_retry, prepare_final_search_synthesis,
    prepare_native_tool_retry, read_bounded_body, resolve_search_query,
    search_results_with_instruction, tool_call_fingerprint,
};
use crate::error::BridgeError;
use crate::handlers::MessagesRequest;
use crate::history::HistoryCapture;
use crate::observability::ToolProtocolMetricClass;
use crate::opencode::forward::fallback_intent::{
    classify_encoded_tool_intent, literal_meta_output_requested, FallbackDecision,
    FallbackIntentContext,
};
use crate::opencode::mapper::{
    is_bridge_search_tool, is_compact_request, map_anthropic_to_openai_with_policy,
};
use crate::opencode::retry::execute_with_warp_retry;
use crate::opencode::sanitize::{
    extract_and_clean_dsml_detailed, strip_system_tags, ParsedDsmlCall,
};
use crate::opencode::search::SearchClient;
use crate::opencode::types::*;
use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};

const MAX_SYNC_COMPAT_TOOL_RETRIES: u32 = 2;
const MAX_SYNC_ENCODED_NATIVE_RETRIES: u32 = 1;

/// How a tool-call batch from one upstream turn must be handled.
///
/// Mirrors the stream executor's split: pure-search batches are collapsed to
/// the first intercepted call, mixed batches emit non-search calls and drop
/// the search calls (the model re-issues them one at a time on later turns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncBatchOutcome {
    Normal,
    Collapse,
    DropSearches,
}

fn classify_sync_tool_batch(
    unavailable_tool: Option<&str>,
    malformed_native_arguments: Option<&str>,
    search_tool_calls: usize,
    total_tool_calls: usize,
) -> Result<SyncBatchOutcome, String> {
    if let Some(name) = unavailable_tool {
        return Err(format!("unavailable tool `{name}`"));
    }
    if let Some(name) = malformed_native_arguments {
        return Err(format!("malformed native arguments for `{name}`"));
    }
    if search_tool_calls > 0 && total_tool_calls > 1 {
        if search_tool_calls < total_tool_calls {
            return Ok(SyncBatchOutcome::DropSearches);
        }
        return Ok(SyncBatchOutcome::Collapse);
    }
    Ok(SyncBatchOutcome::Normal)
}

struct EncodedToolExtraction {
    cleaned: String,
    dsml_calls: Vec<ParsedDsmlCall>,
    compat_calls: Vec<(String, String)>,
}

#[derive(Clone, Copy)]
enum EncodedToolError {
    Dsml,
    Compat,
}

fn extract_encoded_tool_calls(text: &str) -> Result<EncodedToolExtraction, EncodedToolError> {
    let dsml = extract_and_clean_dsml_detailed(text);
    if dsml.malformed_intent {
        return Err(EncodedToolError::Dsml);
    }
    let compat = extract_compat_tool_requests_detailed(&dsml.cleaned_text);
    if compat.malformed_intent {
        return Err(EncodedToolError::Compat);
    }
    Ok(EncodedToolExtraction {
        cleaned: compat.cleaned_text,
        dsml_calls: dsml.calls,
        compat_calls: compat.calls,
    })
}

pub async fn forward_to_llm_sync(
    state: &AppState,
    api_key: String,
    mut payload: MessagesRequest,
    model: String,
    search_client: SearchClient,
    max_search_loops: u32,
    capture: HistoryCapture,
) -> Result<serde_json::Value, BridgeError> {
    let mut upstream_turns = 0_u32;
    let mut completed_searches = 0_u32;
    let mut search_cache = HashMap::<String, String>::new();
    let mut synthesis_only = false;
    let mut compat_tool_retries = 0_u32;
    let mut encoded_native_retries = 0_u32;
    loop {
        upstream_turns = upstream_turns.saturating_add(1);
        if upstream_turns > max_search_loops.saturating_add(3) {
            warn!(
                upstream_turns,
                max_search_loops,
                "Search synthesis turn limit reached; returning a valid terminal response"
            );
            let terminal = search_terminal_response(
                &model,
                "Web research reached the configured turn limit. Use the search results already collected in this conversation; additional search calls were suppressed.",
            );
            capture.append_response(
                "Web research reached the configured turn limit. Use the search results already collected in this conversation; additional search calls were suppressed.",
            );
            capture.finish_success(200, Some("end_turn"), Some(&model));
            return Ok(terminal);
        }

        let openai_req = map_anthropic_to_openai_with_policy(
            &payload,
            model.clone(),
            state.config.protocol.min_reasoning_stream_tokens,
        );
        if let Ok(value) = serde_json::to_value(&openai_req) {
            capture.effective_json(&value, Some(&openai_req.model), "primary", upstream_turns);
        }

        info!("Forwarding sync request for model {}", model);

        let res = match execute_with_warp_retry(state, &api_key, &openai_req).await {
            Ok(response) => response,
            Err(error) => {
                capture.attempt_finished(
                    None,
                    "failed",
                    None,
                    Some("transport_or_provider_error"),
                    Some(&error.to_string()),
                );
                return Err(error);
            }
        };
        let status = res.status();
        let body = match read_bounded_body(res, state.config.protocol.max_sync_response_bytes).await
        {
            Ok(body) => body,
            Err(error) => {
                capture.attempt_finished(
                    Some(status.as_u16()),
                    "failed",
                    None,
                    Some("response_read_error"),
                    Some(&error.to_string()),
                );
                return Err(error);
            }
        };
        capture.first_chunk();
        capture.provider_raw_response(&String::from_utf8_lossy(&body));

        if !status.is_success() {
            error!(
                "Upstream API returned status {}: {} (truncated)",
                status,
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
            capture.attempt_finished(
                Some(status.as_u16()),
                "failed",
                None,
                Some("upstream_non_2xx"),
                Some(&format!("upstream returned status {status}")),
            );
            return Err(BridgeError::UpstreamError(format!(
                "Upstream returned status {}",
                status
            )));
        }

        let openai_resp: OpenAiResponse = match serde_json::from_slice(&body) {
            Ok(response) => response,
            Err(error) => {
                capture.attempt_finished(
                    Some(status.as_u16()),
                    "failed",
                    None,
                    Some("malformed_response"),
                    Some(&error.to_string()),
                );
                return Err(BridgeError::UpstreamError(format!(
                    "Failed to parse response: {error}"
                )));
            }
        };
        let response_model = openai_resp.model.clone();
        capture.response_model(Some(&response_model));

        let choice = match openai_resp.choices.first() {
            Some(choice) => choice,
            None => {
                capture.attempt_finished(
                    Some(status.as_u16()),
                    "failed",
                    None,
                    Some("empty_choices"),
                    Some("no choices returned from upstream"),
                );
                return Err(BridgeError::UpstreamError(
                    "No choices returned from upstream".to_string(),
                ));
            }
        };
        // Native tool_calls have protocol precedence. Encoded channels are still
        // sanitized so marker text cannot leak, but they must never influence
        // validation or execution when native calls are present in this response.
        let native_tool_calls = choice.message.tool_calls.as_deref().unwrap_or(&[]);
        let native_precedence = !native_tool_calls.is_empty();
        let is_compact = is_compact_request(&payload);

        let fallback_decision = if native_precedence || is_compact {
            FallbackDecision::PassThrough
        } else {
            choice
                .message
                .reasoning_content
                .as_deref()
                .into_iter()
                .chain(choice.message.content.as_deref())
                .map(|text| {
                    classify_encoded_tool_intent(
                        text,
                        FallbackIntentContext {
                            payload: &payload,
                            visible_text_emitted: false,
                            native_tool_emitted: false,
                            parser_activated: false,
                        },
                    )
                })
                .fold(FallbackDecision::PassThrough, |current, next| {
                    use FallbackDecision::*;
                    match (current, next) {
                        (Reject, _) | (_, Reject) => Reject,
                        (RetryNative, _) | (_, RetryNative) => RetryNative,
                        (ParseEncoded, _) | (_, ParseEncoded) => ParseEncoded,
                        _ => PassThrough,
                    }
                })
        };

        // Extract text-encoded tool calls from both reasoning and visible text.
        // Free models can place the intent in either channel, and sometimes echo
        // the same marker in both. Raw marker text is never persisted or returned.
        let mut dsml_tool_calls = Vec::new();
        let mut compat_tool_calls = Vec::<(String, String)>::new();
        let mut cleaned_reasoning_content = choice.message.reasoning_content.clone();
        let mut cleaned_message_content = choice.message.content.clone();
        let mut parse_error = None;

        if !is_compact {
            if let Some(reasoning) = choice.message.reasoning_content.as_deref() {
                match extract_encoded_tool_calls(reasoning) {
                    Ok(extraction) => {
                        cleaned_reasoning_content = Some(extraction.cleaned);
                        dsml_tool_calls.extend(extraction.dsml_calls);
                        compat_tool_calls.extend(extraction.compat_calls);
                    }
                    Err(error) => parse_error = Some(error),
                }
            }
            if parse_error.is_none() {
                if let Some(text) = choice.message.content.as_deref() {
                    match extract_encoded_tool_calls(text) {
                        Ok(extraction) => {
                            cleaned_message_content = Some(extraction.cleaned);
                            dsml_tool_calls.extend(extraction.dsml_calls);
                            compat_tool_calls.extend(extraction.compat_calls);
                        }
                        Err(error) => parse_error = Some(error),
                    }
                }
            }
        }

        let encoded_candidate_present =
            !dsml_tool_calls.is_empty() || !compat_tool_calls.is_empty() || parse_error.is_some();
        if encoded_candidate_present {
            state
                .metrics
                .record_tool_protocol(ToolProtocolMetricClass::EncodedCandidate, 1);
            capture.tool_protocol("encoded_candidate", "encoded", 1, None);
        }
        if encoded_candidate_present
            && fallback_decision == FallbackDecision::PassThrough
            && literal_meta_output_requested(&payload)
        {
            state
                .metrics
                .record_tool_protocol(ToolProtocolMetricClass::LiteralMarkerSuppression, 1);
            capture.tool_protocol(
                "literal_marker_suppressed",
                "encoded",
                1,
                Some("explicit literal/meta-output user intent"),
            );
        }

        if native_precedence {
            // Native protocol wins the turn. Complete encoded markers have
            // already been removed from the cleaned text above; discard their
            // parsed calls so they cannot affect availability/batch validation
            // or create a duplicate side effect. If encoded syntax itself was
            // malformed, fail closed on those text channels rather than retrying
            // or leaking the marker beside an otherwise valid native call.
            dsml_tool_calls.clear();
            compat_tool_calls.clear();
            if parse_error.is_some() {
                cleaned_reasoning_content = None;
                cleaned_message_content = None;
            }
            parse_error = None;
        } else {
            match fallback_decision {
                FallbackDecision::RetryNative => {
                    if encoded_native_retries < MAX_SYNC_ENCODED_NATIVE_RETRIES {
                        encoded_native_retries = encoded_native_retries.saturating_add(1);
                        state
                            .metrics
                            .record_tool_protocol(ToolProtocolMetricClass::EncodedNativeRetry, 1);
                        capture.tool_protocol(
                            "native_retry",
                            "encoded_recovery",
                            1,
                            Some("retry encoded candidate through native protocol"),
                        );
                        capture.attempt_finished(
                            Some(status.as_u16()),
                            "retrying",
                            Some("encoded_native_recovery"),
                            Some("encoded_native_recovery"),
                            Some(
                                "encoded tool candidate will be retried using native tool protocol",
                            ),
                        );
                        prepare_native_tool_retry(&mut payload);
                        continue;
                    }
                    return Err(BridgeError::UpstreamError(
                        "Encoded native recovery retry budget exhausted".to_string(),
                    ));
                }
                FallbackDecision::Reject => {
                    state
                        .metrics
                        .record_tool_protocol(ToolProtocolMetricClass::EncodedFallbackRejection, 1);
                    capture.tool_protocol(
                        "encoded_rejection",
                        "encoded",
                        1,
                        Some("encoded marker named an unavailable tool"),
                    );
                    let terminal = "The upstream model emitted an encoded request for a tool that is not safely available in this request. No tool call was executed.";
                    capture.append_response(terminal);
                    capture.attempt_finished(
                        Some(status.as_u16()),
                        "failed",
                        Some("encoded_fallback_rejected"),
                        Some("encoded_fallback_rejected"),
                        Some("encoded marker named an unavailable tool"),
                    );
                    capture.finish_success(200, Some("end_turn"), Some(&response_model));
                    return Ok(search_terminal_response(&model, terminal));
                }
                FallbackDecision::PassThrough => {
                    dsml_tool_calls.clear();
                    compat_tool_calls.clear();
                    if parse_error.is_none() || literal_meta_output_requested(&payload) {
                        cleaned_reasoning_content = choice.message.reasoning_content.clone();
                        cleaned_message_content = choice.message.content.clone();
                        parse_error = None;
                    }
                }
                FallbackDecision::ParseEncoded => {}
            }
        }

        if let Some(error) = parse_error {
            let (retry_code, detail, terminal) = match error {
                EncodedToolError::Dsml => (
                    "malformed_dsml_tool_marker",
                    "sync model emitted an incomplete or ambiguous DSML block",
                    "Upstream repeatedly emitted an incomplete or ambiguous DSML tool block",
                ),
                EncodedToolError::Compat => (
                    "malformed_tool_marker",
                    "sync model emitted an incomplete or ambiguous compatibility marker",
                    "Upstream repeatedly emitted an incomplete or ambiguous tool marker",
                ),
            };
            if compat_tool_retries < MAX_SYNC_COMPAT_TOOL_RETRIES {
                compat_tool_retries = compat_tool_retries.saturating_add(1);
                capture.attempt_finished(
                    Some(status.as_u16()),
                    "retrying",
                    Some(retry_code),
                    Some(retry_code),
                    Some(detail),
                );
                prepare_compat_tool_retry(&mut payload);
                continue;
            }
            return Err(BridgeError::UpstreamError(terminal.to_string()));
        }

        if let Some(reasoning) = cleaned_reasoning_content.as_deref() {
            if !reasoning.is_empty() {
                capture.append_reasoning(reasoning);
            }
        }

        let mut has_search = false;
        let mut search_tc_id = String::new();
        let mut search_tc_name = String::new();
        let mut search_tc_input = serde_json::Value::Null;
        let mut search_args_raw = String::new();

        let unavailable_tool = native_tool_calls
            .iter()
            .map(|call| call.function.name.as_str())
            .chain(dsml_tool_calls.iter().map(|call| call.name.as_str()))
            .chain(compat_tool_calls.iter().map(|(name, _)| name.as_str()))
            .find(|name| matching_tool_name(name, &payload).is_none())
            .map(str::to_string);
        let malformed_native_arguments = native_tool_calls.iter().find_map(|call| {
            serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                .ok()
                .filter(serde_json::Value::is_object)
                .is_none()
                .then(|| call.function.name.clone())
        });
        let invalid_semantic_arguments = native_tool_calls
            .iter()
            .filter_map(|call| {
                let name = matching_tool_name(&call.function.name, &payload)?;
                let arguments =
                    serde_json::from_str::<serde_json::Value>(&call.function.arguments).ok()?;
                let arguments = normalize_dsml_arguments(&name, arguments, &payload);
                invalid_semantic_tool_argument(&name, &arguments)
                    .map(|field| format!("{name}.{field}"))
            })
            .chain(dsml_tool_calls.iter().filter_map(|call| {
                let name = matching_tool_name(&call.name, &payload)?;
                let arguments = normalize_dsml_arguments(&name, call.arguments.clone(), &payload);
                if !arguments.is_object() {
                    return Some(format!("{name}.<object>"));
                }
                invalid_semantic_tool_argument(&name, &arguments)
                    .map(|field| format!("{name}.{field}"))
            }))
            .chain(
                compat_tool_calls
                    .iter()
                    .filter_map(|(raw_name, raw_arguments)| {
                        let name = matching_tool_name(raw_name, &payload)?;
                        let arguments =
                            serde_json::from_str::<serde_json::Value>(raw_arguments).ok()?;
                        let arguments = normalize_dsml_arguments(&name, arguments, &payload);
                        if !arguments.is_object() {
                            return Some(format!("{name}.<object>"));
                        }
                        invalid_semantic_tool_argument(&name, &arguments)
                            .map(|field| format!("{name}.{field}"))
                    }),
            )
            .next();
        let invalid_tool_arguments = malformed_native_arguments.or(invalid_semantic_arguments);
        // Count semantic invocations, not wire encodings. A model may echo the
        // same call in native, reasoning, and visible-marker channels.
        let mut unique_calls = HashSet::new();
        let mut unique_search_calls = HashSet::new();
        for call in native_tool_calls {
            if let Some(name) = matching_tool_name(&call.function.name, &payload) {
                if let Ok(arguments) =
                    serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                {
                    let arguments = normalize_dsml_arguments(&name, arguments, &payload);
                    let fingerprint = tool_call_fingerprint(&name, &arguments);
                    unique_calls.insert(fingerprint.clone());
                    if is_bridge_search_tool(&name) {
                        unique_search_calls.insert(fingerprint);
                    }
                }
            }
        }
        for call in &dsml_tool_calls {
            if let Some(name) = matching_tool_name(&call.name, &payload) {
                let arguments = normalize_dsml_arguments(&name, call.arguments.clone(), &payload);
                let fingerprint = tool_call_fingerprint(&name, &arguments);
                unique_calls.insert(fingerprint.clone());
                if is_bridge_search_tool(&name) {
                    unique_search_calls.insert(fingerprint);
                }
            }
        }
        for (raw_name, raw_arguments) in &compat_tool_calls {
            if let Some(name) = matching_tool_name(raw_name, &payload) {
                if let Ok(arguments) = serde_json::from_str::<serde_json::Value>(raw_arguments) {
                    let arguments = normalize_dsml_arguments(&name, arguments, &payload);
                    let fingerprint = tool_call_fingerprint(&name, &arguments);
                    unique_calls.insert(fingerprint.clone());
                    if is_bridge_search_tool(&name) {
                        unique_search_calls.insert(fingerprint);
                    }
                }
            }
        }
        let total_tool_calls = unique_calls.len();
        let search_tool_calls = unique_search_calls.len();

        let batch_outcome = classify_sync_tool_batch(
            unavailable_tool.as_deref(),
            invalid_tool_arguments.as_deref(),
            search_tool_calls,
            total_tool_calls,
        );
        let mixed_batch = batch_outcome == Ok(SyncBatchOutcome::DropSearches);

        if let Err(issue) = batch_outcome {
            if compat_tool_retries < MAX_SYNC_COMPAT_TOOL_RETRIES {
                compat_tool_retries = compat_tool_retries.saturating_add(1);
                capture.attempt_finished(
                    Some(status.as_u16()),
                    "retrying",
                    Some("invalid_tool_protocol"),
                    Some("invalid_tool_protocol"),
                    Some(&issue),
                );
                prepare_compat_tool_retry(&mut payload);
                continue;
            }
            return Err(BridgeError::UpstreamError(format!(
                "Upstream repeatedly emitted an invalid tool protocol: {issue}"
            )));
        }

        // Check if there is an intercepted search tool call (native first, then DSML)
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let Some(correct_name) = matching_tool_name(&tc.function.name, &payload) else {
                    continue;
                };
                if is_bridge_search_tool(&correct_name) {
                    has_search = true;
                    search_tc_id = tc.id.clone();
                    search_tc_name = correct_name;
                    let input_val: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    search_tc_input = input_val;
                    search_args_raw = tc.function.arguments.clone();
                    break;
                }
            }
        }

        if !has_search {
            for (i, call) in dsml_tool_calls.iter().enumerate() {
                let Some(correct_name) = matching_tool_name(&call.name, &payload) else {
                    continue;
                };
                if is_bridge_search_tool(&correct_name) {
                    has_search = true;
                    search_tc_id = format!(
                        "toolu_dsml_{}_{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis(),
                        i
                    );
                    search_tc_name = correct_name.clone();
                    search_tc_input =
                        normalize_dsml_arguments(&correct_name, call.arguments.clone(), &payload);
                    search_args_raw = serde_json::to_string(&search_tc_input).unwrap_or_default();
                    break;
                }
            }
        }

        if !has_search {
            for (i, (name, arguments)) in compat_tool_calls.iter().enumerate() {
                let Some(correct_name) = matching_tool_name(name, &payload) else {
                    continue;
                };
                if is_bridge_search_tool(&correct_name) {
                    has_search = true;
                    search_tc_id = format!(
                        "toolu_compat_{}_{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis(),
                        i
                    );
                    search_tc_name = correct_name.clone();
                    let parsed = serde_json::from_str::<serde_json::Value>(arguments)
                        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
                    search_tc_input = normalize_dsml_arguments(&correct_name, parsed, &payload);
                    search_args_raw = serde_json::to_string(&search_tc_input).unwrap_or_default();
                    break;
                }
            }
        }

        // Pure-search batches and single searches are intercepted here. A mixed
        // batch falls through to standard formatting so non-search calls are
        // emitted to the client; the search calls are dropped and re-issued by
        // the model on the next turn.
        if has_search && !mixed_batch {
            let (search_query, used_fallback) = resolve_search_query(&search_args_raw, &payload);
            let normalized_query = normalize_search_query(&search_query);
            let cached = search_cache.get(&normalized_query).cloned();
            let duplicate_query = cached.is_some();
            let budget_exhausted = completed_searches >= max_search_loops;
            let final_turn = synthesis_only || budget_exhausted || duplicate_query;

            info!(
                query = %search_query,
                used_fallback,
                completed_searches,
                max_search_loops,
                duplicate = duplicate_query,
                "Intercepted sync search tool call"
            );

            let search_results = if let Some(results) = cached {
                results
            } else if budget_exhausted || synthesis_only {
                "Web search budget reached. No additional network search was executed.".to_string()
            } else {
                let results = search_client.search(&search_query).await;
                completed_searches = completed_searches.saturating_add(1);
                search_cache.insert(normalized_query, results.clone());
                info!("Search completed. Results length: {}", results.len());
                results
            };

            let text_cleaned = cleaned_message_content
                .as_deref()
                .map(strip_system_tags)
                .unwrap_or_default();

            let should_finalize = final_turn || completed_searches >= max_search_loops;
            let injected_results =
                search_results_with_instruction(&search_results, should_finalize);
            inject_search_results(
                &mut payload,
                &injected_results,
                choice.message.reasoning_content.as_deref().unwrap_or(""),
                &text_cleaned,
                &search_tc_id,
                &search_tc_name,
                &search_tc_input,
            );
            capture.search(&search_query, Some(&search_results));
            capture.attempt_finished(
                Some(status.as_u16()),
                "completed",
                Some("tool_calls"),
                None,
                None,
            );
            if should_finalize {
                prepare_final_search_synthesis(
                    &mut payload,
                    if duplicate_query {
                        "duplicate search query"
                    } else {
                        "configured search budget reached"
                    },
                );
                synthesis_only = true;
            }
            continue;
        }

        // Standard response formatting (no search intercepted or final turn)
        let mut content_blocks = Vec::new();

        // 1. Thinking block (reasoning_content), after marker removal.
        if let Some(reasoning) = &cleaned_reasoning_content {
            if !reasoning.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "thinking",
                    "thinking": reasoning
                }));
            }
        }

        // 2. Text block
        if let Some(text) = &cleaned_message_content {
            let cleaned = strip_system_tags(text);
            if !cleaned.is_empty() {
                let visible_text =
                    if total_tool_calls > 0 && looks_like_unverified_tool_success(&cleaned) {
                        warn!("Suppressing unverified sync success claim before tool_result");
                        None
                    } else {
                        Some(cleaned.as_str())
                    };

                if let Some(visible_text) = visible_text.filter(|value| !value.is_empty()) {
                    capture.append_response(visible_text);
                    content_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": visible_text
                    }));
                }
            }
        }

        // 3. Native Tool calls
        let mut has_tool_calls = false;
        let mut native_emitted_count = 0_u64;
        let mut encoded_emitted_count = 0_u64;
        let mut emitted_tool_fingerprints = HashSet::new();
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let Some(correct_name) = matching_tool_name(&tc.function.name, &payload) else {
                    warn!(tool = %tc.function.name, "Ignoring native call for unavailable tool");
                    continue;
                };
                if mixed_batch && is_bridge_search_tool(&correct_name) {
                    warn!(tool = %correct_name, "Dropping search call from mixed sync batch; emitting non-search calls");
                    continue;
                }
                let parsed = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
                let input_val = normalize_dsml_arguments(&correct_name, parsed, &payload);
                let fingerprint = tool_call_fingerprint(&correct_name, &input_val);
                if !emitted_tool_fingerprints.insert(fingerprint) {
                    warn!(tool = %correct_name, "Suppressing duplicate native tool invocation");
                    continue;
                }
                has_tool_calls = true;
                native_emitted_count = native_emitted_count.saturating_add(1);
                let input_json = serde_json::to_string(&input_val).unwrap_or_default();
                capture.tool_call(&correct_name, Some(&input_json));
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": correct_name,
                    "input": input_val
                }));
            }
        }

        // 4. DSML Tool calls
        for (i, call) in dsml_tool_calls.into_iter().enumerate() {
            let Some(cased_name) = matching_tool_name(&call.name, &payload) else {
                warn!(tool = %call.name, "Ignoring DSML call for unavailable tool");
                continue;
            };
            if mixed_batch && is_bridge_search_tool(&cased_name) {
                warn!(tool = %cased_name, "Dropping search call from mixed sync batch; emitting non-search calls");
                continue;
            }
            let input = normalize_dsml_arguments(&cased_name, call.arguments, &payload);
            let fingerprint = tool_call_fingerprint(&cased_name, &input);
            if !emitted_tool_fingerprints.insert(fingerprint) {
                warn!(tool = %cased_name, "Suppressing duplicate DSML tool invocation");
                continue;
            }
            has_tool_calls = true;
            encoded_emitted_count = encoded_emitted_count.saturating_add(1);
            let tool_id = format!(
                "toolu_dsml_{}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                i
            );
            let input_json = serde_json::to_string(&input).unwrap_or_default();
            capture.tool_call(&cased_name, Some(&input_json));
            content_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tool_id,
                "name": cased_name,
                "input": input
            }));
        }

        // 5. Free-model compatibility marker tool calls.
        for (i, (name, arguments)) in compat_tool_calls.into_iter().enumerate() {
            let Some(cased_name) = matching_tool_name(&name, &payload) else {
                warn!(tool = %name, "Ignoring compatibility marker for unavailable tool");
                continue;
            };
            if mixed_batch && is_bridge_search_tool(&cased_name) {
                warn!(tool = %cased_name, "Dropping search call from mixed sync batch; emitting non-search calls");
                continue;
            }
            let parsed = serde_json::from_str::<serde_json::Value>(&arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            let input = normalize_dsml_arguments(&cased_name, parsed, &payload);
            let fingerprint = tool_call_fingerprint(&cased_name, &input);
            if !emitted_tool_fingerprints.insert(fingerprint) {
                warn!(tool = %cased_name, "Suppressing duplicate compatibility tool invocation");
                continue;
            }
            has_tool_calls = true;
            encoded_emitted_count = encoded_emitted_count.saturating_add(1);
            let tool_id = format!(
                "toolu_compat_{}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                i
            );
            let input_json = serde_json::to_string(&input).unwrap_or_default();
            capture.tool_call(&cased_name, Some(&input_json));
            content_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tool_id,
                "name": cased_name,
                "input": input
            }));
        }

        if native_emitted_count > 0 {
            state.metrics.record_tool_protocol(
                ToolProtocolMetricClass::NativeToolCall,
                native_emitted_count,
            );
            capture.tool_protocol("tool_calls", "native", native_emitted_count, None);
        }
        if encoded_emitted_count > 0 {
            state.metrics.record_tool_protocol(
                ToolProtocolMetricClass::EncodedFallbackToolCall,
                encoded_emitted_count,
            );
            capture.tool_protocol(
                "tool_calls",
                "encoded_fallback",
                encoded_emitted_count,
                None,
            );
        }

        // Ensure we always have at least one text or tool_use block in the content list
        // to prevent Anthropic's client-side validation from crashing.
        let has_visible_content = content_blocks.iter().any(|block| {
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            block_type == "text" || block_type == "tool_use"
        });
        if !has_visible_content {
            content_blocks.push(serde_json::json!({
                "type": "text",
                "text": "[Empty upstream response]"
            }));
        }

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("stop") => {
                if has_tool_calls {
                    "tool_use"
                } else {
                    "end_turn"
                }
            }
            Some("tool_calls") => "tool_use",
            Some("length") => "max_tokens",
            _ => {
                if has_tool_calls {
                    "tool_use"
                } else {
                    "end_turn"
                }
            }
        };

        let usage = openai_resp.usage.unwrap_or(OpenAiUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        let anthropic_resp = serde_json::json!({
            "id": format!("msg_opencode_{}", openai_resp.id),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content_blocks,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": usage.prompt_tokens,
                "output_tokens": usage.completion_tokens
            }
        });

        let reasoning_tokens = choice
            .message
            .reasoning_content
            .as_deref()
            .map(|value| (value.chars().count() as u64).div_ceil(4));
        capture.usage(
            Some(u64::from(usage.prompt_tokens)),
            Some(u64::from(usage.completion_tokens)),
            reasoning_tokens,
        );
        capture.attempt_finished(
            Some(status.as_u16()),
            "completed",
            choice.finish_reason.as_deref(),
            None,
            None,
        );
        capture.finish_success(200, Some(stop_reason), Some(&response_model));
        return Ok(anthropic_resp);
    }
    fn search_terminal_response(model: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": format!("msg_opencode_search_guard_{}", std::process::id()),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type":"text","text":text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens":0,"output_tokens":1}
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_batch_with_search_and_non_search_is_split_not_rejected() {
        let outcome = classify_sync_tool_batch(None, None, 1, 2);
        assert_eq!(outcome, Ok(SyncBatchOutcome::DropSearches));
    }

    #[test]
    fn pure_search_batch_is_collapsed_not_rejected() {
        let outcome = classify_sync_tool_batch(None, None, 2, 2);
        assert_eq!(outcome, Ok(SyncBatchOutcome::Collapse));
    }

    #[test]
    fn single_search_call_is_normal() {
        let outcome = classify_sync_tool_batch(None, None, 1, 1);
        assert_eq!(outcome, Ok(SyncBatchOutcome::Normal));
    }

    #[test]
    fn unavailable_tool_is_still_rejected() {
        let outcome = classify_sync_tool_batch(Some("Read"), None, 1, 2);
        assert_eq!(outcome, Err("unavailable tool `Read`".to_string()));
    }

    #[test]
    fn malformed_native_arguments_are_still_rejected() {
        let outcome = classify_sync_tool_batch(None, Some("Bash"), 1, 2);
        assert_eq!(
            outcome,
            Err("malformed native arguments for `Bash`".to_string())
        );
    }
}
