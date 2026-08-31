use super::types::*;
use crate::config::{HistoryCaptureMode, HistoryConfig};
use crate::history::redact::preview;
use crate::history::types::*;
use crate::proxy_pool::RouteKind;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

pub(crate) const HISTORY_COLUMNS: &str = "
    id, started_at_ms, completed_at_ms, duration_ms, time_to_first_chunk_ms,
    protocol, endpoint, operation_kind, client_key_id, client_name, client_environment,
    requested_model, effective_model, response_model, stream, thinking_requested,
    reasoning_effort, status, http_status, finish_reason, error_type, error_message,
    input_tokens, output_tokens, reasoning_tokens, retry_count, fallback_count,
    tool_call_count, search_count, prompt_preview, response_preview, capture_mode,
    capture_incomplete, redacted, truncated, stored_bytes, conversation_id, parent_request_id
";

pub(crate) fn row_to_item(row: &Row<'_>) -> rusqlite::Result<HistoryListItem> {
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

pub(crate) fn insert_start(
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

pub(crate) fn complete_record(
    connection: &Connection,
    record: &CompletedRecord,
) -> Result<(), HistoryError> {
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

pub(crate) fn merge_recovery_into_parent(
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

pub(crate) fn insert_content(
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

pub(crate) fn completed_record_stored_bytes(record: &CompletedRecord) -> u64 {
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
pub(crate) fn maybe_runtime_cleanup(
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

pub(crate) fn cleanup(connection: &Connection, config: &HistoryConfig) -> Result<(), HistoryError> {
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

pub(crate) fn prepare_database(
    path: &Path,
    config: &mut HistoryConfig,
) -> Result<(), HistoryError> {
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

pub(crate) fn open_connection(path: &Path) -> Result<Connection, HistoryError> {
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

pub(crate) fn initialize_schema(connection: &Connection) -> Result<(), HistoryError> {
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

pub(crate) fn route_kind_label(kind: RouteKind) -> &'static str {
    match kind {
        RouteKind::Direct => "direct",
        RouteKind::Proxy => "proxy",
        RouteKind::Standby => "standby",
        RouteKind::DirectHybridFallback => "direct-hybrid-fallback",
    }
}

pub(crate) fn parse_route_kind(value: &str) -> Option<RouteKind> {
    match value {
        "direct" => Some(RouteKind::Direct),
        "proxy" => Some(RouteKind::Proxy),
        "standby" => Some(RouteKind::Standby),
        "direct-hybrid-fallback" => Some(RouteKind::DirectHybridFallback),
        _ => None,
    }
}

pub(crate) fn load_persisted_settings(
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

pub(crate) fn persist_settings(
    connection: &Connection,
    config: &HistoryConfig,
) -> Result<(), HistoryError> {
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

pub(crate) fn optional_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    Ok(row
        .get::<_, Option<i64>>(index)?
        .map(|value| value.max(0) as u64))
}

pub(crate) fn as_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
