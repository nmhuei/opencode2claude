//! Typed configuration preview, atomic apply, and rollback workflow.

use super::dto::{ConfigApplyResponse, ConfigPreviewResponse};
use super::service::ManagementError;
use crate::config::{migration, TomlConfig};
use crate::state::AppState;
use axum::http::StatusCode;
use std::collections::BTreeSet;

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const KNOWN_ROOT_KEYS: &[&str] = &[
    "schema_version",
    "port",
    "host",
    "opencode_port",
    "model",
    "shell_policy",
    "shell_allowlist",
    "auth_tokens",
    "max_body_size",
    "stream_buffer_size",
    "channel_capacity",
    "tavily_api_key",
    "exa_api_key",
    "serper_api_key",
    "searxng_url",
    "searxng_api_key",
    "max_search_loops",
    "search_max_results",
    "search_max_snippet_chars",
    "search_max_response_bytes",
    "search_timeout_secs",
    "allow_private_searxng",
    "tavily_url",
    "exa_url",
    "serper_url",
    "duckduckgo_url",
    "proxies",
    "primary_proxies",
    "warm_standby_proxies",
    "dashboard_admin_token",
    "rest_api_token",
    "csrf_enabled",
    "rate_limit",
    "min_reasoning_stream_tokens",
    "max_sse_line_bytes",
    "max_sync_response_bytes",
    "upstream_base_url",
    "model_fallbacks",
    "enable_default_fallbacks",
    "max_network_attempts",
    "max_provider_attempts",
    "retry_base_backoff_ms",
    "retry_max_backoff_ms",
    "egress_mode",
    "active_proxy_count",
    "require_verified_exit_ip",
    "minimum_unique_exit_ips",
    "identity_endpoints",
    "identity_ttl_secs",
    "proxy_health_interval_secs",
    "proxy_restart_interval_secs",
    "max_proxy_restart_attempts",
    "allow_direct_fallback",
    "runtime_dir",
    "docker_binary",
    "warp_cli_binary",
    "warp_image",
    "worker_shutdown_timeout_secs",
    "server_shutdown_timeout_secs",
    "metrics_enabled",
    "request_id_header",
];

#[derive(Debug, Clone)]
pub struct ConfigPlan {
    pub merged: String,
    pub changed_keys: Vec<String>,
    pub restart_required: bool,
    pub warnings: Vec<String>,
}

pub fn preview_config(state: &AppState, incoming: &str) -> Result<ConfigPlan, ManagementError> {
    if incoming.len() > MAX_CONFIG_BYTES {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "config_too_large",
            format!("Configuration exceeds {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    let incoming_value = migrate_value(parse_document(incoming)?)?;
    validate_known_keys(&incoming_value)?;

    let existing = state
        .file_store
        .read(&state.config.management.config_path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();
    let existing_value = if existing.trim().is_empty() {
        migrate_value(toml::Value::Table(Default::default()))?
    } else {
        migrate_value(parse_document(&existing)?)?
    };

    let mut merged_value = existing_value.clone();
    merge_values(&mut merged_value, incoming_value);
    validate_document(&merged_value)?;
    let changed_keys = changed_paths(&existing_value, &merged_value);
    let restart_required = !changed_keys.is_empty();
    let warnings = warnings_for(&merged_value);
    let merged = toml::to_string_pretty(&merged_value).map_err(|err| {
        error(
            StatusCode::BAD_REQUEST,
            "config_serialize_failed",
            format!("Unable to serialize merged configuration: {err}"),
        )
    })?;

    Ok(ConfigPlan {
        merged,
        changed_keys,
        restart_required,
        warnings,
    })
}

pub fn preview_response(plan: &ConfigPlan) -> ConfigPreviewResponse {
    ConfigPreviewResponse {
        valid: true,
        changed_keys: plan.changed_keys.clone(),
        restart_required: plan.restart_required,
        warnings: plan.warnings.clone(),
    }
}

pub fn apply_config(
    state: &AppState,
    incoming: &str,
) -> Result<ConfigApplyResponse, ManagementError> {
    let plan = preview_config(state, incoming)?;
    let path = &state.config.management.config_path;
    let previous = state.file_store.read(path).ok();

    state
        .file_store
        .atomic_write(path, plan.merged.as_bytes(), true)
        .map_err(|err| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_write_failed",
                format!("Failed to atomically write configuration: {err}"),
            )
        })?;

    let post_write = state.file_store.read(path).map_err(|err| {
        rollback(state, previous.as_deref());
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_verify_failed",
            format!("Unable to read configuration after write; previous content restored: {err}"),
        )
    })?;
    let verified = std::str::from_utf8(&post_write)
        .ok()
        .and_then(|content| parse_document(content).ok())
        .and_then(|value| validate_document(&value).ok().map(|_| value));
    if verified.is_none() || post_write != plan.merged.as_bytes() {
        let rollback_ok = rollback(state, previous.as_deref());
        return Err(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_verify_failed",
            if rollback_ok {
                "Configuration verification failed and previous content was restored"
            } else {
                "Configuration verification failed and rollback also failed"
            },
        ));
    }

    Ok(ConfigApplyResponse {
        status: "ok".to_string(),
        path: path.display().to_string(),
        changed_keys: plan.changed_keys,
        restart_required: plan.restart_required,
        rollback_performed: false,
    })
}

fn rollback(state: &AppState, previous: Option<&[u8]>) -> bool {
    match previous {
        Some(content) => state
            .file_store
            .atomic_write(&state.config.management.config_path, content, true)
            .is_ok(),
        None => state
            .file_store
            .remove_if_exists(&state.config.management.config_path)
            .is_ok(),
    }
}

fn parse_document(content: &str) -> Result<toml::Value, ManagementError> {
    content.parse::<toml::Value>().map_err(|err| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_toml",
            format!("Invalid TOML: {err}"),
        )
    })
}

fn migrate_value(value: toml::Value) -> Result<toml::Value, ManagementError> {
    migration::migrate_value(value)
        .map(|(value, _report)| value)
        .map_err(|message| error(StatusCode::BAD_REQUEST, "config_migration_failed", message))
}

fn validate_document(value: &toml::Value) -> Result<(), ManagementError> {
    validate_known_keys(value)?;
    let text = toml::to_string(value).map_err(|err| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            format!("Configuration cannot be serialized: {err}"),
        )
    })?;
    let cfg: TomlConfig = toml::from_str(&text).map_err(|err| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            format!("Configuration has invalid field types: {err}"),
        )
    })?;

    if cfg.port == Some(0) || cfg.opencode_port == Some(0) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_port",
            "Ports must be between 1 and 65535",
        ));
    }
    if let Some(host) = cfg.host.as_deref() {
        host.parse::<std::net::IpAddr>().map_err(|_| {
            error(
                StatusCode::BAD_REQUEST,
                "invalid_host",
                "host must be a valid IPv4 or IPv6 address",
            )
        })?;
    }
    if let Some(policy) = cfg.shell_policy.as_deref() {
        if !matches!(policy, "disabled" | "allowlist" | "unrestricted") {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "invalid_shell_policy",
                "shell_policy must be disabled, allowlist, or unrestricted",
            ));
        }
    }
    if cfg.stream_buffer_size == Some(0)
        || cfg.channel_capacity == Some(0)
        || cfg
            .max_sse_line_bytes
            .is_some_and(|value| !(1024..=64 * 1024 * 1024).contains(&value))
        || cfg
            .max_sync_response_bytes
            .is_some_and(|value| !(1024..=64 * 1024 * 1024).contains(&value))
        || cfg.max_search_loops == Some(0)
        || cfg.search_max_results == Some(0)
        || cfg.search_max_snippet_chars == Some(0)
        || cfg
            .search_max_response_bytes
            .is_some_and(|value| value < 1024)
        || cfg.search_timeout_secs == Some(0)
        || cfg.active_proxy_count == Some(0)
        || cfg.minimum_unique_exit_ips == Some(0)
        || cfg.max_network_attempts == Some(0)
        || cfg.max_proxy_restart_attempts == Some(0)
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_zero_value",
            "Buffer, search, loop, proxy, identity, and retry limits must satisfy their minimum values",
        ));
    }
    if cfg.search_max_results.is_some_and(|value| value > 20) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_search_limit",
            "search_max_results cannot exceed 20",
        ));
    }
    if let Some(url) = cfg.upstream_base_url.as_deref() {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "invalid_upstream_url",
                "upstream_base_url must start with http:// or https://",
            ));
        }
    }
    if let (Some(base), Some(max)) = (cfg.retry_base_backoff_ms, cfg.retry_max_backoff_ms) {
        if max < base {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "invalid_retry_backoff",
                "retry_max_backoff_ms must be greater than or equal to retry_base_backoff_ms",
            ));
        }
    }
    if cfg.egress_mode.as_deref() == Some("direct") && cfg.require_verified_exit_ip == Some(true) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_egress_policy",
            "require_verified_exit_ip cannot be enabled in direct mode",
        ));
    }
    Ok(())
}

fn validate_known_keys(value: &toml::Value) -> Result<(), ManagementError> {
    let table = value.as_table().ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_config_root",
            "Configuration root must be a TOML table",
        )
    })?;
    let unknown = table
        .keys()
        .filter(|key| !KNOWN_ROOT_KEYS.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(error(
            StatusCode::BAD_REQUEST,
            "unknown_config_keys",
            format!("Unknown configuration keys: {}", unknown.join(", ")),
        ))
    }
}

fn merge_values(target: &mut toml::Value, incoming: toml::Value) {
    match (target, incoming) {
        (toml::Value::Table(target), toml::Value::Table(incoming)) => {
            for (key, value) in incoming {
                if let Some(existing) = target.get_mut(&key) {
                    merge_values(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (target, incoming) => *target = incoming,
    }
}

fn changed_paths(before: &toml::Value, after: &toml::Value) -> Vec<String> {
    let mut changed = BTreeSet::new();
    collect_changes("", before, after, &mut changed);
    changed.into_iter().collect()
}

fn collect_changes(
    prefix: &str,
    before: &toml::Value,
    after: &toml::Value,
    changed: &mut BTreeSet<String>,
) {
    match (before.as_table(), after.as_table()) {
        (Some(before), Some(after)) => {
            for key in before.keys().chain(after.keys()) {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match (before.get(key), after.get(key)) {
                    (Some(left), Some(right)) => collect_changes(&path, left, right, changed),
                    _ => {
                        changed.insert(path);
                    }
                }
            }
        }
        _ if before != after => {
            changed.insert(prefix.to_string());
        }
        _ => {}
    }
}

fn warnings_for(value: &toml::Value) -> Vec<String> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    if table.get("max_body_size").and_then(toml::Value::as_integer) == Some(0) {
        warnings.push("max_body_size=0 disables request body limits".to_string());
    }
    if table.get("shell_policy").and_then(toml::Value::as_str) == Some("unrestricted") {
        warnings.push(
            "unrestricted shell delegation should only be used in trusted development environments"
                .to_string(),
        );
    }
    warnings
}

fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> ManagementError {
    ManagementError::new(status, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use crate::infrastructure::file_store::FileStore;
    use std::io;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct MemoryStore {
        bytes: Mutex<Option<Vec<u8>>>,
        read_count: Mutex<usize>,
        corrupt_read_number: Mutex<Option<usize>>,
    }

    impl FileStore for MemoryStore {
        fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
            let current = {
                let mut count = self.read_count.lock().unwrap();
                *count += 1;
                *count
            };
            if *self.corrupt_read_number.lock().unwrap() == Some(current) {
                return Ok(b"not = [valid".to_vec());
            }
            self.bytes
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
        }
        fn atomic_write(&self, _path: &Path, content: &[u8], _sensitive: bool) -> io::Result<()> {
            *self.bytes.lock().unwrap() = Some(content.to_vec());
            Ok(())
        }
        fn remove_if_exists(&self, _path: &Path) -> io::Result<()> {
            *self.bytes.lock().unwrap() = None;
            Ok(())
        }
    }

    fn state(store: Arc<MemoryStore>) -> AppState {
        let config = BridgeConfig {
            primary_proxies: None,
            warm_standby_proxies: None,
            ..Default::default()
        };
        AppState::new_with_infrastructure(
            config,
            Arc::new(crate::docker::DockerCliRuntime::from_config(
                &BridgeConfig::default(),
            )),
            Arc::new(crate::infrastructure::warp::CliWarpController::new(
                "warp-cli",
            )),
            store,
        )
    }

    #[test]
    fn preview_recursively_merges_and_reports_changed_keys() {
        let store = Arc::new(MemoryStore::default());
        *store.bytes.lock().unwrap() = Some(b"port = 4000\nmodel = \"old\"\n".to_vec());
        let plan = preview_config(&state(store), "model = \"new\"\n").unwrap();
        assert_eq!(plan.changed_keys, vec!["model"]);
        assert!(plan.merged.contains("port = 4000"));
        assert!(plan.restart_required);
    }

    #[test]
    fn unknown_keys_and_invalid_policy_are_rejected() {
        let store = Arc::new(MemoryStore::default());
        let state = state(store);
        assert_eq!(
            preview_config(&state, "mystery = true").unwrap_err().code,
            "unknown_config_keys"
        );
        assert_eq!(
            preview_config(&state, "shell_policy = \"danger\"")
                .unwrap_err()
                .code,
            "invalid_shell_policy"
        );
    }

    #[test]
    fn failed_post_write_verification_rolls_back_previous_content() {
        let store = Arc::new(MemoryStore::default());
        let previous = b"port = 4000\n".to_vec();
        *store.bytes.lock().unwrap() = Some(previous.clone());
        *store.corrupt_read_number.lock().unwrap() = Some(3);
        let state = state(store.clone());
        let error = apply_config(&state, "model = \"new\"").unwrap_err();
        assert_eq!(error.code, "config_verify_failed");
        assert_eq!(*store.bytes.lock().unwrap(), Some(previous));
    }
}
