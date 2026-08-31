use super::types::*;
use crate::history::redact::{as_content, capture_json, capture_text, preview};
use crate::history::types::*;
use crate::proxy_pool::RouteMetadata;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct HistoryCapture {
    pub(crate) inner: Option<Arc<HistoryCaptureInner>>,
}

impl HistoryCapture {
    pub(crate) fn disabled(_request_id: String) -> Self {
        Self { inner: None }
    }

    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn request_id(&self) -> Option<&str> {
        self.inner.as_ref().map(|inner| inner.request_id.as_str())
    }

    pub fn effective_json(
        &self,
        value: &Value,
        model: Option<&str>,
        attempt_kind: &str,
        loop_number: u32,
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        if !inner.config.capture_effective {
            return;
        }
        let captured = capture_json(
            value,
            inner.config.capture_mode,
            inner.config.max_request_bytes,
        );
        let mut draft = lock_draft(inner);
        if let Some(captured) = captured {
            let payload_hash = captured.sha256.clone();
            draft.redacted |= captured.redacted;
            draft.truncated |= captured.truncated;
            draft.effective = Some(as_content(
                "effective_request",
                "application/json",
                captured,
            ));
            let attempt_number = draft.attempts.len() as u32 + 1;
            draft.attempts.push(HistoryAttempt {
                attempt_number,
                loop_number,
                attempt_kind: attempt_kind.to_string(),
                model: model.map(ToOwned::to_owned),
                proxy_node: None,
                route_kind: None,
                started_at_ms: super::sql::now_ms(),
                completed_at_ms: None,
                duration_ms: None,
                http_status: None,
                status: "started".to_string(),
                finish_reason: None,
                error_type: None,
                error_message: None,
                payload_sha256: Some(payload_hash),
                payload_changed: attempt_number > 1,
            });
        }
        add_event_locked(
            &mut draft,
            "upstream_attempt_started",
            "info",
            json!({"attempt_kind":attempt_kind,"loop_number":loop_number,"model":model}),
        );
    }

    pub fn attempt_route(&self, route: &RouteMetadata) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        if let Some(attempt) = draft.attempts.last_mut() {
            attempt.proxy_node = route.proxy_node.clone();
            attempt.route_kind = Some(route.kind);
        }
        add_event_locked(
            &mut draft,
            "upstream_route_selected",
            "info",
            json!({
                "route_kind": super::sql::route_kind_label(route.kind),
                "proxy_node": route.proxy_node,
            }),
        );
    }

    pub fn attempt_finished(
        &self,
        http_status: Option<u16>,
        status: &str,
        finish_reason: Option<&str>,
        error_type: Option<&str>,
        error_message: Option<&str>,
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        if let Some(attempt) = draft.attempts.last_mut() {
            let completed = super::sql::now_ms();
            attempt.completed_at_ms = Some(completed);
            attempt.duration_ms = Some(completed.saturating_sub(attempt.started_at_ms));
            attempt.http_status = http_status;
            attempt.status = status.to_string();
            attempt.finish_reason = finish_reason.map(ToOwned::to_owned);
            attempt.error_type = error_type.map(ToOwned::to_owned);
            attempt.error_message = error_message.map(|value| preview(value, 500));
        }
        add_event_locked(
            &mut draft,
            "upstream_attempt_finished",
            if status == "completed" {
                "info"
            } else {
                "warn"
            },
            json!({"status":status,"http_status":http_status,"finish_reason":finish_reason,"error_type":error_type}),
        );
    }

    pub fn first_chunk(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        if draft.first_chunk_ms.is_none() {
            draft.first_chunk_ms = Some(inner.started.elapsed().as_millis() as u64);
            add_event_locked(&mut draft, "first_chunk", "info", json!({}));
        }
    }

    pub fn append_reasoning(&self, value: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        if !inner.config.capture_reasoning {
            return;
        }
        let mut draft = lock_draft(inner);
        if append_bounded(
            &mut draft.reasoning,
            value,
            inner.config.max_reasoning_bytes,
        ) {
            draft.truncated = true;
        }
    }

    pub fn append_response(&self, value: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        if !inner.config.capture_response {
            return;
        }
        let mut draft = lock_draft(inner);
        if append_bounded(&mut draft.response, value, inner.config.max_response_bytes) {
            draft.truncated = true;
        }
    }

    pub fn provider_raw_response(&self, value: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        if append_bounded(
            &mut draft.provider_raw_response,
            value,
            inner.config.max_response_bytes,
        ) {
            draft.truncated = true;
        }
    }

    pub fn search(&self, query: &str, result: Option<&str>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        draft.search_count = draft.search_count.saturating_add(1);
        let mut metadata = json!({"query": if inner.config.capture_search_queries { query } else { "[NOT CAPTURED]" }});
        if inner.config.capture_search_results {
            if let Some(result) = result {
                metadata["result_preview"] = Value::String(preview(result, 1000));
                metadata["result_bytes"] = json!(result.len());
            }
        }
        add_event_locked(&mut draft, "search_completed", "info", metadata);
    }

    pub fn tool_call(&self, name: &str, arguments: Option<&str>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        draft.tool_call_count = draft.tool_call_count.saturating_add(1);
        let arguments = if inner.config.capture_tools {
            arguments.map(|value| preview(value, 2000))
        } else {
            None
        };
        add_event_locked(
            &mut draft,
            "tool_call",
            "info",
            json!({"name":name,"arguments":arguments}),
        );
    }

    /// Record protocol-path metadata without tool arguments or response payloads.
    pub fn tool_protocol(&self, event: &str, origin: &str, count: u64, reason: Option<&str>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        add_event_locked(
            &mut draft,
            "tool_protocol",
            if event.contains("reject") {
                "warn"
            } else {
                "info"
            },
            json!({
                "event": preview(event, 80),
                "origin": preview(origin, 40),
                "count": count,
                "reason": reason.map(|value| preview(value, 160)),
            }),
        );
    }

    pub fn retry(&self, class: &str, backoff_ms: Option<u64>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        draft.retry_count = draft.retry_count.saturating_add(1);
        add_event_locked(
            &mut draft,
            "retry_scheduled",
            "warn",
            json!({"class":class,"backoff_ms":backoff_ms}),
        );
    }

    pub fn fallback(&self, from: &str, to: &str, reason: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        draft.fallback_count = draft.fallback_count.saturating_add(1);
        add_event_locked(
            &mut draft,
            "model_fallback",
            "warn",
            json!({"from":from,"to":to,"reason":reason}),
        );
    }

    pub fn response_model(&self, model: Option<&str>) {
        if let Some(inner) = &self.inner {
            lock_draft(inner).response_model = model.map(ToOwned::to_owned);
        }
    }

    pub fn usage(&self, input: Option<u64>, output: Option<u64>, reasoning: Option<u64>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut draft = lock_draft(inner);
        draft.input_tokens = input;
        draft.output_tokens = output;
        draft.reasoning_tokens = reasoning;
    }

    pub fn finish_success(
        &self,
        http_status: u16,
        finish_reason: Option<&str>,
        response_model: Option<&str>,
    ) {
        self.finish(
            "completed",
            Some(http_status),
            finish_reason,
            response_model,
            None,
            None,
        );
    }

    pub fn fail(&self, http_status: Option<u16>, error_type: &str, message: &str) {
        self.finish(
            "failed",
            http_status,
            None,
            None,
            Some(error_type),
            Some(message),
        );
    }

    pub fn cancel(&self) {
        self.finish(
            "cancelled",
            None,
            None,
            None,
            Some("client_cancelled"),
            Some("client disconnected before the response completed"),
        );
    }

    fn finish(
        &self,
        status: &str,
        http_status: Option<u16>,
        finish_reason: Option<&str>,
        response_model: Option<&str>,
        error_type: Option<&str>,
        error_message: Option<&str>,
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        if inner.completed.swap(true, Ordering::AcqRel) {
            return;
        }
        let record = build_completed_record(
            inner,
            status,
            http_status,
            finish_reason,
            response_model,
            error_type,
            error_message,
        );
        inner.store.send_completion(record);
    }
}

#[derive(Debug)]
pub(crate) struct HistoryCaptureInner {
    pub(crate) store: Arc<super::HistoryStore>,
    pub(crate) config: crate::config::HistoryConfig,
    pub(crate) request_id: String,
    pub(crate) started: Instant,
    pub(crate) completed: AtomicBool,
    pub(crate) draft: Mutex<CaptureDraft>,
}

impl Drop for HistoryCaptureInner {
    fn drop(&mut self) {
        if self.completed.swap(true, Ordering::AcqRel) {
            return;
        }
        let record = build_completed_record(
            self,
            "cancelled",
            None,
            None,
            None,
            Some("capture_dropped"),
            Some("request capture ended without an explicit terminal event"),
        );
        self.store.send_completion(record);
    }
}

pub(crate) fn build_completed_record(
    inner: &HistoryCaptureInner,
    status: &str,
    http_status: Option<u16>,
    finish_reason: Option<&str>,
    response_model: Option<&str>,
    error_type: Option<&str>,
    error_message: Option<&str>,
) -> CompletedRecord {
    let completed_at_ms = super::sql::now_ms();
    let mut draft = lock_draft(inner);
    draft.http_status = http_status.or(draft.http_status);
    draft.finish_reason = finish_reason
        .map(ToOwned::to_owned)
        .or_else(|| draft.finish_reason.clone());
    draft.response_model = response_model
        .map(ToOwned::to_owned)
        .or_else(|| draft.response_model.clone());
    let terminal_metadata = json!({
        "status": status,
        "http_status": draft.http_status,
        "finish_reason": draft.finish_reason,
    });
    add_event_locked(
        &mut draft,
        match status {
            "completed" => "request_completed",
            "failed" => "request_failed",
            "cancelled" => "client_cancelled",
            _ => "request_finished",
        },
        if status == "completed" {
            "info"
        } else {
            "warn"
        },
        terminal_metadata,
    );

    let mut contents = Vec::new();
    if let Some(effective) = draft.effective.clone() {
        contents.push(effective);
    }
    if !draft.reasoning.is_empty() {
        if let Some(captured) = capture_text(
            &draft.reasoning,
            inner.config.capture_mode,
            inner.config.max_reasoning_bytes,
        ) {
            draft.redacted |= captured.redacted;
            draft.truncated |= captured.truncated;
            contents.push(as_content("reasoning", "text/plain", captured));
        }
    }
    if !draft.response.is_empty() {
        if let Some(captured) = capture_text(
            &draft.response,
            inner.config.capture_mode,
            inner.config.max_response_bytes,
        ) {
            draft.redacted |= captured.redacted;
            draft.truncated |= captured.truncated;
            contents.push(as_content("response", "text/plain", captured));
        }
    }
    if !draft.provider_raw_response.is_empty() {
        if let Some(captured) = capture_text(
            &draft.provider_raw_response,
            inner.config.capture_mode,
            inner.config.max_response_bytes,
        ) {
            draft.redacted |= captured.redacted;
            draft.truncated |= captured.truncated;
            contents.push(as_content(
                "provider_raw_response",
                "application/json",
                captured,
            ));
        }
    }

    let stored_bytes = contents
        .iter()
        .map(|content| content.descriptor.stored_bytes)
        .sum::<usize>();
    if stored_bytes > inner.config.max_record_bytes {
        draft.capture_incomplete = true;
        draft.truncated = true;
    }

    CompletedRecord {
        id: inner.request_id.clone(),
        completed_at_ms,
        duration_ms: inner.started.elapsed().as_millis() as u64,
        time_to_first_chunk_ms: draft.first_chunk_ms,
        status: status.to_string(),
        http_status: draft.http_status,
        finish_reason: draft.finish_reason.clone(),
        error_type: error_type.map(ToOwned::to_owned),
        error_message: error_message.map(|message| preview(message, 1000)),
        response_model: draft.response_model.clone(),
        input_tokens: draft.input_tokens,
        output_tokens: draft.output_tokens,
        reasoning_tokens: draft.reasoning_tokens,
        retry_count: draft.retry_count,
        fallback_count: draft.fallback_count,
        tool_call_count: draft.tool_call_count,
        search_count: draft.search_count,
        capture_incomplete: draft.capture_incomplete,
        redacted: draft.redacted,
        truncated: draft.truncated,
        contents,
        attempts: draft.attempts.clone(),
        events: draft.events.clone(),
    }
}

fn lock_draft(inner: &HistoryCaptureInner) -> MutexGuard<'_, CaptureDraft> {
    inner
        .draft
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn append_bounded(target: &mut String, value: &str, max_bytes: usize) -> bool {
    if target.len() >= max_bytes {
        return true;
    }
    let remaining = max_bytes - target.len();
    if value.len() <= remaining {
        target.push_str(value);
        return false;
    }
    let mut end = remaining;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&value[..end]);
    true
}

fn add_event_locked(draft: &mut CaptureDraft, event_type: &str, severity: &str, metadata: Value) {
    draft.event_sequence = draft.event_sequence.saturating_add(1);
    draft.events.push(HistoryEvent {
        sequence: draft.event_sequence,
        timestamp_ms: super::sql::now_ms(),
        event_type: event_type.to_string(),
        severity: severity.to_string(),
        metadata,
    });
}
