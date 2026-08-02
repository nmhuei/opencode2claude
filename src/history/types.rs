use crate::config::{HistoryCaptureMode, HistoryConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct HistoryRequestStart {
    pub id: String,
    pub conversation_id: Option<String>,
    pub parent_request_id: Option<String>,
    pub protocol: String,
    pub endpoint: String,
    pub operation_kind: String,
    pub client_key_id: Option<String>,
    pub client_name: Option<String>,
    pub client_environment: Option<String>,
    pub requested_model: Option<String>,
    pub effective_model: Option<String>,
    pub stream: bool,
    pub thinking_requested: bool,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub inbound: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryAttempt {
    pub attempt_number: u32,
    pub loop_number: u32,
    pub attempt_kind: String,
    pub model: Option<String>,
    pub proxy_node: Option<String>,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub status: String,
    pub finish_reason: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub payload_sha256: Option<String>,
    pub payload_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub sequence: u32,
    pub timestamp_ms: u64,
    pub event_type: String,
    pub severity: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryContentDescriptor {
    pub kind: String,
    pub content_type: String,
    pub original_bytes: usize,
    pub stored_bytes: usize,
    pub sha256: String,
    pub redacted: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryContent {
    pub descriptor: HistoryContentDescriptor,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryListItem {
    pub id: String,
    pub conversation_id: Option<String>,
    pub parent_request_id: Option<String>,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub time_to_first_chunk_ms: Option<u64>,
    pub protocol: String,
    pub endpoint: String,
    pub operation_kind: String,
    pub client_key_id: Option<String>,
    pub client_name: Option<String>,
    pub client_environment: Option<String>,
    pub requested_model: Option<String>,
    pub effective_model: Option<String>,
    pub response_model: Option<String>,
    pub stream: bool,
    pub thinking_requested: bool,
    pub reasoning_effort: Option<String>,
    pub status: String,
    pub http_status: Option<u16>,
    pub finish_reason: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub retry_count: u32,
    pub fallback_count: u32,
    pub tool_call_count: u32,
    pub search_count: u32,
    pub prompt_preview: Option<String>,
    pub response_preview: Option<String>,
    pub capture_mode: String,
    pub capture_incomplete: bool,
    pub redacted: bool,
    pub truncated: bool,
    pub stored_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryDetail {
    pub request: HistoryListItem,
    pub contents: Vec<HistoryContentDescriptor>,
    pub attempts: Vec<HistoryAttempt>,
    pub events: Vec<HistoryEvent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryQuery {
    pub q: Option<String>,
    pub status: Option<String>,
    pub protocol: Option<String>,
    pub model: Option<String>,
    pub client_id: Option<String>,
    pub thinking: Option<bool>,
    pub stream: Option<bool>,
    pub has_error: Option<bool>,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryPage {
    pub items: Vec<HistoryListItem>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryStats {
    pub total: u64,
    pub today: u64,
    pub success: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub average_latency_ms: u64,
    pub stored_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryStorageStatus {
    pub enabled: bool,
    pub available: bool,
    pub capture_mode: HistoryCaptureMode,
    pub path: String,
    pub logical_bytes: u64,
    pub physical_bytes: u64,
    pub records: u64,
    pub last_error: Option<String>,
    pub retention_days: u32,
    pub max_records: usize,
    pub max_database_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySettingsView {
    pub enabled: bool,
    pub capture_mode: HistoryCaptureMode,
    pub capture_inbound: bool,
    pub capture_effective: bool,
    pub capture_reasoning: bool,
    pub capture_response: bool,
    pub capture_tools: bool,
    pub capture_search_queries: bool,
    pub capture_search_results: bool,
    pub capture_shell_commands: bool,
    pub retention_days: u32,
    pub max_records: usize,
    pub max_database_bytes: u64,
}

impl From<&HistoryConfig> for HistorySettingsView {
    fn from(config: &HistoryConfig) -> Self {
        Self {
            enabled: config.enabled,
            capture_mode: config.capture_mode,
            capture_inbound: config.capture_inbound,
            capture_effective: config.capture_effective,
            capture_reasoning: config.capture_reasoning,
            capture_response: config.capture_response,
            capture_tools: config.capture_tools,
            capture_search_queries: config.capture_search_queries,
            capture_search_results: config.capture_search_results,
            capture_shell_commands: config.capture_shell_commands,
            retention_days: config.retention_days,
            max_records: config.max_records,
            max_database_bytes: config.max_database_bytes,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistorySettingsPatch {
    pub enabled: Option<bool>,
    pub capture_mode: Option<HistoryCaptureMode>,
    pub retention_days: Option<u32>,
    pub max_records: Option<usize>,
    pub max_database_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryPurgeRequest {
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub all: bool,
    pub before_ms: Option<u64>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryExportRequest {
    pub ids: Option<Vec<String>>,
    pub query: Option<HistoryQuery>,
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryExport {
    pub generated_at_ms: u64,
    pub format: String,
    pub records: Vec<HistoryExportRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryExportRecord {
    pub detail: HistoryDetail,
    pub content: Vec<HistoryContent>,
}
