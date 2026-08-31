use crate::config::HistoryCaptureMode;
use crate::history::types::*;

// Runtime retention is amortized by captured logical bytes instead of running
// a full SUM/window cleanup after every tiny request. The threshold is further
// bounded by the configured database cap (see `runtime_cleanup_threshold`).
pub(crate) const MAX_RUNTIME_CLEANUP_WATERMARK_BYTES: u64 = 1024 * 1024;
pub(crate) const MIN_RUNTIME_CLEANUP_WATERMARK_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("history database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("history filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("history record not found")]
    NotFound,
    #[error("history writer is unavailable")]
    Unavailable,
    #[error("invalid history operation: {0}")]
    Invalid(String),
}

#[derive(Debug)]
pub(crate) enum HistoryCommand {
    Start {
        start: HistoryRequestStart,
        capture_mode: HistoryCaptureMode,
        inbound: Option<HistoryContent>,
        prompt_preview: Option<String>,
    },
    Complete(CompletedRecord),
}

#[derive(Debug, Clone)]
pub(crate) struct CompletedRecord {
    pub(crate) id: String,
    pub(crate) completed_at_ms: u64,
    pub(crate) duration_ms: u64,
    pub(crate) time_to_first_chunk_ms: Option<u64>,
    pub(crate) status: String,
    pub(crate) http_status: Option<u16>,
    pub(crate) finish_reason: Option<String>,
    pub(crate) error_type: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) response_model: Option<String>,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) retry_count: u32,
    pub(crate) fallback_count: u32,
    pub(crate) tool_call_count: u32,
    pub(crate) search_count: u32,
    pub(crate) capture_incomplete: bool,
    pub(crate) redacted: bool,
    pub(crate) truncated: bool,
    pub(crate) contents: Vec<HistoryContent>,
    pub(crate) attempts: Vec<HistoryAttempt>,
    pub(crate) events: Vec<HistoryEvent>,
}

#[derive(Debug, Default)]
pub(crate) struct CaptureDraft {
    pub(crate) effective: Option<HistoryContent>,
    pub(crate) reasoning: String,
    pub(crate) response: String,
    pub(crate) provider_raw_response: String,
    pub(crate) response_model: Option<String>,
    pub(crate) finish_reason: Option<String>,
    pub(crate) http_status: Option<u16>,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) retry_count: u32,
    pub(crate) fallback_count: u32,
    pub(crate) tool_call_count: u32,
    pub(crate) search_count: u32,
    pub(crate) first_chunk_ms: Option<u64>,
    pub(crate) capture_incomplete: bool,
    pub(crate) redacted: bool,
    pub(crate) truncated: bool,
    pub(crate) attempts: Vec<HistoryAttempt>,
    pub(crate) events: Vec<HistoryEvent>,
    pub(crate) event_sequence: u32,
}
