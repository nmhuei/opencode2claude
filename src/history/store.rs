use super::redact::{as_content, capture_json, capture_text, preview};
use super::types::*;
use crate::config::{HistoryCaptureMode, HistoryConfig};
use crate::proxy_pool::{RouteKind, RouteMetadata};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

// Runtime retention is amortized by captured logical bytes instead of running
// a full SUM/window cleanup after every tiny request. The threshold is further
// bounded by the configured database cap (see `runtime_cleanup_threshold`).
const MAX_RUNTIME_CLEANUP_WATERMARK_BYTES: u64 = 1024 * 1024;
const MIN_RUNTIME_CLEANUP_WATERMARK_BYTES: u64 = 64 * 1024;

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
enum HistoryCommand {
    Start {
        start: HistoryRequestStart,
        capture_mode: HistoryCaptureMode,
        inbound: Option<HistoryContent>,
        prompt_preview: Option<String>,
    },
    Complete(CompletedRecord),
}

#[derive(Debug, Clone)]
struct CompletedRecord {
    id: String,
    completed_at_ms: u64,
    duration_ms: u64,
    time_to_first_chunk_ms: Option<u64>,
    status: String,
    http_status: Option<u16>,
    finish_reason: Option<String>,
    error_type: Option<String>,
    error_message: Option<String>,
    response_model: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    retry_count: u32,
    fallback_count: u32,
    tool_call_count: u32,
    search_count: u32,
    capture_incomplete: bool,
    redacted: bool,
    truncated: bool,
    contents: Vec<HistoryContent>,
    attempts: Vec<HistoryAttempt>,
    events: Vec<HistoryEvent>,
}

#[derive(Debug, Default)]
struct CaptureDraft {
    effective: Option<HistoryContent>,
    reasoning: String,
    response: String,
    provider_raw_response: String,
    response_model: Option<String>,
    finish_reason: Option<String>,
    http_status: Option<u16>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    retry_count: u32,
    fallback_count: u32,
    tool_call_count: u32,
    search_count: u32,
    first_chunk_ms: Option<u64>,
    capture_incomplete: bool,
    redacted: bool,
    truncated: bool,
    attempts: Vec<HistoryAttempt>,
    events: Vec<HistoryEvent>,
    event_sequence: u32,
}

#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    settings: Arc<RwLock<HistoryConfig>>,
    sender: Option<SyncSender<HistoryCommand>>,
    writer_connection: Option<Arc<Mutex<Connection>>>,
    runtime_cleanup_bytes: Arc<AtomicU64>,
    available: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl HistoryStore {
    pub fn open(mut config: HistoryConfig, default_path: PathBuf) -> Arc<Self> {
        let path = config.path.clone().unwrap_or(default_path);
        let mut available = true;
        let mut last_error = None;
        if let Err(error) = prepare_database(&path, &mut config) {
            available = false;
            last_error = Some(error.to_string());
            warn!(error = %error, path = %path.display(), "request history database unavailable; inference will continue");
        }
        let settings = Arc::new(RwLock::new(config.clone()));

        // The connection is shared between the background writer thread and the
        // queue-full fallback path behind one mutex, so every write stays
        // serialized in a single-writer discipline (no SQLITE_BUSY contention).
        let mut sender = None;
        let mut writer_connection = None;
        let runtime_cleanup_bytes = Arc::new(AtomicU64::new(0));
        if available {
            match open_connection(&path) {
                Ok(connection) => {
                    let connection = Arc::new(Mutex::new(connection));
                    let (queue_sender, receiver) = sync_channel(config.queue_capacity.max(1));
                    let writer_path = path.clone();
                    let writer_connection_thread = Arc::clone(&connection);
                    let writer_cleanup_bytes = Arc::clone(&runtime_cleanup_bytes);
                    let writer_settings = Arc::clone(&settings);
                    let spawned = std::thread::Builder::new()
                        .name("opencode2api-history-writer".to_string())
                        .spawn(move || {
                            while let Ok(command) = receiver.recv() {
                                let result = {
                                    let guard = writer_connection_thread
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    match command {
                                        HistoryCommand::Start {
                                            start,
                                            capture_mode,
                                            inbound,
                                            prompt_preview,
                                        } => insert_start(
                                            &guard,
                                            &start,
                                            capture_mode,
                                            inbound.as_ref(),
                                            prompt_preview.as_deref(),
                                        ),
                                        HistoryCommand::Complete(record) => {
                                            let added_bytes =
                                                completed_record_stored_bytes(&record);
                                            let cleanup_config = writer_settings
                                                .read()
                                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                                .clone();
                                            complete_record(&guard, &record).and_then(|_| {
                                                maybe_runtime_cleanup(
                                                    &guard,
                                                    &cleanup_config,
                                                    &writer_cleanup_bytes,
                                                    added_bytes,
                                                )
                                            })
                                        }
                                    }
                                };
                                if let Err(error) = result {
                                    warn!(error = %error, "request history write failed");
                                }
                            }
                        });
                    match spawned {
                        Ok(_join_handle) => {
                            sender = Some(queue_sender);
                            writer_connection = Some(Arc::clone(&connection));
                        }
                        Err(error) => {
                            available = false;
                            last_error = Some(error.to_string());
                            warn!(error = %error, path = %writer_path.display(), "history writer thread unavailable; inference will continue");
                        }
                    }
                }
                Err(error) => {
                    available = false;
                    last_error = Some(error.to_string());
                    warn!(error = %error, path = %path.display(), "history writer failed to open database; inference will continue");
                }
            }
        }

        Arc::new(Self {
            path,
            settings,
            sender,
            writer_connection,
            runtime_cleanup_bytes,
            available: AtomicBool::new(available),
            last_error: Mutex::new(last_error),
        })
    }

    pub fn begin(self: &Arc<Self>, start: HistoryRequestStart) -> HistoryCapture {
        let config = self.settings();
        if !config.enabled || config.capture_mode == HistoryCaptureMode::Off || !self.is_available()
        {
            return HistoryCapture::disabled(start.id);
        }

        let inbound = if config.capture_inbound
            && (start.operation_kind != "shell" || config.capture_shell_commands)
        {
            start.inbound.as_ref().and_then(|value| {
                capture_json(value, config.capture_mode, config.max_request_bytes)
                    .map(|captured| as_content("inbound_request", "application/json", captured))
            })
        } else {
            None
        };
        let prompt_preview = inbound.as_ref().map(|content| preview(&content.body, 220));
        let command = HistoryCommand::Start {
            start: start.clone(),
            capture_mode: config.capture_mode,
            inbound,
            prompt_preview,
        };
        let start_dropped = !self.try_send(command);

        HistoryCapture {
            inner: Some(Arc::new(HistoryCaptureInner {
                store: self.clone(),
                config,
                request_id: start.id,
                started: Instant::now(),
                completed: AtomicBool::new(false),
                draft: Mutex::new(CaptureDraft {
                    capture_incomplete: start_dropped,
                    ..Default::default()
                }),
            })),
        }
    }

    pub fn settings(&self) -> HistoryConfig {
        self.settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn update_settings(
        &self,
        patch: HistorySettingsPatch,
    ) -> Result<HistorySettingsView, HistoryError> {
        let mut snapshot = self.settings();
        if let Some(enabled) = patch.enabled {
            snapshot.enabled = enabled;
        }
        if let Some(mode) = patch.capture_mode {
            snapshot.capture_mode = mode;
        }
        if let Some(days) = patch.retention_days {
            snapshot.retention_days = days.min(3650);
        }
        if let Some(records) = patch.max_records {
            snapshot.max_records = records.clamp(1, 1_000_000);
        }
        if let Some(bytes) = patch.max_database_bytes {
            snapshot.max_database_bytes = bytes.max(1024 * 1024);
        }

        let connection = open_connection(&self.path)?;
        persist_settings(&connection, &snapshot)?;
        cleanup(&connection, &snapshot)?;
        *self
            .settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot.clone();
        Ok(HistorySettingsView::from(&snapshot))
    }

    pub fn settings_view(&self) -> HistorySettingsView {
        HistorySettingsView::from(&self.settings())
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed) && self.sender.is_some()
    }

    pub fn list(&self, query: HistoryQuery) -> Result<HistoryPage, HistoryError> {
        let connection = open_connection(&self.path)?;
        let limit = query.limit.unwrap_or(50).clamp(1, 100);
        let offset = query.offset.unwrap_or(0);
        let q_pattern = query
            .q
            .as_ref()
            .map(|value| format!("%{}%", value.trim()))
            .filter(|value| value != "%%");
        let thinking = query.thinking.map(i64::from);
        let stream = query.stream.map(i64::from);
        let has_error = query.has_error.map(i64::from);
        let from_ms = query.from_ms.map(as_i64);
        let to_ms = query.to_ms.map(as_i64);

        let where_sql = "
            WHERE operation_kind != 'response_recovery'
              AND (?1 IS NULL OR status = ?1)
              AND (?2 IS NULL OR protocol = ?2)
              AND (?3 IS NULL OR effective_model = ?3 OR requested_model = ?3 OR response_model = ?3)
              AND (?4 IS NULL OR client_key_id = ?4)
              AND (?5 IS NULL OR thinking_requested = ?5)
              AND (?6 IS NULL OR stream = ?6)
              AND (?7 IS NULL OR (error_type IS NOT NULL) = ?7)
              AND (?8 IS NULL OR started_at_ms >= ?8)
              AND (?9 IS NULL OR started_at_ms <= ?9)
              AND (?10 IS NULL OR id LIKE ?10 OR prompt_preview LIKE ?10 OR response_preview LIKE ?10 OR client_name LIKE ?10 OR effective_model LIKE ?10)
        ";
        let count_sql = format!("SELECT COUNT(*) FROM history_requests {where_sql}");
        let total: i64 = connection.query_row(
            &count_sql,
            params![
                query.status,
                query.protocol,
                query.model,
                query.client_id,
                thinking,
                stream,
                has_error,
                from_ms,
                to_ms,
                q_pattern,
            ],
            |row| row.get(0),
        )?;

        let select_sql = format!(
            "SELECT {} FROM history_requests {where_sql} ORDER BY started_at_ms DESC LIMIT ?11 OFFSET ?12",
            HISTORY_COLUMNS
        );
        let mut statement = connection.prepare(&select_sql)?;
        let items = statement
            .query_map(
                params![
                    query.status,
                    query.protocol,
                    query.model,
                    query.client_id,
                    thinking,
                    stream,
                    has_error,
                    from_ms,
                    to_ms,
                    q_pattern,
                    as_i64(limit as u64),
                    as_i64(offset as u64),
                ],
                row_to_item,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HistoryPage {
            items,
            total: total.max(0) as u64,
            limit,
            offset,
        })
    }

    pub fn detail(&self, id: &str) -> Result<Option<HistoryDetail>, HistoryError> {
        let connection = open_connection(&self.path)?;
        let request = connection
            .query_row(
                &format!("SELECT {HISTORY_COLUMNS} FROM history_requests WHERE id = ?1"),
                [id],
                row_to_item,
            )
            .optional()?;
        let Some(request) = request else {
            return Ok(None);
        };

        let mut content_statement = connection.prepare(
            "SELECT kind, content_type, original_bytes, stored_bytes, sha256, redacted, truncated
             FROM history_content WHERE request_id = ?1 ORDER BY sequence, id",
        )?;
        let contents = content_statement
            .query_map([id], |row| {
                Ok(HistoryContentDescriptor {
                    kind: row.get(0)?,
                    content_type: row.get(1)?,
                    original_bytes: row.get::<_, i64>(2)?.max(0) as usize,
                    stored_bytes: row.get::<_, i64>(3)?.max(0) as usize,
                    sha256: row.get(4)?,
                    redacted: row.get::<_, i64>(5)? != 0,
                    truncated: row.get::<_, i64>(6)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut attempt_statement = connection.prepare(
            "SELECT attempt_number, loop_number, attempt_kind, model, proxy_node, route_kind, started_at_ms,
                    completed_at_ms, duration_ms, http_status, status, finish_reason, error_type,
                    error_message, payload_sha256, payload_changed
             FROM history_attempts WHERE request_id = ?1 ORDER BY attempt_number, id",
        )?;
        let attempts = attempt_statement
            .query_map([id], |row| {
                Ok(HistoryAttempt {
                    attempt_number: row.get::<_, i64>(0)?.max(0) as u32,
                    loop_number: row.get::<_, i64>(1)?.max(0) as u32,
                    attempt_kind: row.get(2)?,
                    model: row.get(3)?,
                    proxy_node: row.get(4)?,
                    route_kind: row
                        .get::<_, Option<String>>(5)?
                        .as_deref()
                        .and_then(parse_route_kind),
                    started_at_ms: row.get::<_, i64>(6)?.max(0) as u64,
                    completed_at_ms: optional_u64(row, 7)?,
                    duration_ms: optional_u64(row, 8)?,
                    http_status: row.get::<_, Option<i64>>(9)?.map(|value| value as u16),
                    status: row.get(10)?,
                    finish_reason: row.get(11)?,
                    error_type: row.get(12)?,
                    error_message: row.get(13)?,
                    payload_sha256: row.get(14)?,
                    payload_changed: row.get::<_, i64>(15)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut event_statement = connection.prepare(
            "SELECT sequence, timestamp_ms, event_type, severity, metadata_json
             FROM history_events WHERE request_id = ?1 ORDER BY sequence, id",
        )?;
        let events = event_statement
            .query_map([id], |row| {
                let metadata: String = row.get(4)?;
                Ok(HistoryEvent {
                    sequence: row.get::<_, i64>(0)?.max(0) as u32,
                    timestamp_ms: row.get::<_, i64>(1)?.max(0) as u64,
                    event_type: row.get(2)?,
                    severity: row.get(3)?,
                    metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| json!({})),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(HistoryDetail {
            request,
            contents,
            attempts,
            events,
        }))
    }

    pub fn content(&self, id: &str, kind: &str) -> Result<Option<HistoryContent>, HistoryError> {
        let connection = open_connection(&self.path)?;
        connection
            .query_row(
                "SELECT kind, content_type, body, original_bytes, stored_bytes, sha256, redacted, truncated
                 FROM history_content WHERE request_id = ?1 AND kind = ?2 ORDER BY sequence DESC LIMIT 1",
                params![id, kind],
                |row| {
                    Ok(HistoryContent {
                        descriptor: HistoryContentDescriptor {
                            kind: row.get(0)?,
                            content_type: row.get(1)?,
                            original_bytes: row.get::<_, i64>(3)?.max(0) as usize,
                            stored_bytes: row.get::<_, i64>(4)?.max(0) as usize,
                            sha256: row.get(5)?,
                            redacted: row.get::<_, i64>(6)? != 0,
                            truncated: row.get::<_, i64>(7)? != 0,
                        },
                        body: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn stats(&self) -> Result<HistoryStats, HistoryError> {
        let connection = open_connection(&self.path)?;
        let today_ms = (now_ms() / 86_400_000) * 86_400_000;
        connection
            .query_row(
                "SELECT COUNT(*),
                        SUM(CASE WHEN started_at_ms >= ?1 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END),
                        COALESCE(AVG(duration_ms), 0),
                        COALESCE(SUM(stored_bytes), 0)
                 FROM history_requests
                 WHERE operation_kind != 'response_recovery'",
                [as_i64(today_ms)],
                |row| {
                    Ok(HistoryStats {
                        total: row.get::<_, i64>(0)?.max(0) as u64,
                        today: row.get::<_, i64>(1)?.max(0) as u64,
                        success: row.get::<_, i64>(2)?.max(0) as u64,
                        failed: row.get::<_, i64>(3)?.max(0) as u64,
                        cancelled: row.get::<_, i64>(4)?.max(0) as u64,
                        average_latency_ms: row.get::<_, f64>(5)?.max(0.0) as u64,
                        stored_bytes: row.get::<_, i64>(6)?.max(0) as u64,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn storage_status(&self) -> Result<HistoryStorageStatus, HistoryError> {
        let stats = self.stats().unwrap_or_default();
        let config = self.settings();
        let physical_bytes = fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        Ok(HistoryStorageStatus {
            enabled: config.enabled,
            available: self.is_available(),
            capture_mode: config.capture_mode,
            path: self.path.display().to_string(),
            logical_bytes: stats.stored_bytes,
            physical_bytes,
            records: stats.total,
            last_error: self
                .last_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            retention_days: config.retention_days,
            max_records: config.max_records,
            max_database_bytes: config.max_database_bytes,
        })
    }

    pub fn delete(&self, id: &str) -> Result<bool, HistoryError> {
        let mut connection = open_connection(&self.path)?;
        let transaction = connection.transaction()?;
        let affected = transaction.execute(
            "DELETE FROM history_requests WHERE id = ?1 OR parent_request_id = ?1",
            [id],
        )?;
        transaction.commit()?;
        Ok(affected > 0)
    }

    pub fn purge(&self, request: &HistoryPurgeRequest) -> Result<u64, HistoryError> {
        if !request.confirm {
            return Err(HistoryError::Invalid(
                "confirm=true is required".to_string(),
            ));
        }
        let connection = open_connection(&self.path)?;
        let affected = if request.all {
            connection.execute("DELETE FROM history_requests", [])?
        } else if let Some(before) = request.before_ms {
            if let Some(status) = &request.status {
                connection.execute(
                    "DELETE FROM history_requests WHERE started_at_ms < ?1 AND status = ?2",
                    params![as_i64(before), status],
                )?
            } else {
                connection.execute(
                    "DELETE FROM history_requests WHERE started_at_ms < ?1",
                    [as_i64(before)],
                )?
            }
        } else if let Some(status) = &request.status {
            connection.execute("DELETE FROM history_requests WHERE status = ?1", [status])?
        } else {
            return Err(HistoryError::Invalid(
                "all=true, before_ms or status is required".to_string(),
            ));
        };
        let _ = connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        Ok(affected as u64)
    }

    pub fn export(&self, request: HistoryExportRequest) -> Result<HistoryExport, HistoryError> {
        let ids = if let Some(ids) = request.ids {
            ids.into_iter().take(1000).collect::<Vec<_>>()
        } else {
            self.list(request.query.unwrap_or_default())?
                .items
                .into_iter()
                .map(|item| item.id)
                .take(1000)
                .collect()
        };
        let mut records = Vec::new();
        for id in ids {
            let Some(detail) = self.detail(&id)? else {
                continue;
            };
            let mut content = Vec::new();
            for descriptor in &detail.contents {
                if let Some(section) = self.content(&id, &descriptor.kind)? {
                    content.push(section);
                }
            }
            records.push(HistoryExportRecord { detail, content });
        }
        Ok(HistoryExport {
            generated_at_ms: now_ms(),
            format: request.format,
            records,
        })
    }

    fn try_send(&self, command: HistoryCommand) -> bool {
        let Some(sender) = &self.sender else {
            return false;
        };
        match sender.try_send(command) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                warn!("request history queue is full; capture will be incomplete");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.available.store(false, Ordering::Relaxed);
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some("history writer disconnected".to_string());
                false
            }
        }
    }

    /// Queue a terminal completion; on any queue failure the record is
    /// recovered from the error and applied through the direct-writer fallback
    /// so no completion is ever silently lost.
    fn send_completion(&self, record: CompletedRecord) {
        let Some(sender) = &self.sender else {
            self.apply_completion_directly(&record.id, &record);
            return;
        };
        match sender.try_send(HistoryCommand::Complete(record)) {
            Ok(()) => {}
            Err(TrySendError::Full(command)) => {
                warn!("request history queue is full; applying completion directly");
                if let HistoryCommand::Complete(record) = command {
                    self.apply_completion_directly(&record.id, &record);
                }
            }
            Err(TrySendError::Disconnected(command)) => {
                self.available.store(false, Ordering::Relaxed);
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some("history writer disconnected".to_string());
                // The shared connection may still be writable even though the
                // writer thread is gone.
                if let HistoryCommand::Complete(record) = command {
                    self.apply_completion_directly(&record.id, &record);
                }
            }
        }
    }

    /// Apply a completion directly when the writer queue is full.
    ///
    /// The completion runs through the same mutex-guarded connection the
    /// background writer uses, so it stays serialized with queued commands.
    /// Ordering note: a fallback completion may land before older queued
    /// commands for other requests; `complete_record` tolerates a missing row
    /// (it inserts a minimal record before updating), and a queued `Start`
    /// draining after such a completion keeps the row's terminal state intact
    /// (`ON CONFLICT(id) DO NOTHING`, event not duplicated) while still storing
    /// the captured inbound payload.
    ///
    /// Blocking is bounded: at most one in-flight record transaction (WAL +
    /// synchronous=NORMAL). History failures degrade to logs and never error
    /// the inference request.
    fn apply_completion_directly(&self, request_id: &str, record: &CompletedRecord) {
        let Some(connection) = &self.writer_connection else {
            return;
        };
        let started = Instant::now();
        let cleanup_config = self.settings();
        let added_bytes = completed_record_stored_bytes(record);
        let result = match connection.try_lock() {
            Ok(guard) => complete_record(&guard, record).and_then(|_| {
                maybe_runtime_cleanup(
                    &guard,
                    &cleanup_config,
                    &self.runtime_cleanup_bytes,
                    added_bytes,
                )
            }),
            Err(_) => {
                let guard = connection
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                complete_record(&guard, record).and_then(|_| {
                    maybe_runtime_cleanup(
                        &guard,
                        &cleanup_config,
                        &self.runtime_cleanup_bytes,
                        added_bytes,
                    )
                })
            }
        };
        match result {
            Ok(()) => warn!(
                request_id = %request_id,
                waited_ms = started.elapsed().as_millis() as u64,
                "history completion applied through direct-writer fallback (queue was full)"
            ),
            Err(error) => {
                warn!(
                    request_id = %request_id,
                    error = %error,
                    "history completion direct-writer fallback failed"
                );
                self.mark_running_row_failed(request_id);
            }
        }
    }

    /// Best-effort terminal-status flip so no row is left permanently
    /// 'running' after a failed completion write.
    fn mark_running_row_failed(&self, id: &str) {
        let Some(connection) = &self.writer_connection else {
            return;
        };
        let result = connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(error) = result.execute(
            "UPDATE history_requests SET status='failed', capture_incomplete=1,
             error_type='history_write_failed',
             error_message='history completion could not be persisted',
             completed_at_ms=?2, duration_ms=MAX(0, ?2-started_at_ms)
             WHERE id=?1 AND status='running'",
            params![id, as_i64(now_ms())],
        ) {
            warn!(request_id = %id, error = %error, "history running-row failure flip also failed");
        }
    }
}

#[derive(Debug, Clone)]
pub struct HistoryCapture {
    inner: Option<Arc<HistoryCaptureInner>>,
}

impl HistoryCapture {
    fn disabled(_request_id: String) -> Self {
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
                started_at_ms: now_ms(),
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
                "route_kind": route_kind_label(route.kind),
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
            let completed = now_ms();
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
struct HistoryCaptureInner {
    store: Arc<HistoryStore>,
    config: HistoryConfig,
    request_id: String,
    started: Instant,
    completed: AtomicBool,
    draft: Mutex<CaptureDraft>,
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
fn build_completed_record(
    inner: &HistoryCaptureInner,
    status: &str,
    http_status: Option<u16>,
    finish_reason: Option<&str>,
    response_model: Option<&str>,
    error_type: Option<&str>,
    error_message: Option<&str>,
) -> CompletedRecord {
    let completed_at_ms = now_ms();
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

const HISTORY_COLUMNS: &str = "
    id, started_at_ms, completed_at_ms, duration_ms, time_to_first_chunk_ms,
    protocol, endpoint, operation_kind, client_key_id, client_name, client_environment,
    requested_model, effective_model, response_model, stream, thinking_requested,
    reasoning_effort, status, http_status, finish_reason, error_type, error_message,
    input_tokens, output_tokens, reasoning_tokens, retry_count, fallback_count,
    tool_call_count, search_count, prompt_preview, response_preview, capture_mode,
    capture_incomplete, redacted, truncated, stored_bytes, conversation_id, parent_request_id
";

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<HistoryListItem> {
    Ok(HistoryListItem {
        id: row.get(0)?,
        conversation_id: row.get(36)?,
        parent_request_id: row.get(37)?,
        started_at_ms: row.get::<_, i64>(1)?.max(0) as u64,
        completed_at_ms: optional_u64(row, 2)?,
        duration_ms: optional_u64(row, 3)?,
        time_to_first_chunk_ms: optional_u64(row, 4)?,
        protocol: row.get(5)?,
        endpoint: row.get(6)?,
        operation_kind: row.get(7)?,
        client_key_id: row.get(8)?,
        client_name: row.get(9)?,
        client_environment: row.get(10)?,
        requested_model: row.get(11)?,
        effective_model: row.get(12)?,
        response_model: row.get(13)?,
        stream: row.get::<_, i64>(14)? != 0,
        thinking_requested: row.get::<_, i64>(15)? != 0,
        reasoning_effort: row.get(16)?,
        status: row.get(17)?,
        http_status: row.get::<_, Option<i64>>(18)?.map(|value| value as u16),
        finish_reason: row.get(19)?,
        error_type: row.get(20)?,
        error_message: row.get(21)?,
        input_tokens: optional_u64(row, 22)?,
        output_tokens: optional_u64(row, 23)?,
        reasoning_tokens: optional_u64(row, 24)?,
        retry_count: row.get::<_, i64>(25)?.max(0) as u32,
        fallback_count: row.get::<_, i64>(26)?.max(0) as u32,
        tool_call_count: row.get::<_, i64>(27)?.max(0) as u32,
        search_count: row.get::<_, i64>(28)?.max(0) as u32,
        prompt_preview: row.get(29)?,
        response_preview: row.get(30)?,
        capture_mode: row.get(31)?,
        capture_incomplete: row.get::<_, i64>(32)? != 0,
        redacted: row.get::<_, i64>(33)? != 0,
        truncated: row.get::<_, i64>(34)? != 0,
        stored_bytes: row.get::<_, i64>(35)?.max(0) as u64,
    })
}

fn insert_start(
    connection: &Connection,
    start: &HistoryRequestStart,
    capture_mode: HistoryCaptureMode,
    inbound: Option<&HistoryContent>,
    prompt_preview: Option<&str>,
) -> Result<(), HistoryError> {
    let started_at_ms = now_ms();
    let transaction = connection.unchecked_transaction()?;
    let inserted = transaction.execute(
        "INSERT INTO history_requests (
            id, conversation_id, parent_request_id, started_at_ms, protocol, endpoint,
            operation_kind, client_key_id, client_name, client_environment, requested_model,
            effective_model, stream, thinking_requested, reasoning_effort,
            reasoning_budget_tokens, status, prompt_preview, capture_mode,
            capture_incomplete, redacted, truncated, stored_bytes, created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'running',?17,?18,0,?19,?20,?21,?4)
         ON CONFLICT(id) DO NOTHING",
        params![
            start.id,
            start.conversation_id,
            start.parent_request_id,
            as_i64(started_at_ms),
            start.protocol,
            start.endpoint,
            start.operation_kind,
            start.client_key_id,
            start.client_name,
            start.client_environment,
            start.requested_model,
            start.effective_model,
            i64::from(start.stream),
            i64::from(start.thinking_requested),
            start.reasoning_effort,
            start.reasoning_budget_tokens.map(i64::from),
            prompt_preview,
            capture_mode.as_str(),
            inbound.map(|content| i64::from(content.descriptor.redacted)).unwrap_or(0),
            inbound.map(|content| i64::from(content.descriptor.truncated)).unwrap_or(0),
            inbound.map(|content| content.descriptor.stored_bytes as i64).unwrap_or(0),
        ],
    )?;
    if inserted == 0 {
        // A direct-writer fallback completion outran this queued Start and
        // already created and finalized the row. Keep the terminal state
        // intact while restoring what the Start alone contributes: the
        // canonical request_received event (exactly once, in either drain
        // order) and the captured inbound payload with its bytes accounted.
        let has_received_event = transaction
            .query_row(
                "SELECT 1 FROM history_events
                 WHERE request_id = ?1 AND event_type = 'request_received' LIMIT 1",
                [&start.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !has_received_event {
            let row_started_at: i64 = transaction.query_row(
                "SELECT started_at_ms FROM history_requests WHERE id = ?1",
                [&start.id],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO history_events(request_id, sequence, timestamp_ms, event_type, severity, metadata_json)
                 VALUES (?1,0,?2,'request_received','info','{}')",
                params![start.id, row_started_at],
            )?;
        }
        if let Some(content) = inbound {
            insert_content(&transaction, &start.id, 0, content)?;
            transaction.execute(
                "UPDATE history_requests SET stored_bytes = stored_bytes + ?2 WHERE id = ?1",
                params![start.id, content.descriptor.stored_bytes as i64],
            )?;
        }
        transaction.commit()?;
        return Ok(());
    }
    if let Some(content) = inbound {
        insert_content(&transaction, &start.id, 0, content)?;
    }
    transaction.execute(
        "INSERT INTO history_events(request_id, sequence, timestamp_ms, event_type, severity, metadata_json)
         VALUES (?1,0,?2,'request_received','info','{}')",
        params![start.id, as_i64(started_at_ms)],
    )?;
    transaction.commit()?;
    Ok(())
}

fn complete_record(connection: &Connection, record: &CompletedRecord) -> Result<(), HistoryError> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO history_requests(id, started_at_ms, protocol, endpoint, operation_kind, stream,
                                      thinking_requested, status, capture_mode, created_at_ms)
         VALUES (?1,?2,'unknown','unknown','unknown',0,0,'running','redacted',?2)
         ON CONFLICT(id) DO NOTHING",
        params![record.id, as_i64(record.completed_at_ms.saturating_sub(record.duration_ms))],
    )?;
    let recovery_parent_id = transaction
        .query_row(
            "SELECT parent_request_id FROM history_requests
             WHERE id = ?1 AND operation_kind = 'response_recovery'",
            [&record.id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    transaction.execute(
        "DELETE FROM history_content WHERE request_id = ?1 AND kind != 'inbound_request'",
        [&record.id],
    )?;
    transaction.execute(
        "DELETE FROM history_attempts WHERE request_id = ?1",
        [&record.id],
    )?;
    transaction.execute(
        "DELETE FROM history_events WHERE request_id = ?1 AND sequence > 0",
        [&record.id],
    )?;

    let mut stored_bytes = transaction
        .query_row(
            "SELECT COALESCE(SUM(stored_bytes),0) FROM history_content WHERE request_id = ?1",
            [&record.id],
            |row| row.get::<_, i64>(0),
        )?
        .max(0) as u64;
    let mut response_preview = None;
    for (index, content) in record.contents.iter().enumerate() {
        insert_content(&transaction, &record.id, (index + 1) as u32, content)?;
        stored_bytes = stored_bytes.saturating_add(content.descriptor.stored_bytes as u64);
        if content.descriptor.kind == "response" {
            response_preview = Some(preview(&content.body, 220));
        }
    }
    for attempt in &record.attempts {
        transaction.execute(
            "INSERT INTO history_attempts(
                request_id, attempt_number, loop_number, attempt_kind, model, proxy_node, route_kind,
                started_at_ms, completed_at_ms, duration_ms, http_status, status, finish_reason,
                error_type, error_message, payload_sha256, payload_changed
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                record.id,
                i64::from(attempt.attempt_number),
                i64::from(attempt.loop_number),
                attempt.attempt_kind,
                attempt.model,
                attempt.proxy_node,
                attempt.route_kind.map(route_kind_label),
                as_i64(attempt.started_at_ms),
                attempt.completed_at_ms.map(as_i64),
                attempt.duration_ms.map(as_i64),
                attempt.http_status.map(i64::from),
                attempt.status,
                attempt.finish_reason,
                attempt.error_type,
                attempt.error_message,
                attempt.payload_sha256,
                i64::from(attempt.payload_changed),
            ],
        )?;
    }
    for event in &record.events {
        transaction.execute(
            "INSERT INTO history_events(request_id, sequence, timestamp_ms, event_type, severity, metadata_json)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                record.id,
                i64::from(event.sequence),
                as_i64(event.timestamp_ms),
                event.event_type,
                event.severity,
                serde_json::to_string(&event.metadata).unwrap_or_else(|_| "{}".to_string()),
            ],
        )?;
    }

    transaction.execute(
        "UPDATE history_requests SET
            completed_at_ms=?2, duration_ms=?3, time_to_first_chunk_ms=?4, status=?5,
            http_status=?6, finish_reason=?7, error_type=?8, error_message=?9,
            response_model=COALESCE(?10,response_model), input_tokens=?11, output_tokens=?12,
            reasoning_tokens=?13, retry_count=?14, fallback_count=?15, tool_call_count=?16,
            search_count=?17, response_preview=?18, capture_incomplete=?19, redacted=?20,
            truncated=?21, stored_bytes=?22
         WHERE id=?1",
        params![
            record.id,
            as_i64(record.completed_at_ms),
            as_i64(record.duration_ms),
            record.time_to_first_chunk_ms.map(as_i64),
            record.status,
            record.http_status.map(i64::from),
            record.finish_reason,
            record.error_type,
            record.error_message,
            record.response_model,
            record.input_tokens.map(as_i64),
            record.output_tokens.map(as_i64),
            record.reasoning_tokens.map(as_i64),
            i64::from(record.retry_count),
            i64::from(record.fallback_count),
            i64::from(record.tool_call_count),
            i64::from(record.search_count),
            response_preview,
            i64::from(record.capture_incomplete),
            i64::from(record.redacted),
            i64::from(record.truncated),
            as_i64(stored_bytes),
        ],
    )?;
    if let Some(parent_id) = recovery_parent_id.as_deref() {
        merge_recovery_into_parent(&transaction, parent_id, record)?;
    }
    transaction.commit()?;
    Ok(())
}

fn merge_recovery_into_parent(
    transaction: &Transaction<'_>,
    parent_id: &str,
    record: &CompletedRecord,
) -> Result<(), HistoryError> {
    if parent_id == record.id || record.status != "completed" {
        return Ok(());
    }
    let Some(response) = record
        .contents
        .iter()
        .find(|content| content.descriptor.kind == "response" && !content.body.trim().is_empty())
    else {
        return Ok(());
    };
    let parent_started_at = transaction
        .query_row(
            "SELECT started_at_ms FROM history_requests WHERE id = ?1",
            [parent_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(parent_started_at) = parent_started_at else {
        return Ok(());
    };

    let response_exists = transaction
        .query_row(
            "SELECT 1 FROM history_content WHERE request_id = ?1 AND kind = 'response' LIMIT 1",
            [parent_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    let mut added_bytes = 0_u64;
    if !response_exists {
        let sequence = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM history_content WHERE request_id = ?1",
            [parent_id],
            |row| row.get::<_, i64>(0),
        )?;
        insert_content(transaction, parent_id, sequence.max(0) as u32, response)?;
        added_bytes = response.descriptor.stored_bytes as u64;
    }

    let next_event_sequence = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM history_events WHERE request_id = ?1",
        [parent_id],
        |row| row.get::<_, i64>(0),
    )?;
    transaction.execute(
        "INSERT INTO history_events(request_id, sequence, timestamp_ms, event_type, severity, metadata_json)
         VALUES (?1,?2,?3,'response_recovered','info',?4)",
        params![
            parent_id,
            next_event_sequence,
            as_i64(record.completed_at_ms),
            serde_json::to_string(&json!({
                "child_request_id": record.id,
                "finish_reason": record.finish_reason,
                "http_status": record.http_status,
            }))
            .unwrap_or_else(|_| "{}".to_string()),
        ],
    )?;

    if let Some(attempt) = record.attempts.first() {
        let next_attempt_number = transaction.query_row(
            "SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM history_attempts WHERE request_id = ?1",
            [parent_id],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute(
            "INSERT INTO history_attempts(
                request_id, attempt_number, loop_number, attempt_kind, model, proxy_node, route_kind,
                started_at_ms, completed_at_ms, duration_ms, http_status, status, finish_reason,
                error_type, error_message, payload_sha256, payload_changed
             ) VALUES (?1,?2,0,'response_recovery',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,1)",
            params![
                parent_id,
                next_attempt_number,
                attempt.model,
                attempt.proxy_node,
                attempt.route_kind.map(route_kind_label),
                as_i64(attempt.started_at_ms),
                attempt.completed_at_ms.map(as_i64),
                attempt.duration_ms.map(as_i64),
                attempt.http_status.map(i64::from),
                attempt.status,
                attempt.finish_reason,
                attempt.error_type,
                attempt.error_message,
                attempt.payload_sha256,
            ],
        )?;
    }

    let parent_started_at = parent_started_at.max(0) as u64;
    let duration_ms = record.completed_at_ms.saturating_sub(parent_started_at);
    transaction.execute(
        "UPDATE history_requests SET
            completed_at_ms=?2, duration_ms=?3, status='completed', http_status=?4,
            finish_reason='recovered', error_type=NULL, error_message=NULL,
            response_model=COALESCE(?5,response_model), output_tokens=COALESCE(?6,output_tokens),
            fallback_count=fallback_count+1, response_preview=?7,
            stored_bytes=stored_bytes+?8
         WHERE id=?1",
        params![
            parent_id,
            as_i64(record.completed_at_ms),
            as_i64(duration_ms),
            record.http_status.map(i64::from),
            record.response_model,
            record.output_tokens.map(as_i64),
            preview(&response.body, 220),
            as_i64(added_bytes),
        ],
    )?;
    Ok(())
}

fn insert_content(
    connection: &Connection,
    request_id: &str,
    sequence: u32,
    content: &HistoryContent,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO history_content(
            request_id, sequence, kind, content_type, body, original_bytes, stored_bytes,
            sha256, redacted, truncated, created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            request_id,
            i64::from(sequence),
            content.descriptor.kind,
            content.descriptor.content_type,
            content.body,
            content.descriptor.original_bytes as i64,
            content.descriptor.stored_bytes as i64,
            content.descriptor.sha256,
            i64::from(content.descriptor.redacted),
            i64::from(content.descriptor.truncated),
            as_i64(now_ms()),
        ],
    )?;
    Ok(())
}

fn completed_record_stored_bytes(record: &CompletedRecord) -> u64 {
    record.contents.iter().fold(0_u64, |total, content| {
        total.saturating_add(content.descriptor.stored_bytes as u64)
    })
}

fn runtime_cleanup_threshold(config: &HistoryConfig) -> u64 {
    config.max_database_bytes.clamp(
        MIN_RUNTIME_CLEANUP_WATERMARK_BYTES,
        MAX_RUNTIME_CLEANUP_WATERMARK_BYTES,
    )
}

/// Run exact retention/byte-cap cleanup only after enough logical response
/// bytes accumulated to justify the database-wide scan. The writer and the
/// queue-full direct fallback share both the connection mutex and this
/// watermark, so there is one serialized cleanup cadence for the process.
fn maybe_runtime_cleanup(
    connection: &Connection,
    config: &HistoryConfig,
    watermark: &AtomicU64,
    added_bytes: u64,
) -> Result<(), HistoryError> {
    let pending = watermark
        .fetch_add(added_bytes, Ordering::AcqRel)
        .saturating_add(added_bytes);
    if pending < runtime_cleanup_threshold(config) {
        return Ok(());
    }

    let carried = watermark.swap(0, Ordering::AcqRel);
    if let Err(error) = cleanup(connection, config) {
        // Preserve pressure after a transient SQLite failure so a later
        // completion retries cleanup instead of forgetting the accumulated
        // bytes forever.
        let _ = watermark.fetch_add(carried, Ordering::AcqRel);
        return Err(error);
    }
    Ok(())
}

fn cleanup(connection: &Connection, config: &HistoryConfig) -> Result<(), HistoryError> {
    if config.retention_days > 0 {
        let cutoff = now_ms().saturating_sub(u64::from(config.retention_days) * 86_400_000);
        connection.execute(
            "DELETE FROM history_requests WHERE started_at_ms < ?1",
            [as_i64(cutoff)],
        )?;
    }
    connection.execute(
        "DELETE FROM history_requests WHERE id IN (
            SELECT id FROM history_requests ORDER BY started_at_ms DESC LIMIT -1 OFFSET ?1
         )",
        [config.max_records as i64],
    )?;
    let logical: u64 = connection
        .query_row(
            "SELECT COALESCE(SUM(stored_bytes),0) FROM history_requests",
            [],
            |row| row.get::<_, i64>(0),
        )?
        .max(0) as u64;
    // Single-pass byte-cap eviction: delete the oldest records while the bytes
    // before them account for less than the excess, which is exactly the
    // boundary the previous delete-one/re-SUM loop reached — without rescanning
    // the table after every deleted row.
    let excess = logical.saturating_sub(config.max_database_bytes);
    if excess > 0 {
        connection.execute(
            "DELETE FROM history_requests WHERE id IN (
                SELECT id FROM (
                    SELECT id,
                           COALESCE(SUM(stored_bytes) OVER (
                               ORDER BY started_at_ms ASC, id ASC
                               ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
                           ), 0) AS prior_bytes
                    FROM history_requests
                ) WHERE prior_bytes < ?1
             )",
            [as_i64(excess)],
        )?;
    }
    let _ = connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

fn prepare_database(path: &Path, config: &mut HistoryConfig) -> Result<(), HistoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_private_permissions(parent, true)?;
    }
    let connection = open_connection(path)?;
    initialize_schema(&connection)?;
    connection.execute(
        "UPDATE history_requests SET status='interrupted', capture_incomplete=1,
         error_type='process_interrupted', error_message='bridge stopped before capture finalized',
         completed_at_ms=?1, duration_ms=MAX(0, ?1-started_at_ms)
         WHERE status='running'",
        [as_i64(now_ms())],
    )?;
    let _overridden = load_persisted_settings(&connection, config)?;
    cleanup(&connection, config)?;
    drop(connection);
    set_private_permissions(path, false)?;
    Ok(())
}

fn open_connection(path: &Path) -> Result<Connection, HistoryError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA secure_delete=FAST;",
    )?;
    initialize_schema(&connection)?;
    Ok(connection)
}

fn initialize_schema(connection: &Connection) -> Result<(), HistoryError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS history_requests (
            id TEXT PRIMARY KEY,
            conversation_id TEXT,
            parent_request_id TEXT,
            started_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER,
            duration_ms INTEGER,
            time_to_first_chunk_ms INTEGER,
            protocol TEXT NOT NULL,
            endpoint TEXT NOT NULL,
            operation_kind TEXT NOT NULL,
            client_key_id TEXT,
            client_name TEXT,
            client_environment TEXT,
            requested_model TEXT,
            effective_model TEXT,
            response_model TEXT,
            stream INTEGER NOT NULL,
            thinking_requested INTEGER NOT NULL,
            reasoning_effort TEXT,
            reasoning_budget_tokens INTEGER,
            status TEXT NOT NULL,
            http_status INTEGER,
            finish_reason TEXT,
            error_type TEXT,
            error_message TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            reasoning_tokens INTEGER,
            retry_count INTEGER NOT NULL DEFAULT 0,
            fallback_count INTEGER NOT NULL DEFAULT 0,
            tool_call_count INTEGER NOT NULL DEFAULT 0,
            search_count INTEGER NOT NULL DEFAULT 0,
            prompt_preview TEXT,
            response_preview TEXT,
            capture_mode TEXT NOT NULL,
            capture_incomplete INTEGER NOT NULL DEFAULT 0,
            redacted INTEGER NOT NULL DEFAULT 0,
            truncated INTEGER NOT NULL DEFAULT 0,
            stored_bytes INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS history_content (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            request_id TEXT NOT NULL,
            attempt_id INTEGER,
            sequence INTEGER NOT NULL,
            kind TEXT NOT NULL,
            content_type TEXT NOT NULL,
            body TEXT NOT NULL,
            original_bytes INTEGER NOT NULL,
            stored_bytes INTEGER NOT NULL,
            sha256 TEXT NOT NULL,
            redacted INTEGER NOT NULL DEFAULT 0,
            truncated INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            FOREIGN KEY(request_id) REFERENCES history_requests(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS history_attempts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            request_id TEXT NOT NULL,
            attempt_number INTEGER NOT NULL,
            loop_number INTEGER NOT NULL DEFAULT 0,
            attempt_kind TEXT NOT NULL,
            model TEXT,
            proxy_node TEXT,
            route_kind TEXT,
            started_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER,
            duration_ms INTEGER,
            http_status INTEGER,
            status TEXT NOT NULL,
            finish_reason TEXT,
            error_type TEXT,
            error_message TEXT,
            payload_sha256 TEXT,
            payload_changed INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(request_id) REFERENCES history_requests(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS history_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            request_id TEXT NOT NULL,
            attempt_id INTEGER,
            sequence INTEGER NOT NULL,
            timestamp_ms INTEGER NOT NULL,
            event_type TEXT NOT NULL,
            severity TEXT NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            content_id INTEGER,
            FOREIGN KEY(request_id) REFERENCES history_requests(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS history_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_history_started ON history_requests(started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_status ON history_requests(status, started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_model ON history_requests(effective_model, started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_client ON history_requests(client_key_id, started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_endpoint ON history_requests(endpoint, started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_conversation ON history_requests(conversation_id, started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_parent ON history_requests(parent_request_id, started_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_history_content_request ON history_content(request_id, kind);
         CREATE INDEX IF NOT EXISTS idx_history_attempt_request ON history_attempts(request_id, attempt_number);
         CREATE INDEX IF NOT EXISTS idx_history_event_request ON history_events(request_id, sequence);",
    )?;
    ensure_history_attempt_route_kind_column(connection)?;
    Ok(())
}

fn ensure_history_attempt_route_kind_column(connection: &Connection) -> Result<(), HistoryError> {
    let mut statement = connection.prepare("PRAGMA table_info(history_attempts)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "route_kind") {
        connection.execute(
            "ALTER TABLE history_attempts ADD COLUMN route_kind TEXT",
            [],
        )?;
    }
    // Writing user_version forces a header write transaction; read paths open
    // a fresh connection per call, so only pay that cost when actually
    // migrating an older database instead of on every connection open.
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 2 {
        connection.pragma_update(None, "user_version", 2_i64)?;
    }
    Ok(())
}

fn route_kind_label(kind: RouteKind) -> &'static str {
    match kind {
        RouteKind::Direct => "direct",
        RouteKind::Proxy => "proxy",
        RouteKind::Standby => "standby",
        RouteKind::DirectHybridFallback => "direct-hybrid-fallback",
    }
}

fn parse_route_kind(value: &str) -> Option<RouteKind> {
    match value {
        "direct" => Some(RouteKind::Direct),
        "proxy" => Some(RouteKind::Proxy),
        "standby" => Some(RouteKind::Standby),
        "direct-hybrid-fallback" => Some(RouteKind::DirectHybridFallback),
        _ => None,
    }
}

fn load_persisted_settings(
    connection: &Connection,
    config: &mut HistoryConfig,
) -> Result<Vec<String>, HistoryError> {
    let mut statement = connection.prepare("SELECT key, value FROM history_settings")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut overridden = Vec::new();
    for row in rows {
        let (key, value) = row?;
        if apply_persisted_setting(config, &key, &value) {
            overridden.push(format!("{key}={value}"));
        }
    }
    if !overridden.is_empty() {
        // Provenance only: persisted dashboard settings intentionally keep
        // their override precedence over TOML/env; this line makes the split
        // visible to operators instead of silently shadowing startup config.
        info!(
            overrides = %overridden.join(", "),
            "history settings restored from database; these keys shadow the TOML/env startup configuration"
        );
    }
    Ok(overridden)
}

/// Applies one persisted setting to `config`; returns true when the key was
/// recognized and overrode the loaded value.
fn apply_persisted_setting(config: &mut HistoryConfig, key: &str, value: &str) -> bool {
    match key {
        "enabled" => match value.parse::<bool>() {
            Ok(enabled) => {
                config.enabled = enabled;
                true
            }
            Err(_) => false,
        },
        "capture_mode" => match HistoryCaptureMode::parse(value) {
            Some(mode) => {
                config.capture_mode = mode;
                true
            }
            None => false,
        },
        "retention_days" => match value.parse::<u32>() {
            Ok(days) => {
                config.retention_days = days.min(3650);
                true
            }
            Err(_) => false,
        },
        "max_records" => match value.parse::<usize>() {
            Ok(records) => {
                config.max_records = records.clamp(1, 1_000_000);
                true
            }
            Err(_) => false,
        },
        "max_database_bytes" => match value.parse::<u64>() {
            Ok(bytes) => {
                config.max_database_bytes = bytes.max(1024 * 1024);
                true
            }
            Err(_) => false,
        },
        _ => false,
    }
}

fn persist_settings(connection: &Connection, config: &HistoryConfig) -> Result<(), HistoryError> {
    let transaction = connection.unchecked_transaction()?;
    let updated_at = as_i64(now_ms());
    for (key, value) in [
        ("enabled", config.enabled.to_string()),
        ("capture_mode", config.capture_mode.to_string()),
        ("retention_days", config.retention_days.to_string()),
        ("max_records", config.max_records.to_string()),
        ("max_database_bytes", config.max_database_bytes.to_string()),
    ] {
        transaction.execute(
            "INSERT INTO history_settings(key, value, updated_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at_ms=excluded.updated_at_ms",
            params![key, value, updated_at],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> std::io::Result<()> {
    Ok(())
}

fn lock_draft(inner: &HistoryCaptureInner) -> std::sync::MutexGuard<'_, CaptureDraft> {
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
        timestamp_ms: now_ms(),
        event_type: event_type.to_string(),
        severity: severity.to_string(),
        metadata,
    });
}

fn optional_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    Ok(row
        .get::<_, Option<i64>>(index)?
        .map(|value| value.max(0) as u64))
}

fn as_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_pool::{RouteKind, RouteMetadata};
    use std::time::Duration;

    fn test_config(path: PathBuf) -> HistoryConfig {
        HistoryConfig {
            enabled: true,
            path: Some(path),
            ..crate::config::BridgeConfig::default().history
        }
    }

    #[test]
    fn attempt_route_round_trips_route_kind_and_proxy_node() {
        let root = std::env::temp_dir().join(format!("oc2-history-route-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let store = HistoryStore::open(test_config(path.clone()), root.join("fallback.sqlite3"));
        let capture = store.begin(HistoryRequestStart {
            id: "req-route".to_string(),
            conversation_id: None,
            parent_request_id: None,
            protocol: "anthropic".to_string(),
            endpoint: "/v1/messages".to_string(),
            operation_kind: "messages".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Client".to_string()),
            client_environment: Some("test".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: false,
            thinking_requested: false,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
            inbound: Some(json!({"messages":[{"role":"user","content":"test"}]})),
        });
        capture.effective_json(
            &json!({"model":"test-model","messages":[]}),
            Some("test-model"),
            "primary",
            1,
        );
        capture.attempt_route(&RouteMetadata {
            kind: RouteKind::Standby,
            proxy_node: Some("opencode-warp-4".to_string()),
        });
        capture.attempt_finished(Some(200), "completed", Some("stop"), None, None);
        capture.finish_success(200, Some("stop"), Some("test-model"));
        std::thread::sleep(Duration::from_millis(120));
        drop(store);

        let reopened = HistoryStore::open(test_config(path), root.join("fallback-2.sqlite3"));
        let detail = reopened.detail("req-route").unwrap().unwrap();
        assert_eq!(detail.attempts.len(), 1);
        assert_eq!(detail.attempts[0].route_kind, Some(RouteKind::Standby));
        assert_eq!(
            detail.attempts[0].proxy_node.as_deref(),
            Some("opencode-warp-4")
        );
        drop(reopened);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn history_route_kind_migration_is_idempotent_for_existing_database() {
        let root = std::env::temp_dir().join(format!("oc2-history-migrate-{}", now_ms()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("history.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE history_attempts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    request_id TEXT NOT NULL,
                    attempt_number INTEGER NOT NULL,
                    loop_number INTEGER NOT NULL DEFAULT 0,
                    attempt_kind TEXT NOT NULL,
                    model TEXT,
                    proxy_node TEXT,
                    started_at_ms INTEGER NOT NULL,
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    http_status INTEGER,
                    status TEXT NOT NULL,
                    finish_reason TEXT,
                    error_type TEXT,
                    error_message TEXT,
                    payload_sha256 TEXT,
                    payload_changed INTEGER NOT NULL DEFAULT 0
                );
                PRAGMA user_version=1;",
            )
            .unwrap();

        initialize_schema(&connection).unwrap();
        initialize_schema(&connection).unwrap();

        let mut statement = connection
            .prepare("PRAGMA table_info(history_attempts)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns
                .iter()
                .filter(|column| *column == "route_kind")
                .count(),
            1
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(statement);
        drop(connection);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dashboard_history_settings_persist_across_store_reopen() {
        let root = std::env::temp_dir().join(format!("oc2-history-settings-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let store = HistoryStore::open(test_config(path.clone()), root.join("fallback.sqlite3"));
        let updated = store
            .update_settings(HistorySettingsPatch {
                enabled: Some(false),
                capture_mode: Some(HistoryCaptureMode::Metadata),
                retention_days: Some(7),
                max_records: Some(321),
                max_database_bytes: Some(64 * 1024 * 1024),
            })
            .unwrap();
        assert!(!updated.enabled);
        drop(store);

        let reopened = HistoryStore::open(test_config(path), root.join("fallback-2.sqlite3"));
        let settings = reopened.settings_view();
        assert!(!settings.enabled);
        assert_eq!(settings.capture_mode, HistoryCaptureMode::Metadata);
        assert_eq!(settings.retention_days, 7);
        assert_eq!(settings.max_records, 321);
        assert_eq!(settings.max_database_bytes, 64 * 1024 * 1024);
        drop(reopened);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stores_redacted_request_reasoning_and_response() {
        let root = std::env::temp_dir().join(format!("oc2-history-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let store = HistoryStore::open(test_config(path), root.join("fallback.sqlite3"));
        let capture = store.begin(HistoryRequestStart {
            id: "req-test".to_string(),
            conversation_id: None,
            parent_request_id: None,
            protocol: "anthropic".to_string(),
            endpoint: "/v1/messages".to_string(),
            operation_kind: "messages".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Client".to_string()),
            client_environment: Some("test".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: true,
            thinking_requested: true,
            reasoning_effort: Some("high".to_string()),
            reasoning_budget_tokens: None,
            inbound: Some(json!({"messages":[{"content":"Bearer abcdefghijklmnop"}]})),
        });
        capture.append_reasoning("private reasoning");
        capture.append_response("visible answer");
        capture.finish_success(200, Some("end_turn"), Some("test-model"));
        std::thread::sleep(Duration::from_millis(100));
        let detail = store.detail("req-test").unwrap().unwrap();
        assert_eq!(detail.request.status, "completed");
        let inbound = store
            .content("req-test", "inbound_request")
            .unwrap()
            .unwrap();
        assert!(!inbound.body.contains("abcdefghijklmnop"));
        assert!(store.content("req-test", "reasoning").unwrap().is_some());
        assert!(store.content("req-test", "response").unwrap().is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn response_recovery_is_merged_into_parent_and_hidden_from_default_list() {
        let root = std::env::temp_dir().join(format!("oc2-history-recovery-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let store = HistoryStore::open(test_config(path), root.join("fallback.sqlite3"));

        let parent = store.begin(HistoryRequestStart {
            id: "req-parent".to_string(),
            conversation_id: Some("tester-conversation".to_string()),
            parent_request_id: None,
            protocol: "openai".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            operation_kind: "chat_completions".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Dashboard tester".to_string()),
            client_environment: Some("local".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: true,
            thinking_requested: true,
            reasoning_effort: Some("max".to_string()),
            reasoning_budget_tokens: None,
            inbound: Some(json!({"messages":[{"role":"user","content":"test"}]})),
        });
        parent.append_reasoning("captured reasoning");
        parent.cancel();

        let recovery = store.begin(HistoryRequestStart {
            id: "req-child".to_string(),
            conversation_id: Some("tester-conversation".to_string()),
            parent_request_id: Some("req-parent".to_string()),
            protocol: "openai".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            operation_kind: "response_recovery".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Dashboard tester".to_string()),
            client_environment: Some("local".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: false,
            thinking_requested: false,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
            inbound: Some(json!({"messages":[{"role":"user","content":"test"}]})),
        });
        recovery.effective_json(
            &json!({"model":"test-model","messages":[{"role":"user","content":"test"}]}),
            Some("test-model"),
            "response_recovery",
            0,
        );
        recovery.append_response("recovered final answer");
        recovery.attempt_finished(Some(200), "completed", Some("stop"), None, None);
        recovery.usage(Some(5), Some(4), None);
        recovery.finish_success(200, Some("stop"), Some("test-model"));

        std::thread::sleep(Duration::from_millis(150));
        let parent_detail = store.detail("req-parent").unwrap().unwrap();
        assert_eq!(parent_detail.request.status, "completed");
        assert_eq!(
            parent_detail.request.finish_reason.as_deref(),
            Some("recovered")
        );
        assert_eq!(
            parent_detail.request.conversation_id.as_deref(),
            Some("tester-conversation")
        );
        assert!(parent_detail.request.parent_request_id.is_none());
        assert_eq!(parent_detail.request.fallback_count, 1);
        assert_eq!(
            store
                .content("req-parent", "reasoning")
                .unwrap()
                .unwrap()
                .body,
            "captured reasoning"
        );
        assert_eq!(
            store
                .content("req-parent", "response")
                .unwrap()
                .unwrap()
                .body,
            "recovered final answer"
        );
        assert!(parent_detail
            .attempts
            .iter()
            .any(|attempt| attempt.attempt_kind == "response_recovery"));
        assert!(parent_detail
            .events
            .iter()
            .any(|event| event.event_type == "response_recovered"));

        let child_detail = store.detail("req-child").unwrap().unwrap();
        assert_eq!(child_detail.request.operation_kind, "response_recovery");
        assert_eq!(
            child_detail.request.parent_request_id.as_deref(),
            Some("req-parent")
        );
        let page = store.list(HistoryQuery::default()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, "req-parent");
        assert_eq!(store.stats().unwrap().total, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn queue_full_completion_falls_back_to_direct_writer() {
        let root = std::env::temp_dir().join(format!("oc2-history-qfull-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let mut config = test_config(path.clone());
        config.queue_capacity = 1;
        let store = HistoryStore::open(config, root.join("fallback.sqlite3"));
        let connection = Arc::clone(store.writer_connection.as_ref().unwrap());
        let gate = Arc::new(std::sync::Barrier::new(2));
        let holder_gate = Arc::clone(&gate);
        let holder = std::thread::spawn(move || {
            // Block the writer thread so the single queue slot stays full.
            let guard = connection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            holder_gate.wait();
            std::thread::sleep(Duration::from_millis(200));
            drop(guard);
        });
        gate.wait();

        // Start fills the only queue slot; the completion cannot enqueue and
        // must land through the direct-writer fallback.
        let capture = store.begin(HistoryRequestStart {
            id: "req-qfull".to_string(),
            conversation_id: None,
            parent_request_id: None,
            protocol: "anthropic".to_string(),
            endpoint: "/v1/messages".to_string(),
            operation_kind: "messages".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Client".to_string()),
            client_environment: Some("test".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: false,
            thinking_requested: false,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
            inbound: Some(json!({"messages":[{"role":"user","content":"test"}]})),
        });
        capture.finish_success(200, Some("stop"), Some("test-model"));
        let _ = holder.join();
        std::thread::sleep(Duration::from_millis(250)); // let the queued Start drain

        let detail = store.detail("req-qfull").unwrap().unwrap();
        assert_eq!(detail.request.status, "completed");
        assert_eq!(detail.request.finish_reason.as_deref(), Some("stop"));
        let stats = store.stats().unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.success, 1);
        assert_eq!(stats.failed, 0);
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_enabled_requires_strict_boolean() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_schema(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO history_settings(key, value, updated_at_ms)
                 VALUES ('enabled', 'True', 0)",
                [],
            )
            .unwrap();
        let mut config = crate::config::BridgeConfig::default().history;
        config.enabled = true;
        let overridden = load_persisted_settings(&connection, &mut config).unwrap();
        // A non-canonical boolean must be rejected like every other malformed
        // key: no override reported, startup value preserved.
        assert!(overridden.is_empty());
        assert!(config.enabled);
    }

    #[test]
    fn late_queued_start_does_not_duplicate_events_or_stored_bytes() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_schema(&connection).unwrap();

        // Direct-writer fallback landed the completion first: the row exists
        // in terminal state before the queued Start drained.
        let record = CompletedRecord {
            id: "req-late".to_string(),
            completed_at_ms: now_ms(),
            duration_ms: 50,
            time_to_first_chunk_ms: Some(5),
            status: "completed".to_string(),
            http_status: Some(200),
            finish_reason: Some("stop".to_string()),
            error_type: None,
            error_message: None,
            response_model: Some("test-model".to_string()),
            input_tokens: Some(1),
            output_tokens: Some(2),
            reasoning_tokens: None,
            retry_count: 0,
            fallback_count: 0,
            tool_call_count: 0,
            search_count: 0,
            capture_incomplete: false,
            redacted: false,
            truncated: false,
            contents: Vec::new(),
            attempts: Vec::new(),
            events: Vec::new(),
        };
        complete_record(&connection, &record).unwrap();

        let inbound = HistoryContent {
            descriptor: HistoryContentDescriptor {
                kind: "inbound_request".to_string(),
                content_type: "application/json".to_string(),
                original_bytes: 44,
                stored_bytes: 44,
                sha256: "deadbeef".to_string(),
                redacted: false,
                truncated: false,
            },
            body: "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}".to_string(),
        };
        let start = HistoryRequestStart {
            id: "req-late".to_string(),
            conversation_id: None,
            parent_request_id: None,
            protocol: "anthropic".to_string(),
            endpoint: "/v1/messages".to_string(),
            operation_kind: "messages".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Client".to_string()),
            client_environment: Some("test".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: false,
            thinking_requested: false,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
            inbound: Some(json!({"messages":[{"role":"user","content":"hi"}]})),
        };
        insert_start(
            &connection,
            &start,
            HistoryCaptureMode::Redacted,
            Some(&inbound),
            Some("hi"),
        )
        .unwrap();

        let received: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history_events
                 WHERE request_id = 'req-late' AND event_type = 'request_received'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(received, 1);
        let total_events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history_events WHERE request_id = 'req-late'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total_events, 1);
        let stored: i64 = connection
            .query_row(
                "SELECT stored_bytes FROM history_requests WHERE id = 'req-late'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, 44);
        let status: String = connection
            .query_row(
                "SELECT status FROM history_requests WHERE id = 'req-late'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[test]
    fn byte_cap_eviction_deletes_exact_oldest_prefix() {
        let root = std::env::temp_dir().join(format!("oc2-history-bytecap-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let mut config = test_config(path.clone());
        config.retention_days = 0;
        config.max_records = 100;
        config.max_database_bytes = 1000;
        let store = HistoryStore::open(config.clone(), root.join("fallback.sqlite3"));
        {
            let connection = store
                .writer_connection
                .as_ref()
                .unwrap()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let base = now_ms().saturating_sub(100_000);
            for (index, (id, size)) in [
                ("r1", 100_i64),
                ("r2", 200),
                ("r3", 300),
                ("r4", 400),
                ("r5", 500),
            ]
            .into_iter()
            .enumerate()
            {
                connection
                    .execute(
                        "INSERT INTO history_requests(
                            id, started_at_ms, protocol, endpoint, operation_kind, stream,
                            thinking_requested, status, capture_mode, stored_bytes, created_at_ms
                         ) VALUES (?1,?2,'anthropic','/v1/messages','messages',0,0,'completed','metadata',?3,?2)",
                        params![id, as_i64(base + index as u64 * 1000), size],
                    )
                    .unwrap();
            }

            // Total 1500 bytes vs cap 1000: the old loop deleted r1, r2, r3 and
            // stopped; the window pass must delete exactly that prefix.
            cleanup(&connection, &config).unwrap();
            let remaining: Vec<String> = connection
                .prepare("SELECT id FROM history_requests ORDER BY started_at_ms ASC")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(remaining, vec!["r4".to_string(), "r5".to_string()]);
            assert_eq!(store.stats().unwrap().stored_bytes, 900);

            // Edge: excess exceeds every prefix — everything is evicted, and
            // the pass terminates instead of looping on an empty table.
            let mut wipe_config = config.clone();
            wipe_config.max_database_bytes = 10;
            cleanup(&connection, &wipe_config).unwrap();
            let count: i64 = connection
                .query_row("SELECT COUNT(*) FROM history_requests", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_settings_override_enumeration_reports_only_applied_keys() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_schema(&connection).unwrap();
        for (key, value) in [
            ("enabled", "false"),
            ("bogus_key", "ignored"),
            ("retention_days", "notanumber"),
            ("max_records", "321"),
        ] {
            connection
                .execute(
                    "INSERT INTO history_settings(key, value, updated_at_ms) VALUES (?1, ?2, 0)",
                    params![key, value],
                )
                .unwrap();
        }
        let mut config = crate::config::BridgeConfig::default().history;
        let before = config.clone();
        let overridden = load_persisted_settings(&connection, &mut config).unwrap();
        let mut reported = overridden.clone();
        reported.sort();
        assert_eq!(
            reported,
            vec!["enabled=false".to_string(), "max_records=321".to_string()]
        );
        assert!(!config.enabled);
        assert_eq!(config.max_records, 321);
        assert_eq!(config.retention_days, before.retention_days);
    }

    /// The byte cap must be enforced during long runtime, not only at store
    /// open and settings change: completions streamed through the live writer
    /// loop have to trigger eviction once logical bytes pass the configured
    /// cap, without any restart or settings write.
    #[test]
    fn runtime_completions_trigger_byte_cap_trim_without_restart() {
        let root = std::env::temp_dir().join(format!("oc2-history-rtrim-live-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let mut config = test_config(path.clone());
        config.retention_days = 0;
        config.max_records = 10_000;
        config.max_database_bytes = 512 * 1024;
        let store = HistoryStore::open(config.clone(), root.join("fallback.sqlite3"));

        let payload = "x".repeat(256 * 1024);
        let total = 8usize;
        for index in 0..total {
            let capture = store.begin(HistoryRequestStart {
                id: format!("req-live-{index}"),
                conversation_id: None,
                parent_request_id: None,
                protocol: "anthropic".to_string(),
                endpoint: "/v1/messages".to_string(),
                operation_kind: "messages".to_string(),
                client_key_id: Some("client".to_string()),
                client_name: Some("Client".to_string()),
                client_environment: Some("test".to_string()),
                requested_model: Some("test-model".to_string()),
                effective_model: Some("test-model".to_string()),
                stream: false,
                thinking_requested: false,
                reasoning_effort: None,
                reasoning_budget_tokens: None,
                inbound: None,
            });
            capture.append_response(&payload);
            capture.finish_success(200, Some("stop"), Some("test-model"));
        }

        // Every completion drains asynchronously; the runtime trim runs inside
        // the same writer loop, so once all rows are terminal and committed the
        // trim for the final watermark crossing follows within one iteration.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let stats = store.stats().unwrap();
            let newest_committed = store
                .detail(&format!("req-live-{}", total - 1))
                .unwrap()
                .is_some();
            if newest_committed && stats.stored_bytes <= config.max_database_bytes {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "runtime byte-cap trim did not converge: total={} stored={} newest_committed={}",
                stats.total,
                stats.stored_bytes,
                newest_committed
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            store.detail("req-live-0").unwrap().is_none(),
            "oldest records must be evicted once the cap is exceeded at runtime"
        );
        assert!(
            store
                .detail(&format!("req-live-{}", total - 1))
                .unwrap()
                .is_some(),
            "the newest record must survive eviction"
        );
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    /// Normal operation below the check watermark must never evict anything:
    /// small runtime growth on a store that is under its cap keeps every row.
    #[test]
    fn runtime_growth_below_watermark_keeps_existing_records() {
        let root = std::env::temp_dir().join(format!("oc2-history-rtrim-calm-{}", now_ms()));
        let path = root.join("history.sqlite3");
        let mut config = test_config(path.clone());
        config.retention_days = 0;
        config.max_records = 10_000;
        config.max_database_bytes = 64 * 1024;
        let store = HistoryStore::open(config, root.join("fallback.sqlite3"));
        {
            let connection = store
                .writer_connection
                .as_ref()
                .unwrap()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let base = now_ms().saturating_sub(100_000);
            for (index, (id, size)) in [("s1", 100_i64), ("s2", 200), ("s3", 300)]
                .into_iter()
                .enumerate()
            {
                connection
                    .execute(
                        "INSERT INTO history_requests(
                            id, started_at_ms, protocol, endpoint, operation_kind, stream,
                            thinking_requested, status, capture_mode, stored_bytes, created_at_ms
                         ) VALUES (?1,?2,'anthropic','/v1/messages','messages',0,0,'completed','metadata',?3,?2)",
                        params![id, as_i64(base + index as u64 * 1000), size],
                    )
                    .unwrap();
            }
        }

        let capture = store.begin(HistoryRequestStart {
            id: "req-small".to_string(),
            conversation_id: None,
            parent_request_id: None,
            protocol: "anthropic".to_string(),
            endpoint: "/v1/messages".to_string(),
            operation_kind: "messages".to_string(),
            client_key_id: Some("client".to_string()),
            client_name: Some("Client".to_string()),
            client_environment: Some("test".to_string()),
            requested_model: Some("test-model".to_string()),
            effective_model: Some("test-model".to_string()),
            stream: false,
            thinking_requested: false,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
            inbound: None,
        });
        capture.append_response("ok");
        capture.finish_success(200, Some("stop"), Some("test-model"));

        let deadline = Instant::now() + Duration::from_secs(5);
        while store.stats().unwrap().total < 4 {
            assert!(Instant::now() < deadline, "completion never drained");
            std::thread::sleep(Duration::from_millis(25));
        }
        for id in ["s1", "s2", "s3"] {
            assert!(
                store.detail(id).unwrap().is_some(),
                "record {id} must survive when growth stays below the check watermark"
            );
        }
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
}
