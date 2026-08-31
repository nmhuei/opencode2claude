mod capture;
mod sql;
#[cfg(test)]
mod tests;
mod types;

use crate::config::{HistoryCaptureMode, HistoryConfig};
use crate::history::redact::{as_content, capture_json, preview};
use crate::history::types::{
    HistoryAttempt, HistoryContent, HistoryContentDescriptor, HistoryDetail, HistoryEvent,
    HistoryExport, HistoryExportRecord, HistoryExportRequest, HistoryPage, HistoryPurgeRequest,
    HistoryQuery, HistoryRequestStart, HistorySettingsPatch, HistorySettingsView, HistoryStats,
    HistoryStorageStatus,
};
use capture::HistoryCaptureInner;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use sql::{
    as_i64, cleanup, complete_record, completed_record_stored_bytes, insert_start,
    maybe_runtime_cleanup, open_connection, optional_u64, parse_route_kind, persist_settings,
    prepare_database, row_to_item, HISTORY_COLUMNS,
};
#[cfg(test)]
use sql::{initialize_schema, load_persisted_settings};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tracing::warn;
use types::{CaptureDraft, CompletedRecord, HistoryCommand};

pub use capture::HistoryCapture;
pub use sql::now_ms;
pub use types::HistoryError;

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
