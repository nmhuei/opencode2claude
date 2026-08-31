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
