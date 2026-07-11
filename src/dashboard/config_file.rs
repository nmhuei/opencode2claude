//! Raw configuration inspection and atomic configuration writes for the dashboard.

use super::auth::check_admin_token;
use super::events::DashboardEvent;
use super::time::unix_timestamp;
use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use tracing::{error, info};

/// GET /api/dashboard/config/raw — return the raw config file content (including secrets).
pub async fn handler_config_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;
    let config_path = state
        .config
        .management
        .config_path
        .to_string_lossy()
        .to_string();
    let raw = std::fs::read_to_string(&config_path).unwrap_or_default();
    Ok(Json(json!({ "raw": raw })))
}

/// POST /api/dashboard/config/save — atomic TOML config write with merge.
///
/// Accepts JSON body: `{ "content": "<TOML string>" }`.
/// Merges incoming fields into the existing config file so that fields not
/// present in the incoming content are preserved (e.g. API keys not visible
/// in the form are not lost).
pub async fn handler_config_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<axum::Json<Value>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    check_admin_token(&state, &headers, None)?;

    // Extract TOML content from JSON body
    let incoming_toml = match body {
        Some(Json(ref payload)) => match payload.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "status": "error",
                        "success": false,
                        "message": "Missing 'content' field in JSON body",
                    })),
                ));
            }
        },
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status": "error",
                    "success": false,
                    "message": "Request body is required",
                })),
            ));
        }
    };

    // Validate that the body parses as valid TOML
    if let Err(e) = incoming_toml.parse::<toml::Table>() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "error",
                "success": false,
                "message": format!("Invalid TOML: {}", e),
            })),
        ));
    }

    // Read existing config file content
    let config_path = state
        .config
        .management
        .config_path
        .to_string_lossy()
        .to_string();
    let existing_content = std::fs::read_to_string(&config_path).unwrap_or_default();

    // Merge: incoming overrides existing, existing fills gaps
    let merged = merge_toml_configs(&existing_content, &incoming_toml);

    // Atomic write: write to .tmp, fsync, rename
    let tmp_path = format!("{}.tmp", config_path);

    match std::fs::write(&tmp_path, merged.as_bytes()) {
        Ok(()) => {
            // Open file for fsync
            if let Ok(file) = std::fs::File::open(&tmp_path) {
                let _ = file.sync_all();
            }
            match std::fs::rename(&tmp_path, &config_path) {
                Ok(()) => {
                    info!("Dashboard: configuration saved to {}", config_path);
                    let ts = unix_timestamp();
                    let _ = state.event_tx.send(DashboardEvent::ConfigSaved {
                        timestamp: ts.clone(),
                    });
                    Ok(Json(
                        json!({ "status": "ok", "path": config_path, "success": true }),
                    ))
                }
                Err(e) => {
                    error!("Dashboard: failed to rename config file: {}", e);
                    let _ = std::fs::remove_file(&tmp_path);
                    Ok(Json(json!({
                        "status": "error",
                        "success": false,
                        "message": format!("Failed to write config: {}", e),
                    })))
                }
            }
        }
        Err(e) => {
            error!("Dashboard: failed to write config file: {}", e);
            Ok(Json(json!({
                "status": "error",
                "success": false,
                "message": format!("Failed to write config: {}", e),
            })))
        }
    }
}

/// Merge a new TOML content into existing content.
/// Fields present in the new content override existing ones.
/// Fields NOT present in the new content are preserved from the existing content.
/// This ensures that secrets (API keys, auth tokens) not included in the
/// incoming content are not lost during saves from the dashboard UI.
fn merge_toml_configs(existing: &str, incoming: &str) -> String {
    // Parse lines from both into key -> value maps
    fn parse_toml_lines(text: &str) -> BTreeMap<String, String> {
        text.lines()
            .filter_map(|line| {
                let line = line.trim();
                // Match key = "value" or key = value (skip comments and blank lines)
                if line.starts_with('#') || line.is_empty() {
                    return None;
                }
                if let Some(eq_pos) = line.find('=') {
                    let key = line[..eq_pos].trim().to_string();
                    let val = line[eq_pos + 1..].trim().to_string();
                    Some((key, val))
                } else {
                    None
                }
            })
            .collect()
    }

    let _existing_map = parse_toml_lines(existing);
    let incoming_map = parse_toml_lines(incoming);

    // Build result: incoming overrides, existing fills gaps
    let mut result = String::new();
    let mut seen = HashSet::new();

    // First pass: existing lines, but override values from incoming
    for line in existing.lines() {
        let trimmed = line.trim();
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            if incoming_map.contains_key(&key) {
                // Override with incoming value
                if let Some(new_val) = incoming_map.get(&key) {
                    result.push_str(&format!("{} = {}\n", key, new_val));
                }
                seen.insert(key);
            } else {
                // Preserve existing line unchanged (including original formatting/whitespace)
                result.push_str(line);
                result.push('\n');
            }
        } else {
            // Non-key-value lines (comments, blank lines, section headers) are preserved
            result.push_str(line);
            result.push('\n');
        }
    }

    // Second pass: add incoming keys not already seen (new keys from incoming)
    for (key, val) in &incoming_map {
        if !seen.contains(key) {
            result.push_str(&format!("{} = {}\n", key, val));
        }
    }

    result
}
