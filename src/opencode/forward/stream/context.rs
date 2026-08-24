//! Per-response stream state and OpenAI-to-Anthropic delta translation.

use super::transport::send_sse;
use crate::handlers::MessagesRequest;
use crate::opencode::forward::common::{
    compat_tool_marker_pending_suffix_len, find_compat_tool_intent_marker_in_context,
    find_literal_marker_in_context, invalid_semantic_tool_argument,
    looks_like_unverified_tool_success, matching_tool_name, normalize_dsml_arguments,
    parse_compat_tool_requests_at_eof, parse_compat_tool_requests_with_consumed,
    tool_call_fingerprint, CompatMarkdownState, CompatToolCall,
};
use crate::opencode::forward::fallback_intent::{
    classify_encoded_tool_intent, literal_meta_output_requested, safe_tool_intent_preamble,
    FallbackDecision, FallbackIntentContext,
};
use crate::opencode::mapper::is_bridge_search_tool;
use crate::opencode::sanitize::{parse_dsml_tool_calls_detailed, strip_system_tags_with_context};
use crate::opencode::types::*;
use crate::sse::SseEventBuilder;
use crate::stream_tracker::SseBlockTracker;
use axum::response::sse::Event;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;
use tracing::{trace, warn};

const MAX_DSML_BUFFER_SIZE: usize = 256 * 1024;
const MAX_COMPAT_TOOL_BUFFER_SIZE: usize = 64 * 1024;
// Claude Code's interactive renderer keeps an open thinking block collapsed
// until content_block_stop. Segment long reasoning into bounded blocks so the
// user sees completed reasoning portions while the model is still working.
const THINKING_RENDER_CHUNK_BYTES: usize = 16384;
// Bound each outgoing Anthropic delta even when the provider emits one large
// OpenAI SSE event. Pathological multi-kilobyte provider deltas are also paced
// very briefly between chunks so Claude Code can visibly render progress
// instead of repainting the whole report in one scheduler burst.
const RENDER_DELTA_CHUNK_BYTES: usize = 256;
const LARGE_DELTA_PACING_DELAY: Duration = Duration::from_millis(2);
const DSML_OPEN_TAG: &str = "<｜DSML｜tool_calls>";
const DSML_CLOSE_TAG: &str = "</｜DSML｜tool_calls>";

fn split_utf8_prefix(text: &str, max_bytes: usize) -> (&str, &str) {
    if text.len() <= max_bytes {
        return (text, "");
    }

    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.split_at(end)
}

/// Merge an identifier carried across OpenAI streaming deltas.
///
/// Providers normally send disjoint fragments (`"Ba"`, then `"sh"`), while
/// some OpenAI-compatible gateways resend a cumulative snapshot (`"Ba"`, then
/// `"Bash"`). Supporting both avoids either dropping the prefix or producing
/// `"BaBash"`. Exact repeats are ignored; every other fragment is appended.
fn merge_streamed_identifier(slot: &mut Option<String>, fragment: &str) {
    if fragment.is_empty() {
        return;
    }

    match slot {
        None => *slot = Some(fragment.to_string()),
        Some(current) if current == fragment => {}
        Some(current) if fragment.starts_with(current.as_str()) => {
            current.clear();
            current.push_str(fragment);
        }
        Some(current) => current.push_str(fragment),
    }
}

/// Merge streamed JSON arguments while tolerating gateways that resend the
/// complete argument snapshot on every chunk instead of true deltas.
fn merge_streamed_arguments(current: &mut String, fragment: &str) {
    if fragment.is_empty() || current == fragment {
        return;
    }
    if fragment.starts_with(current.as_str()) {
        current.clear();
        current.push_str(fragment);
    } else {
        current.push_str(fragment);
    }
}

#[derive(Debug, Clone, Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Finalize the stream.
///
/// Used on error paths (stream_failed, search loop protection, empty upstream)
/// when `message_start` has already been emitted but no content blocks or
/// `message_stop` have been sent. A fatal/errored stream is closed with a
/// single Anthropic `error` event that ENDS the stream — no `message_delta`
/// and no `message_stop` follow it, per the Messages API spec. Only a healthy
/// end of message emits `message_delta` + `message_stop`.
pub(super) async fn finalize_stream(
    reason: &str,
    tx: &tokio::sync::mpsc::Sender<Event>,
    builder: &SseEventBuilder,
    tracker: &mut SseBlockTracker,
    ctx: &mut StreamContext,
) {
    if !ctx.message_started {
        // message_start was never emitted — emit a minimal fallback one
        let _ = send_sse(tx, builder.message_start(0)).await;
        ctx.message_started = true;
    }

    // Flush any remaining buffers
    let had_pending_tool_calls = !ctx.pending_tool_calls.is_empty();
    ctx.flush_remaining(tracker, tx, builder, &Default::default())
        .await;

    // Close any remaining active blocks
    for (_, idx) in tracker.close_all() {
        let _ = send_sse(tx, crate::sse::emit_block_stop(idx)).await;
    }

    let stranded = !tracker.has_any_blocks_ever_opened()
        || (had_pending_tool_calls && !ctx.has_emitted_tool_use)
        || ctx.stream_failed;
    if stranded {
        // No usable assistant content survived (nothing was opened, the tool
        // call never completed, or the stream broke mid-flight). Report the
        // failure honestly as a terminal error event instead of a clean
        // end_turn that would hide a truncated/corrupted message.
        let error_msg = if ctx.stream_failed
            && !tracker.has_any_blocks_ever_opened()
            && !had_pending_tool_calls
        {
            format!(
                "Upstream response did not contain content blocks (reason: {})",
                reason
            )
        } else if had_pending_tool_calls && !ctx.has_emitted_tool_use {
            format!(
                "Upstream stream ended before a pending tool call completed (reason: {})",
                reason
            )
        } else {
            format!("Upstream stream ended with a read error (reason: {reason})")
        };
        let _ = send_sse(tx, builder.api_error(&error_msg)).await;
        return;
    }

    let stop_reason = if ctx.has_emitted_tool_use {
        "tool_use".to_string()
    } else {
        "end_turn".to_string()
    };
    let _ = send_sse(tx, builder.message_delta_with_stop(&stop_reason, 1)).await;
    let _ = send_sse(tx, builder.message_stop()).await;
}

pub(super) async fn finalize_stream_with_text(
    text: &str,
    tx: &tokio::sync::mpsc::Sender<Event>,
    builder: &SseEventBuilder,
    tracker: &mut SseBlockTracker,
    message_started: bool,
) {
    if !message_started {
        let _ = send_sse(tx, builder.message_start(0)).await;
    }
    for (_, idx) in tracker.close_all() {
        let _ = send_sse(tx, crate::sse::emit_block_stop(idx)).await;
    }
    let (text_idx, _, _) = tracker.ensure_text();
    let _ = send_sse(
        tx,
        builder.content_block_start_at(text_idx, "text", None, None),
    )
    .await;
    let _ = send_sse(tx, builder.text_delta_at(text_idx, text)).await;
    let _ = send_sse(tx, crate::sse::emit_block_stop(text_idx)).await;
    let _ = send_sse(tx, builder.message_delta_with_stop("end_turn", 1)).await;
    let _ = send_sse(tx, builder.message_stop()).await;
}

pub(super) async fn process_openai_sse_line(
    line: &str,
    ctx: &mut StreamContext,
    tracker: &mut SseBlockTracker,
    tx: &tokio::sync::mpsc::Sender<Event>,
    builder: &SseEventBuilder,
    payload: &MessagesRequest,
) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }

    let Some(stripped) = line.strip_prefix("data:") else {
        return false;
    };
    let data_str = stripped.trim();
    if data_str == "[DONE]" {
        return true;
    }

    let chunk = match serde_json::from_str::<OpenAiStreamChunk>(data_str) {
        Ok(chunk) => chunk,
        Err(e) => {
            warn!(
                "Ignoring malformed upstream SSE data line: {} ({})",
                data_str.chars().take(120).collect::<String>(),
                e
            );
            return false;
        }
    };

    // A mid-stream upstream error payload (`{"error": {...}}`) is a definitive
    // terminal signal: surface it as an Anthropic error event that ends the
    // stream. Silently dropping it let truncated messages exit as clean
    // end_turn, hiding the failure from the client.
    if let Some(upstream_error) = &chunk.error {
        let message = upstream_error
            .message
            .as_deref()
            .unwrap_or("Upstream streaming error")
            .chars()
            .take(200)
            .collect::<String>();
        let _ = send_sse(tx, builder.api_error(&message)).await;
        ctx.error_terminated = true;
        ctx.stream_failed = true;
        return true;
    }

    if let Some(choice) = chunk.choices.first() {
        trace!(
            content_bytes = choice.delta.content.as_deref().map(str::len).unwrap_or(0),
            reasoning_bytes = choice
                .delta
                .reasoning_content
                .as_deref()
                .map(str::len)
                .unwrap_or(0),
            tool_call_fragments = choice.delta.tool_calls.as_ref().map(Vec::len).unwrap_or(0),
            finish_reason = ?choice.finish_reason,
            "stream_timing upstream_delta_parsed"
        );
        if let Some(reason) = &choice.finish_reason {
            ctx.update_stop_reason(reason);
        }

        if let Some(reasoning) = &choice.delta.reasoning_content {
            ctx.process_reasoning_delta(reasoning, tracker, tx, builder, payload)
                .await;
        }

        if let Some(content) = &choice.delta.content {
            ctx.process_content_delta(content, tracker, tx, builder, payload)
                .await;
        }

        if let Some(tool_calls) = &choice.delta.tool_calls {
            ctx.process_tool_calls(tool_calls);
        }
        if choice.finish_reason.is_some() {
            ctx.finalize_pending_native_tool_calls(tracker, tx, builder, payload)
                .await;
        }
    }

    false
}

/// Bundles all mutable streaming state for `forward_to_llm_stream`.
///
/// Encapsulates the 11+ mutable variables that track search interception,
/// DSML parsing, text accumulation, and stream health into a single unit.
/// Methods on this struct handle the core processing logic for each
/// streaming event type (reasoning deltas, text deltas with DSML detection,
/// tool call deltas, and final buffer flushing).
pub(super) struct StreamContext {
    /// Whether `message_start` has been emitted.
    pub(super) message_started: bool,
    /// Whether we are currently intercepting a web search tool call.
    pub(super) intercepting_search: bool,
    /// Tool call ID for the intercepted search, if any.
    pub(super) search_tc_id: String,
    /// Tool call name for the intercepted search, if any.
    pub(super) search_tc_name: String,
    /// Accumulated JSON arguments for the intercepted search tool call.
    pub(super) search_tc_args: String,
    /// OpenAI tool-call index selected for search interception.
    search_tc_index: Option<usize>,
    /// Per-index fragments, including arguments received before the tool name.
    pending_tool_calls: BTreeMap<usize, PendingToolCall>,
    /// Accumulated thinking text across all chunks in this response turn.
    pub(super) accumulated_thinking: String,
    /// Rolling reasoning buffer used to detect compatibility tool markers that
    /// free models occasionally place inside the reasoning channel.
    reasoning_stream_buffer: String,
    /// Markdown/code context carried across reasoning chunks.
    reasoning_markdown_state: CompatMarkdownState,
    /// Whether an oversized malformed compatibility marker in reasoning has
    /// entered fail-closed discard mode for the rest of this response turn.
    discarding_reasoning_compat: bool,
    /// Bytes emitted in the currently open thinking block. Long blocks are
    /// segmented so Claude Code's TUI can render progress before reasoning ends.
    thinking_block_bytes: usize,
    /// Accumulated visible text across all chunks in this response turn.
    pub(super) accumulated_text: String,
    /// Whether the stream encountered a fatal read error.
    pub(super) stream_failed: bool,
    /// Whether an outgoing Anthropic `error` event was already emitted. The
    /// stream must end at that event: no `message_delta`/`message_stop` and
    /// no further content may follow it.
    pub(super) error_terminated: bool,
    /// Whether any `tool_use` content block has been emitted.
    pub(super) has_emitted_tool_use: bool,
    /// Whether a tool_use came from the native OpenAI tool_calls protocol.
    /// Encoded fallback tool_use blocks intentionally do not set this flag.
    has_emitted_native_tool_use: bool,
    /// Per-attempt protocol counters surfaced to the outer executor for metrics
    /// and history without retaining tool arguments.
    pub(super) native_tool_calls_emitted: u32,
    pub(super) encoded_tool_calls_emitted: u32,
    pub(super) literal_marker_suppressed: bool,
    /// Semantic tool calls already emitted during this assistant turn. This
    /// prevents a marker echoed in both thinking and visible text, or repeated
    /// verbatim by the model, from executing twice.
    emitted_tool_fingerprints: HashSet<String>,
    /// Whether we are currently inside a <｜DSML｜tool_calls> block.
    pub(super) dsml_mode: bool,
    /// Buffer for DSML content being accumulated inside a <｜DSML｜tool_calls> block.
    pub(super) dsml_stream_buffer: String,
    /// Buffer for text content before DSML tag detection or after DSML parsing.
    pub(super) text_stream_buffer: String,
    /// Markdown/code context carried across visible-text chunks.
    text_markdown_state: CompatMarkdownState,
    /// Whether an oversized malformed compatibility marker in visible text has
    /// entered fail-closed discard mode for the rest of this response turn.
    discarding_text_compat: bool,
    /// A prose-only or structurally incomplete tool marker was retained through
    /// EOF. The outer execution loop can retry upstream with a correction.
    pub(super) compat_retry_requested: bool,
    /// A complete encoded tool candidate was observed in text/reasoning.
    pub(super) encoded_candidate_seen: bool,
    /// The first encoded candidate requested one upstream retry that must use
    /// the native tool-calling protocol before encoded fallback is considered.
    pub(super) native_recovery_retry_requested: bool,
    /// A candidate was rejected by the lightweight gate (for example because
    /// it named a tool that Claude Code did not provide). Rejects fail closed.
    pub(super) encoded_fallback_rejected: bool,
    /// Whether this upstream attempt is allowed to execute a strict encoded
    /// fallback after a previous native-recovery retry failed.
    encoded_fallback_permitted: bool,
    /// Recovery attempts retain encoded candidates until native tool-call
    /// fragments have had the full response to arrive. Native calls are
    /// finalized first at EOF; only then may the encoded parser run.
    defer_encoded_fallback_until_native_finalized: bool,
    /// Determined from `finish_reason` in the last stream chunk.
    pub(super) final_stop_reason: String,
}

impl StreamContext {
    #[cfg(test)]
    pub(super) fn new(is_compact: bool) -> Self {
        // Parser-focused unit tests use this constructor to exercise the strict
        // encoded compatibility parser directly. Production streaming chooses
        // permission explicitly via `new_with_encoded_fallback` and defers
        // encoded execution until native fragments have had the full attempt.
        let mut context = Self::new_with_encoded_fallback(is_compact, true);
        context.defer_encoded_fallback_until_native_finalized = false;
        context
    }

    pub(super) fn new_with_encoded_fallback(
        _is_compact: bool,
        encoded_fallback_permitted: bool,
    ) -> Self {
        Self {
            message_started: false,
            intercepting_search: false,
            search_tc_id: String::new(),
            search_tc_name: String::new(),
            search_tc_args: String::new(),
            search_tc_index: None,
            pending_tool_calls: BTreeMap::new(),
            accumulated_thinking: String::new(),
            reasoning_stream_buffer: String::new(),
            reasoning_markdown_state: CompatMarkdownState::default(),
            discarding_reasoning_compat: false,
            thinking_block_bytes: 0,
            accumulated_text: String::new(),
            stream_failed: false,
            error_terminated: false,
            has_emitted_tool_use: false,
            has_emitted_native_tool_use: false,
            native_tool_calls_emitted: 0,
            encoded_tool_calls_emitted: 0,
            literal_marker_suppressed: false,
            emitted_tool_fingerprints: HashSet::new(),
            dsml_mode: false,
            dsml_stream_buffer: String::new(),
            text_stream_buffer: String::new(),
            text_markdown_state: CompatMarkdownState::default(),
            discarding_text_compat: false,
            compat_retry_requested: false,
            encoded_candidate_seen: false,
            native_recovery_retry_requested: false,
            encoded_fallback_rejected: false,
            encoded_fallback_permitted,
            defer_encoded_fallback_until_native_finalized: encoded_fallback_permitted,
            final_stop_reason: "end_turn".to_string(),
        }
    }

    fn classify_encoded_candidate(
        &mut self,
        text: &str,
        payload: &MessagesRequest,
    ) -> FallbackDecision {
        self.encoded_candidate_seen = true;
        let decision = classify_encoded_tool_intent(
            text,
            FallbackIntentContext {
                payload,
                visible_text_emitted: !self.accumulated_text.is_empty()
                    || !self.accumulated_thinking.is_empty(),
                native_tool_emitted: self.has_emitted_native_tool_use,
                native_retry_attempted: self.encoded_fallback_permitted,
            },
        );
        match decision {
            FallbackDecision::RetryNative => self.native_recovery_retry_requested = true,
            FallbackDecision::Reject => self.encoded_fallback_rejected = true,
            FallbackDecision::PassThrough if literal_meta_output_requested(payload) => {
                self.literal_marker_suppressed = true;
            }
            FallbackDecision::PassThrough | FallbackDecision::ParseEncoded => {}
        }
        decision
    }

    /// Update the final stop reason from a stream chunk's `finish_reason`.
    fn update_stop_reason(&mut self, reason: &str) {
        self.final_stop_reason = match reason {
            "stop" => "end_turn".to_string(),
            // An upstream model may hallucinate a tool that Claude Code did not
            // provide. In that case no tool_use block is emitted, so returning
            // tool_use would make Claude Code report malformed_tool_use.
            "tool_calls" if self.has_emitted_tool_use || self.intercepting_search => {
                "tool_use".to_string()
            }
            "tool_calls" => "end_turn".to_string(),
            "length" => "max_tokens".to_string(),
            _ => "end_turn".to_string(),
        };
    }

    async fn emit_thinking_fragment(
        &mut self,
        text: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
    ) {
        let cleaned = strip_system_tags_with_context(text, &self.reasoning_markdown_state);
        if cleaned.is_empty() {
            return;
        }
        self.reasoning_markdown_state.advance(&cleaned);
        if self.intercepting_search {
            return;
        }
        self.accumulated_thinking.push_str(&cleaned);

        let mut remaining = cleaned.as_str();
        while !remaining.is_empty() {
            let (fragment, rest) = split_utf8_prefix(remaining, RENDER_DELTA_CHUNK_BYTES);
            remaining = rest;

            let (thinking_idx, thinking_is_new, closed_text) = tracker.ensure_thinking();
            if let Some(closed) = closed_text {
                let _ = tx.send(crate::sse::emit_block_stop(closed)).await;
            }
            if thinking_is_new {
                let _ = tx
                    .send(builder.content_block_start_at(thinking_idx, "thinking", None, None))
                    .await;
            }
            let _ = tx
                .send(builder.thinking_delta(thinking_idx, fragment))
                .await;
            self.thinking_block_bytes = self.thinking_block_bytes.saturating_add(fragment.len());
            trace!(
                block_index = thinking_idx,
                bytes = fragment.len(),
                "stream_timing anthropic_thinking_delta_enqueued"
            );
            if self.thinking_block_bytes >= THINKING_RENDER_CHUNK_BYTES {
                if let Some(closed) = tracker.close_thinking() {
                    let _ = tx.send(crate::sse::emit_block_stop(closed)).await;
                    trace!(
                        block_index = closed,
                        bytes = self.thinking_block_bytes,
                        "stream_timing thinking_block_segment_closed"
                    );
                }
                self.thinking_block_bytes = 0;
            }
            if !remaining.is_empty() {
                tokio::time::sleep(LARGE_DELTA_PACING_DELAY).await;
            }
        }
    }

    /// Process a reasoning/thinking delta from the upstream stream chunk.
    ///
    /// Free models sometimes close their thinking XML and place a compatibility
    /// tool marker in the same reasoning channel. Retain only a possible marker
    /// suffix, stream safe reasoning immediately, and convert complete markers
    /// into normal Claude Code tool_use blocks.
    async fn process_reasoning_delta(
        &mut self,
        reasoning: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        if self.intercepting_search
            || self.discarding_reasoning_compat
            || self.compat_retry_requested
            || self.native_recovery_retry_requested
            || self.encoded_fallback_rejected
        {
            return;
        }
        self.reasoning_stream_buffer.push_str(reasoning);

        loop {
            if self.intercepting_search
                || self.compat_retry_requested
                || self.native_recovery_retry_requested
                || self.encoded_fallback_rejected
                || self.reasoning_stream_buffer.is_empty()
            {
                return;
            }

            if let Some(marker_pos) = find_compat_tool_intent_marker_in_context(
                &self.reasoning_stream_buffer,
                &self.reasoning_markdown_state,
            ) {
                if marker_pos > 0 {
                    let safe_prefix = self.reasoning_stream_buffer[..marker_pos].to_string();
                    self.reasoning_stream_buffer.drain(..marker_pos);
                    self.emit_thinking_fragment(&safe_prefix, tracker, tx, builder)
                        .await;
                    continue;
                }

                if let Some(parsed) =
                    parse_compat_tool_requests_with_consumed(&self.reasoning_stream_buffer)
                {
                    if self.defer_encoded_fallback_until_native_finalized {
                        return;
                    }
                    let raw_candidate = self.reasoning_stream_buffer.clone();
                    match self.classify_encoded_candidate(&raw_candidate, payload) {
                        FallbackDecision::RetryNative | FallbackDecision::Reject => {
                            self.reasoning_stream_buffer.clear();
                            return;
                        }
                        FallbackDecision::PassThrough => {
                            self.reasoning_stream_buffer.clear();
                            self.emit_thinking_fragment(&raw_candidate, tracker, tx, builder)
                                .await;
                            return;
                        }
                        FallbackDecision::ParseEncoded => {}
                    }
                    let remaining = strip_system_tags_with_context(
                        &self.reasoning_stream_buffer[parsed.consumed..],
                        &CompatMarkdownState::default(),
                    );
                    self.reasoning_stream_buffer.clear();
                    if !parsed.prefix.is_empty() {
                        self.emit_thinking_fragment(&parsed.prefix, tracker, tx, builder)
                            .await;
                    }
                    self.emit_compat_tool_calls(parsed.calls, tracker, tx, builder, payload)
                        .await;
                    if self.intercepting_search || self.compat_retry_requested {
                        return;
                    }
                    self.reasoning_stream_buffer = remaining;
                    continue;
                }

                if let Some(next_marker) = find_compat_tool_intent_marker_in_context(
                    &self.reasoning_stream_buffer[1..],
                    &self.reasoning_markdown_state,
                ) {
                    let recover_at = next_marker + 1;
                    warn!(
                        discarded_bytes = recover_at,
                        "Discarding malformed reasoning marker and resynchronizing at a later valid marker"
                    );
                    self.reasoning_stream_buffer.drain(..recover_at);
                    continue;
                }

                if self.reasoning_stream_buffer.len() > MAX_COMPAT_TOOL_BUFFER_SIZE {
                    warn!(
                        bytes = self.reasoning_stream_buffer.len(),
                        "Reasoning compatibility marker exceeded limit; discarding marker remainder"
                    );
                    self.reasoning_stream_buffer.clear();
                    self.discarding_reasoning_compat = true;
                    self.emit_thinking_fragment(
                        "[Oversized tool request omitted]",
                        tracker,
                        tx,
                        builder,
                    )
                    .await;
                    return;
                }
                return;
            }

            let (to_yield, pending) = split_pending_text_with_compat_prefixes(
                &self.reasoning_stream_buffer,
                &["</thinking>", "<thinking>", "</think>", "<think>"],
                payload,
                false,
            );
            self.reasoning_stream_buffer = pending;
            if !to_yield.is_empty() {
                self.emit_thinking_fragment(&to_yield, tracker, tx, builder)
                    .await;
            }
            return;
        }
    }

    async fn emit_text_fragment(
        &mut self,
        text: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
    ) {
        let cleaned = strip_system_tags_with_context(text, &self.text_markdown_state);
        if cleaned.is_empty() {
            return;
        }
        if self.has_emitted_tool_use && looks_like_unverified_tool_success(&cleaned) {
            warn!("Suppressing unverified success claim after tool_use and before tool_result");
            return;
        }

        self.text_markdown_state.advance(&cleaned);
        self.accumulated_text.push_str(&cleaned);
        if self.intercepting_search {
            return;
        }
        if let Some(idx) = tracker.close_thinking() {
            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
        }
        self.thinking_block_bytes = 0;
        let (text_idx, text_is_new, _closed) = tracker.ensure_text();
        if text_is_new {
            let _ = tx
                .send(builder.content_block_start_at(text_idx, "text", None, None))
                .await;
        }

        let mut remaining = cleaned.as_str();
        while !remaining.is_empty() {
            let (fragment, rest) = split_utf8_prefix(remaining, RENDER_DELTA_CHUNK_BYTES);
            remaining = rest;
            let _ = tx.send(builder.text_delta_at(text_idx, fragment)).await;
            trace!(
                block_index = text_idx,
                bytes = fragment.len(),
                "stream_timing anthropic_text_delta_enqueued"
            );
            if !remaining.is_empty() {
                tokio::time::sleep(LARGE_DELTA_PACING_DELAY).await;
            }
        }
    }

    async fn emit_compat_tool_calls(
        &mut self,
        calls: Vec<CompatToolCall>,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        if calls.is_empty() {
            return;
        }

        let mut resolved = Vec::with_capacity(calls.len());
        for call in calls {
            let Some(correct_name) = matching_tool_name(&call.name, payload) else {
                warn!(
                    tool = call.name,
                    "Compatibility marker requested an unavailable tool"
                );
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                }
                return;
            };
            let arguments = normalize_dsml_arguments(&correct_name, call.arguments, payload);
            if !arguments.is_object() {
                warn!(tool = %correct_name, "Compatibility marker arguments were not a JSON object");
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                }
                return;
            }
            if let Some(field) = invalid_semantic_tool_argument(&correct_name, &arguments) {
                warn!(tool = %correct_name, field, "Compatibility marker used an empty or placeholder tool argument");
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                }
                return;
            }
            resolved.push((correct_name, arguments));
        }

        let search_count = resolved
            .iter()
            .filter(|(name, _)| is_bridge_search_tool(name))
            .count();
        if search_count > 0 && resolved.len() > 1 {
            if search_count == resolved.len() {
                // Pure search batch: the emit loop below sets interception on the
                // first search call and returns; the rest are dropped and the
                // model re-issues them on later turns.
                warn!(
                    calls = resolved.len(),
                    "Collapsing compatibility batch of search calls; intercepting the first"
                );
            } else {
                // Mixed batch: drop search calls and emit the rest so the client
                // is never left waiting for a search result that is intercepted.
                warn!(
                    calls = resolved.len(),
                    searches = search_count,
                    "Dropping search calls from mixed compatibility batch; emitting non-search calls"
                );
                resolved.retain(|(name, _)| !is_bridge_search_tool(name));
            }
        }

        for (correct_name, arguments) in resolved {
            self.emit_resolved_compat_tool_use(&correct_name, arguments, tracker, tx, builder)
                .await;
            if self.intercepting_search {
                return;
            }
        }
    }

    async fn emit_resolved_compat_tool_use(
        &mut self,
        correct_name: &str,
        arguments: serde_json::Value,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
    ) {
        let fingerprint = tool_call_fingerprint(correct_name, &arguments);
        if !self.emitted_tool_fingerprints.insert(fingerprint) {
            warn!(
                tool = correct_name,
                "Suppressing duplicate compatibility tool invocation"
            );
            return;
        }
        self.encoded_tool_calls_emitted = self.encoded_tool_calls_emitted.saturating_add(1);
        let arguments_json = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".into());

        if is_bridge_search_tool(correct_name) {
            self.intercepting_search = true;
            self.search_tc_id = format!(
                "toolu_compat_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            );
            self.search_tc_name = correct_name.to_string();
            self.search_tc_args = arguments_json;
            return;
        }

        if let Some(idx) = tracker.close_thinking() {
            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
        }
        self.thinking_block_bytes = 0;
        if let Some(idx) = tracker.close_text() {
            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
        }

        let call_idx = tracker.next_index();
        let tool_id = format!(
            "toolu_compat_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            call_idx
        );
        let _ = tx
            .send(builder.content_block_start_at(
                call_idx,
                "tool_use",
                Some(&tool_id),
                Some(correct_name),
            ))
            .await;
        let _ = tx
            .send(builder.input_json_delta(call_idx, &arguments_json))
            .await;
        let _ = tx.send(crate::sse::emit_block_stop(call_idx)).await;
        self.has_emitted_tool_use = true;
        self.final_stop_reason = "tool_use".to_string();
    }

    async fn process_complete_dsml_block(
        &mut self,
        dsml_block: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        match self.classify_encoded_candidate(dsml_block, payload) {
            FallbackDecision::RetryNative | FallbackDecision::Reject => return,
            FallbackDecision::PassThrough => {
                self.emit_text_fragment(dsml_block, tracker, tx, builder)
                    .await;
                return;
            }
            FallbackDecision::ParseEncoded => {}
        }

        let (calls, malformed) = parse_dsml_tool_calls_detailed(dsml_block);
        if malformed || calls.is_empty() {
            warn!(
                bytes = dsml_block.len(),
                calls = calls.len(),
                "Rejecting malformed or empty DSML tool block"
            );
            if !self.has_emitted_tool_use {
                self.compat_retry_requested = true;
            }
            return;
        }

        let structured = calls
            .into_iter()
            .map(|call| CompatToolCall {
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        self.emit_compat_tool_calls(structured, tracker, tx, builder, payload)
            .await;
    }

    /// Process a content/text delta from the upstream stream chunk.
    ///
    /// Only a suffix that can still become a DSML or compatibility marker is
    /// retained. All other text is emitted immediately, so WebSearch-capable
    /// subagents do not wait until the final upstream chunk before displaying.
    async fn process_content_delta(
        &mut self,
        content: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        if self.intercepting_search
            || self.discarding_text_compat
            || self.compat_retry_requested
            || self.native_recovery_retry_requested
            || self.encoded_fallback_rejected
        {
            return;
        }
        if self.dsml_mode {
            self.dsml_stream_buffer.push_str(content);
        } else {
            self.text_stream_buffer.push_str(content);
        }

        loop {
            if self.intercepting_search
                || self.compat_retry_requested
                || self.native_recovery_retry_requested
                || self.encoded_fallback_rejected
            {
                return;
            }

            if self.dsml_mode {
                if self.dsml_stream_buffer.len() > MAX_DSML_BUFFER_SIZE {
                    warn!(
                        bytes = self.dsml_stream_buffer.len(),
                        "DSML stream buffer exceeded limit; emitting it as text"
                    );
                    self.dsml_stream_buffer.clear();
                    self.dsml_mode = false;
                    if !self.has_emitted_tool_use {
                        self.compat_retry_requested = true;
                    }
                    return;
                }

                let Some(end_pos) = self.dsml_stream_buffer.find(DSML_CLOSE_TAG) else {
                    return;
                };
                if self.defer_encoded_fallback_until_native_finalized {
                    return;
                }
                let end_idx = end_pos + DSML_CLOSE_TAG.len();
                let dsml_block = self.dsml_stream_buffer[..end_idx].to_string();
                let remaining = self.dsml_stream_buffer[end_idx..].to_string();
                self.dsml_stream_buffer.clear();
                self.dsml_mode = false;
                self.process_complete_dsml_block(&dsml_block, tracker, tx, builder, payload)
                    .await;
                if self.intercepting_search || self.compat_retry_requested {
                    return;
                }
                self.text_stream_buffer.push_str(&remaining);
                continue;
            }

            if self.text_stream_buffer.is_empty() {
                return;
            }

            let dsml_pos = find_literal_marker_in_context(
                &self.text_stream_buffer,
                DSML_OPEN_TAG,
                &self.text_markdown_state,
            );
            let compat_pos = find_compat_tool_intent_marker_in_context(
                &self.text_stream_buffer,
                &self.text_markdown_state,
            );
            let next_marker = match (dsml_pos, compat_pos) {
                (Some(dsml), Some(compat)) if dsml <= compat => Some((dsml, true)),
                (Some(_), Some(compat)) => Some((compat, false)),
                (Some(dsml), None) => Some((dsml, true)),
                (None, Some(compat)) => Some((compat, false)),
                (None, None) => None,
            };

            if let Some((marker_pos, is_dsml)) = next_marker {
                if marker_pos > 0 {
                    let safe_prefix = self.text_stream_buffer[..marker_pos].to_string();
                    let no_visible_output = self.accumulated_text.is_empty()
                        && self.accumulated_thinking.is_empty()
                        && !self.has_emitted_tool_use;
                    if no_visible_output
                        && safe_tool_intent_preamble(&safe_prefix)
                        && !literal_meta_output_requested(payload)
                    {
                        let raw_candidate = self.text_stream_buffer.clone();
                        match self.classify_encoded_candidate(&raw_candidate, payload) {
                            FallbackDecision::RetryNative | FallbackDecision::Reject => {
                                self.text_stream_buffer.clear();
                                return;
                            }
                            FallbackDecision::ParseEncoded => {
                                // The preamble only describes the intended tool
                                // invocation; suppress it before strict fallback.
                                self.text_stream_buffer.drain(..marker_pos);
                                continue;
                            }
                            FallbackDecision::PassThrough => {
                                // A safe execution preamble followed by an
                                // incomplete marker may be split across SSE
                                // chunks. Keep both buffered until the candidate
                                // becomes complete or EOF decides it is malformed.
                                return;
                            }
                        }
                    }

                    self.text_stream_buffer.drain(..marker_pos);
                    if looks_like_unverified_tool_success(&safe_prefix) {
                        warn!("Suppressing unverified success claim before tool_use");
                    } else {
                        self.emit_text_fragment(&safe_prefix, tracker, tx, builder)
                            .await;
                    }
                    continue;
                }

                if is_dsml {
                    self.dsml_mode = true;
                    self.dsml_stream_buffer = std::mem::take(&mut self.text_stream_buffer);
                    continue;
                }

                if let Some(parsed) =
                    parse_compat_tool_requests_with_consumed(&self.text_stream_buffer)
                {
                    if self.defer_encoded_fallback_until_native_finalized {
                        return;
                    }
                    let raw_candidate = self.text_stream_buffer.clone();
                    match self.classify_encoded_candidate(&raw_candidate, payload) {
                        FallbackDecision::RetryNative | FallbackDecision::Reject => {
                            self.text_stream_buffer.clear();
                            return;
                        }
                        FallbackDecision::PassThrough => {
                            self.text_stream_buffer.clear();
                            self.emit_text_fragment(&raw_candidate, tracker, tx, builder)
                                .await;
                            return;
                        }
                        FallbackDecision::ParseEncoded => {}
                    }
                    let remaining = strip_system_tags_with_context(
                        &self.text_stream_buffer[parsed.consumed..],
                        &CompatMarkdownState::default(),
                    );
                    self.text_stream_buffer.clear();
                    if !parsed.prefix.is_empty() {
                        self.emit_text_fragment(&parsed.prefix, tracker, tx, builder)
                            .await;
                    }
                    self.emit_compat_tool_calls(parsed.calls, tracker, tx, builder, payload)
                        .await;
                    if self.intercepting_search || self.compat_retry_requested {
                        return;
                    }
                    self.text_stream_buffer = remaining;
                    continue;
                }

                if let Some(next_marker) = find_compat_tool_intent_marker_in_context(
                    &self.text_stream_buffer[1..],
                    &self.text_markdown_state,
                ) {
                    let recover_at = next_marker + 1;
                    warn!(
                        discarded_bytes = recover_at,
                        "Discarding malformed text marker and resynchronizing at a later valid marker"
                    );
                    self.text_stream_buffer.drain(..recover_at);
                    continue;
                }

                if self.text_stream_buffer.len() > MAX_COMPAT_TOOL_BUFFER_SIZE {
                    warn!(
                        bytes = self.text_stream_buffer.len(),
                        "Compatibility marker exceeded limit; discarding marker remainder"
                    );
                    self.text_stream_buffer.clear();
                    self.discarding_text_compat = true;
                    self.emit_text_fragment(
                        "[Oversized tool request omitted]",
                        tracker,
                        tx,
                        builder,
                    )
                    .await;
                    return;
                }
                return;
            }

            let (to_yield, pending) =
                split_pending_text_for_markers(&self.text_stream_buffer, payload);
            self.text_stream_buffer = pending;
            if !to_yield.is_empty() {
                self.emit_text_fragment(&to_yield, tracker, tx, builder)
                    .await;
            }
            return;
        }
    }

    /// Process tool call deltas from the upstream stream chunk.
    ///
    /// For web search tools, sets `intercepting_search` flags and accumulates
    /// JSON arguments. For regular tool calls, opens a `tool_use` content block
    /// and emits `input_json_delta` events for the streaming arguments.
    fn process_tool_calls(&mut self, tool_calls: &[OpenAiStreamToolCall]) {
        if self.compat_retry_requested {
            return;
        }
        for tc in tool_calls {
            let pending = self.pending_tool_calls.entry(tc.index).or_default();
            if let Some(id) = &tc.id {
                merge_streamed_identifier(&mut pending.id, id);
            }
            if let Some(name) = tc
                .function
                .as_ref()
                .and_then(|function| function.name.as_ref())
            {
                merge_streamed_identifier(&mut pending.name, name);
            }
            if let Some(arguments) = tc
                .function
                .as_ref()
                .and_then(|function| function.arguments.as_ref())
            {
                merge_streamed_arguments(&mut pending.arguments, arguments);
            }
        }
    }

    async fn finalize_pending_native_tool_calls(
        &mut self,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        if self.pending_tool_calls.is_empty() || self.compat_retry_requested {
            return;
        }

        let pending = std::mem::take(&mut self.pending_tool_calls);
        let mut resolved = Vec::with_capacity(pending.len());
        for (source_index, call) in pending {
            let Some(name) = call.name else {
                warn!(source_index, "Native tool call ended without a name");
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                    self.final_stop_reason = "end_turn".to_string();
                }
                return;
            };
            let Some(correct_name) = matching_tool_name(&name, payload) else {
                warn!(
                    tool = name,
                    source_index, "Native stream requested an unavailable tool"
                );
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                    self.final_stop_reason = "end_turn".to_string();
                }
                return;
            };
            let Ok(arguments) = serde_json::from_str::<serde_json::Value>(&call.arguments) else {
                warn!(
                    tool = correct_name,
                    source_index, "Native tool arguments were not valid JSON"
                );
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                    self.final_stop_reason = "end_turn".to_string();
                }
                return;
            };
            if !arguments.is_object() {
                warn!(
                    tool = correct_name,
                    source_index, "Native tool arguments were not a JSON object"
                );
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                    self.final_stop_reason = "end_turn".to_string();
                }
                return;
            }
            let arguments = normalize_dsml_arguments(&correct_name, arguments, payload);
            if let Some(field) = invalid_semantic_tool_argument(&correct_name, &arguments) {
                warn!(
                    tool = %correct_name,
                    field,
                    source_index,
                    "Native tool call used an empty or placeholder tool argument"
                );
                if !self.has_emitted_tool_use {
                    self.compat_retry_requested = true;
                    self.final_stop_reason = "end_turn".to_string();
                }
                return;
            }
            let id = call
                .id
                .unwrap_or_else(|| format!("toolu_native_{source_index}"));
            resolved.push((source_index, id, correct_name, arguments));
        }

        let mut seen = self.emitted_tool_fingerprints.clone();
        resolved.retain(|(_, _, name, arguments)| {
            let unique = seen.insert(tool_call_fingerprint(name, arguments));
            if !unique {
                warn!(tool = name, "Suppressing duplicate native tool invocation");
            }
            unique
        });
        if resolved.is_empty() {
            return;
        }

        let search_count = resolved
            .iter()
            .filter(|(_, _, name, _)| is_bridge_search_tool(name))
            .count();
        if search_count > 0 && resolved.len() > 1 {
            if search_count == resolved.len() {
                // Pure search batch: the interception branch below handles the
                // first call; the rest never enter conversation history, so the
                // model re-issues them one at a time on later turns.
                warn!(
                    calls = resolved.len(),
                    "Collapsing native batch of search calls; intercepting the first"
                );
            } else {
                // Mixed batch: emit non-search calls normally and drop the search
                // calls instead of rejecting the whole batch. The client executes
                // the emitted calls, and the model re-issues the dropped searches
                // on the next turn as single calls, which are intercepted.
                warn!(
                    calls = resolved.len(),
                    searches = search_count,
                    "Dropping search calls from mixed native batch; emitting non-search calls"
                );
                resolved.retain(|(_, _, name, _)| !is_bridge_search_tool(name));
            }
        }

        if let Some((source_index, id, name, arguments)) = resolved
            .first()
            .filter(|(_, _, name, _)| is_bridge_search_tool(name))
        {
            // A valid native call in the same upstream response always wins over
            // any earlier encoded candidate. Do not replay or reject the turn.
            self.native_recovery_retry_requested = false;
            self.encoded_fallback_rejected = false;
            self.has_emitted_native_tool_use = true;
            self.native_tool_calls_emitted = self.native_tool_calls_emitted.saturating_add(1);
            self.emitted_tool_fingerprints
                .insert(tool_call_fingerprint(name, arguments));
            self.intercepting_search = true;
            self.search_tc_index = Some(*source_index);
            self.search_tc_id = id.clone();
            self.search_tc_name = name.clone();
            self.search_tc_args = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into());
            self.final_stop_reason = "tool_use".to_string();
            return;
        }

        if let Some(idx) = tracker.close_thinking() {
            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
        }
        self.thinking_block_bytes = 0;
        if let Some(idx) = tracker.close_text() {
            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
        }

        if !resolved.is_empty() {
            // Native tool protocol is authoritative even if encoded text was
            // observed earlier in this same response.
            self.native_recovery_retry_requested = false;
            self.encoded_fallback_rejected = false;
            self.has_emitted_native_tool_use = true;
            self.native_tool_calls_emitted = self
                .native_tool_calls_emitted
                .saturating_add(resolved.len() as u32);
        }

        for (source_index, id, correct_name, arguments) in resolved {
            self.emitted_tool_fingerprints
                .insert(tool_call_fingerprint(&correct_name, &arguments));
            let args_json = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".into());
            let (block_idx, _, _) =
                tracker.open_tool_use(source_index, id.clone(), correct_name.clone());
            let _ = tx
                .send(builder.content_block_start_at(
                    block_idx,
                    "tool_use",
                    Some(&id),
                    Some(&correct_name),
                ))
                .await;
            let _ = tx
                .send(builder.input_json_delta(block_idx, &args_json))
                .await;
            if let Some((closed_idx, _, _)) = tracker.close_tool_use(source_index) {
                debug_assert_eq!(closed_idx, block_idx);
                let _ = tx.send(crate::sse::emit_block_stop(closed_idx)).await;
            }
            self.has_emitted_tool_use = true;
        }
        if self.has_emitted_tool_use {
            self.final_stop_reason = "tool_use".to_string();
        }
    }

    /// Flush any retained marker prefix or incomplete DSML buffer at stream end.
    pub(super) async fn flush_remaining(
        &mut self,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        self.finalize_pending_native_tool_calls(tracker, tx, builder, payload)
            .await;
        if self.intercepting_search || self.has_emitted_tool_use || self.compat_retry_requested {
            return;
        }
        if self.native_recovery_retry_requested || self.encoded_fallback_rejected {
            return;
        }

        // No valid native call won this attempt. Release the strict encoded
        // fallback only now, after native fragments have been finalized.
        self.defer_encoded_fallback_until_native_finalized = false;

        // First give both rolling parsers one final chance to consume complete
        // compatibility markers already present in retained buffers.
        self.process_reasoning_delta("", tracker, tx, builder, payload)
            .await;
        if self.intercepting_search {
            return;
        }
        if !self.reasoning_stream_buffer.is_empty() {
            let pending_reasoning = std::mem::take(&mut self.reasoning_stream_buffer);
            if let Some(marker_pos) = find_compat_tool_intent_marker_in_context(
                &pending_reasoning,
                &self.reasoning_markdown_state,
            ) {
                let safe_prefix = &pending_reasoning[..marker_pos];
                if !safe_prefix.is_empty() {
                    self.emit_thinking_fragment(safe_prefix, tracker, tx, builder)
                        .await;
                }
                let marker = &pending_reasoning[marker_pos..];
                if let Some(parsed) = parse_compat_tool_requests_at_eof(marker) {
                    if !parsed.prefix.is_empty() {
                        self.emit_thinking_fragment(&parsed.prefix, tracker, tx, builder)
                            .await;
                    }
                    self.emit_compat_tool_calls(parsed.calls, tracker, tx, builder, payload)
                        .await;
                    if self.intercepting_search {
                        return;
                    }
                } else {
                    trace!(
                        bytes = marker.len(),
                        preview = ?marker.chars().take(1024).collect::<String>(),
                        "Incomplete compatibility marker debug preview"
                    );
                    if self.has_emitted_tool_use {
                        warn!("Malformed reasoning marker omitted after an emitted tool_use; upstream retry suppressed to prevent duplicate side effects");
                    } else {
                        warn!("Incomplete compatibility marker remained in reasoning at EOF; requesting retry");
                        self.compat_retry_requested = true;
                    }
                }
            } else {
                self.emit_thinking_fragment(&pending_reasoning, tracker, tx, builder)
                    .await;
            }
        }

        self.process_content_delta("", tracker, tx, builder, payload)
            .await;
        if self.intercepting_search {
            return;
        }

        if self.dsml_mode && !self.dsml_stream_buffer.is_empty() {
            let pending_dsml = std::mem::take(&mut self.dsml_stream_buffer);
            self.dsml_mode = false;
            self.process_complete_dsml_block(&pending_dsml, tracker, tx, builder, payload)
                .await;
            if self.intercepting_search {
                return;
            }
            if self.compat_retry_requested {
                return;
            }
        }

        if !self.text_stream_buffer.is_empty() {
            let pending_text = std::mem::take(&mut self.text_stream_buffer);
            if let Some(marker_pos) =
                find_compat_tool_intent_marker_in_context(&pending_text, &self.text_markdown_state)
            {
                let safe_prefix = &pending_text[..marker_pos];
                if !safe_prefix.is_empty() {
                    if looks_like_unverified_tool_success(safe_prefix) {
                        warn!("Suppressing unverified success claim before EOF tool_use");
                    } else {
                        self.emit_text_fragment(safe_prefix, tracker, tx, builder)
                            .await;
                    }
                }
                let marker = &pending_text[marker_pos..];
                if let Some(parsed) = parse_compat_tool_requests_at_eof(marker) {
                    if !parsed.prefix.is_empty() {
                        self.emit_text_fragment(&parsed.prefix, tracker, tx, builder)
                            .await;
                    }
                    self.emit_compat_tool_calls(parsed.calls, tracker, tx, builder, payload)
                        .await;
                } else {
                    trace!(
                        bytes = marker.len(),
                        preview = ?marker.chars().take(1024).collect::<String>(),
                        "Incomplete compatibility marker debug preview"
                    );
                    if self.has_emitted_tool_use {
                        warn!("Malformed text marker omitted after an emitted tool_use; upstream retry suppressed to prevent duplicate side effects");
                    } else {
                        warn!("Incomplete or malformed compatibility marker remained in text at EOF; requesting retry");
                        self.compat_retry_requested = true;
                    }
                }
            } else {
                self.emit_text_fragment(&pending_text, tracker, tx, builder)
                    .await;
            }
        }
    }
}

#[cfg(test)]
pub(super) fn split_pending_text(text: &str) -> (String, String) {
    split_pending_text_for_prefixes(text, &[DSML_OPEN_TAG])
}

fn split_pending_text_for_markers(text: &str, payload: &MessagesRequest) -> (String, String) {
    split_pending_text_with_compat_prefixes(
        text,
        &[
            DSML_OPEN_TAG,
            "</thinking>",
            "<thinking>",
            "</think>",
            "<think>",
        ],
        payload,
        true,
    )
}

fn split_pending_text_with_compat_prefixes(
    text: &str,
    markers: &[&str],
    payload: &MessagesRequest,
    hold_unverified_success: bool,
) -> (String, String) {
    if hold_unverified_success
        && payload
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        && looks_like_unverified_tool_success(text)
    {
        return (String::new(), text.to_string());
    }
    let (_, exact_pending) = split_pending_text_for_prefixes(text, markers);
    let longest = exact_pending
        .len()
        .max(compat_tool_marker_pending_suffix_len(text, payload));
    let split_idx = text.len().saturating_sub(longest);
    (text[..split_idx].to_string(), text[split_idx..].to_string())
}

fn split_pending_text_for_prefixes(text: &str, markers: &[&str]) -> (String, String) {
    let mut longest = 0;
    for marker in markers {
        for prefix_len in (1..=marker.len()).rev() {
            if !marker.is_char_boundary(prefix_len) {
                continue;
            }
            let prefix = &marker[..prefix_len];
            if text.ends_with(prefix) {
                longest = longest.max(prefix.len());
                break;
            }
        }
    }

    let split_idx = text.len().saturating_sub(longest);
    (text[..split_idx].to_string(), text[split_idx..].to_string())
}
