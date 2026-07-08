//! SOC dashboard — real-time monitoring, config editor, proxy management, SSE events.
//!
//! ## Endpoints
//!
//! | Method | Path | Purpose |
//! |--------|------|---------|
//! | GET | `/dashboard/*path` | Serve dashboard web UI (SPA) |
//! | GET | `/api/dashboard/status` | Bridge status + uptime + proxy tier stats |
//! | GET | `/api/dashboard/proxies` | Detailed proxy node list |
//! | GET | `/api/dashboard/config` | Active config (secrets masked) |
//! | POST | `/api/dashboard/config/save` | Atomic TOML config write |
//! | GET | `/api/dashboard/events` | Server-Sent Events stream |
//! | POST | `/api/dashboard/proxy/:port/restart` | Restart a managed proxy container |

use crate::proxy_pool::is_protected_proxy_port;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures_util::Stream;
use serde::Serialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

// ── Constants ──

/// Keep-alive interval for SSE connections (15 seconds).
const SSE_KEEPALIVE_SECS: u64 = 15;
/// Heartbeat interval for dashboard events (30 seconds).
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
/// Default TOML config file path.
const DEFAULT_CONFIG_PATH: &str = "opencode2api.toml";

// ── Dashboard Events ──

/// Events emitted to dashboard SSE clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    /// Proxy status changed (started, stopped, restarted, cooldown, recovered).
    #[serde(rename = "proxy_status")]
    ProxyStatus {
        port: u16,
        status: String,
        timestamp: String,
    },
    /// Log message from a proxy container.
    #[serde(rename = "proxy_log")]
    ProxyLog {
        port: u16,
        message: String,
        level: String,
        timestamp: String,
    },
    /// Configuration was saved.
    #[serde(rename = "config_saved")]
    ConfigSaved { timestamp: String },
    /// Periodic heartbeat to keep SSE connections alive.
    #[serde(rename = "heartbeat")]
    Heartbeat { timestamp: String },
    /// Error event for the dashboard.
    #[serde(rename = "error")]
    DashboardError { message: String, timestamp: String },
}

// ── Web Assets (Embedded SPA) ──

/// Embedded dashboard web UI assets from `src/webui/`.
#[derive(rust_embed::RustEmbed)]
#[folder = "src/webui/"]
struct WebAssets;

// ── Handlers ──

/// Add baseline browser security headers to the response
fn add_security_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src 'self' https://fonts.gstatic.com; \
             img-src 'self' data: https://raw.githubusercontent.com; \
             connect-src 'self' ws: wss:; \
             frame-ancestors 'none';",
        ),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

/// Serve the web UI SPA — serves embedded assets with fallback to `index.html`.
pub async fn serve_webui(uri: axum::http::Uri) -> Result<(HeaderMap, Vec<u8>), StatusCode> {
    let path = uri.path();
    let path = path.strip_prefix("/dashboard").unwrap_or(path);
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let asset = WebAssets::get(path).or_else(|| {
        // SPA fallback — serve index.html for unknown paths
        warn!(
            "Dashboard asset not found: '{}', falling back to index.html",
            path
        );
        WebAssets::get("index.html")
    });

    match asset {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            );
            headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
            add_security_headers(&mut headers);
            Ok((headers, content.data.to_vec()))
        }
        None => {
            warn!("Dashboard index.html not found in embedded assets");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

/// Serve the beautiful landing page at the root URL (/)
pub async fn serve_landing() -> Result<(HeaderMap, Vec<u8>), StatusCode> {
    let asset = WebAssets::get("landing.html");
    match asset {
        Some(content) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            );
            headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
            add_security_headers(&mut headers);
            Ok((headers, content.data.to_vec()))
        }
        None => {
            warn!("Dashboard landing.html not found in embedded assets");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

/// GET /api/dashboard/status — bridge status with uptime and proxy tier stats.
pub async fn handler_rest_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;
    let pool = state.proxy_pool.read().await;
    let snapshot = pool.snapshot();
    let uptime_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(state.started_at.load(Ordering::Relaxed));

    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime": uptime_string(uptime_secs),
        "uptime_secs": uptime_secs,
        "model": state.config.model,
        "bridge_port": state.config.bridge_port,
        "auth_enabled": state.config.auth_enabled(),
        "admin_token_configured": true,
        "shell_policy": state.config.shell_policy.description(),
        "primary_proxies": {
            "total": snapshot.primary.total,
            "healthy": snapshot.primary.healthy,
            "degraded": snapshot.primary.degraded,
            "cooldown": snapshot.primary.cooldown,
            "recovering": snapshot.primary.recovering,
            "dead": snapshot.primary.dead,
        },
        "warm_standby": {
            "total": snapshot.warm_standby.total,
            "healthy": snapshot.warm_standby.healthy,
            "degraded": snapshot.warm_standby.degraded,
            "cooldown": snapshot.warm_standby.cooldown,
            "recovering": snapshot.warm_standby.recovering,
            "dead": snapshot.warm_standby.dead,
        },
    })))
}

/// GET /api/dashboard/proxies — detailed proxy node list.
pub async fn handler_proxies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;
    let pool = state.proxy_pool.read().await;
    let nodes: Vec<Value> = pool
        .proxies
        .iter()
        .map(|entry| {
            let cooldown_remaining = match entry.status {
                crate::proxy_pool::ProxyStatus::Cooldown(until) => until
                    .checked_duration_since(std::time::Instant::now())
                    .map(|d| d.as_secs()),
                _ => None,
            };

            json!({
                "port": entry.port,
                "role": entry.role,
                "lifecycle": entry.lifecycle,
                "status": entry.status.description(),
                "failure_count": entry.consecutive_failures,
                "success_count": entry.consecutive_successes,
                "cooldown_remaining_secs": cooldown_remaining,
            })
        })
        .collect();
    Ok(Json(nodes))
}

/// GET /api/dashboard/config — active configuration with secrets masked.
pub async fn handler_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;
    let cfg = &state.config;
    Ok(Json(json!({
        "host": cfg.host.to_string(),
        "bridge_port": cfg.bridge_port,
        "model": cfg.model,
        "shell_policy": cfg.shell_policy.description(),
        "auth_tokens": mask_auth_tokens(&cfg.auth_tokens),
        "max_body_size": cfg.max_body_size,
        "stream_buffer_size": cfg.stream_buffer_size,
        "channel_capacity": cfg.channel_capacity,
        "tavily_api_key": cfg.tavily_api_key.as_deref().map(mask_secret),
        "exa_api_key": cfg.exa_api_key.as_deref().map(mask_secret),
        "serper_api_key": cfg.serper_api_key.as_deref().map(mask_secret),
        "searxng_url": cfg.searxng_url,
        "primary_proxies": cfg.primary_proxies.as_ref().map(|p| p.join(", ")),
        "warm_standby_proxies": cfg.warm_standby_proxies.as_ref().map(|p| p.join(", ")),
    })))
}

/// POST /api/dashboard/config/save — atomic TOML config write.
pub async fn handler_config_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;

    let content = String::from_utf8_lossy(&body);

    // Validate that the body parses as valid TOML
    if let Err(e) = content.parse::<toml::Table>() {
        return Ok(Json(json!({
            "status": "error",
            "message": format!("Invalid TOML: {}", e),
        })));
    }

    // Atomic write: write to .tmp, fsync, rename
    let config_path =
        std::env::var("BRIDGE_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());

    let tmp_path = format!("{}.tmp", config_path);

    match std::fs::write(&tmp_path, content.as_bytes()) {
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
                    Ok(Json(json!({ "status": "ok", "path": config_path })))
                }
                Err(e) => {
                    error!("Dashboard: failed to rename config file: {}", e);
                    let _ = std::fs::remove_file(&tmp_path);
                    Ok(Json(json!({
                        "status": "error",
                        "message": format!("Failed to write config: {}", e),
                    })))
                }
            }
        }
        Err(e) => {
            error!("Dashboard: failed to write config file: {}", e);
            Ok(Json(json!({
                "status": "error",
                "message": format!("Failed to write config: {}", e),
            })))
        }
    }
}

/// POST /api/dashboard/proxy/:port/restart — restart a managed proxy container.
pub async fn handler_proxy_restart(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(port): Path<u16>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;

    // Check port is not protected
    if is_protected_proxy_port(port) {
        return Ok(Json(json!({
            "status": "error",
            "message": format!("Port {} is a protected warm-standby proxy and cannot be restarted via dashboard", port),
        })));
    }

    match crate::docker::create_container(port).await {
        Ok(()) => {
            let ts = unix_timestamp();
            let _ = state.event_tx.send(DashboardEvent::ProxyStatus {
                port,
                status: "restarted".to_string(),
                timestamp: ts,
            });
            Ok(Json(json!({ "status": "ok", "port": port })))
        }
        Err(e) => {
            error!("Dashboard: failed to restart proxy on port {}: {}", port, e);
            Ok(Json(json!({
                "status": "error",
                "message": format!("Failed to restart proxy on port {}: {}", port, e),
            })))
        }
    }
}

/// GET /api/dashboard/events — SSE event stream for real-time dashboard updates.
pub async fn handler_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<Value>),
> {
    let token = params.get("token").map(|s| s.as_str());
    check_admin_token(&headers, token)?;

    let mut rx = state.event_tx.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let event_type = match &event {
                        DashboardEvent::ProxyStatus { .. } => "proxy_status",
                        DashboardEvent::ProxyLog { .. } => "proxy_log",
                        DashboardEvent::ConfigSaved { .. } => "config_saved",
                        DashboardEvent::Heartbeat { .. } => "heartbeat",
                        DashboardEvent::DashboardError { .. } => "error",
                    };
                    let json = serde_json::to_string(&event).unwrap_or_default();
                    yield Ok(Event::default().event(event_type).data(json));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Dashboard SSE client lagged, dropped {} events", n);
                    let fallback = DashboardEvent::DashboardError {
                        message: format!("dropped {} events", n),
                        timestamp: unix_timestamp(),
                    };
                    let json = serde_json::to_string(&fallback).unwrap_or_default();
                    yield Ok(Event::default().data(json));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    let sse = Sse::new(stream)
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(SSE_KEEPALIVE_SECS)));
    Ok(sse)
}

// ── Background Tasks ──

/// Spawn a heartbeat task that sends a `Heartbeat` event every 30 seconds.
pub fn spawn_heartbeat(event_tx: broadcast::Sender<DashboardEvent>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if event_tx
                .send(DashboardEvent::Heartbeat {
                    timestamp: unix_timestamp(),
                })
                .is_err()
            {
                // No subscribers — that's fine, keep going
            }
        }
    });
    info!(
        "Dashboard heartbeat task spawned ({}s interval).",
        HEARTBEAT_INTERVAL_SECS
    );
}

// ── Helpers ──

/// Format a duration in seconds to a human-readable string.
pub fn uptime_string(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

/// Return current Unix epoch timestamp as a string.
pub fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

/// Mask API key: show first 5 chars + "..." + last 4 chars.
fn mask_secret(s: &str) -> String {
    if s.len() <= 10 {
        return "***".to_string();
    }
    format!("{}...{}", &s[..5], &s[s.len() - 4..])
}

/// Mask auth tokens: return "***" if any tokens are configured.
fn mask_auth_tokens(tokens: &Option<Vec<String>>) -> Value {
    match tokens {
        Some(t) if !t.is_empty() => json!("***"),
        Some(_) => json!([]),
        None => json!(null),
    }
}

/// POST /api/dashboard/login — check X-Dashboard-Token header against DASHBOARD_ADMIN_TOKEN.
pub async fn handler_login(
    headers: HeaderMap,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;
    Ok(Json(json!({ "status": "ok", "success": true })))
}

/// GET /api/dashboard/diagnostics — Rich operational status for authenticated admin.
pub async fn handler_dashboard_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    check_admin_token(&headers, None)?;

    let daemon_ok =
        crate::opencode::check_daemon(&state.http_client, state.config.opencode_port).await;
    let proxy_pool_stats = state.proxy_pool.read().await.snapshot();

    Ok(Json(json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "daemon": {
            "status": if daemon_ok { "connected" } else { "disconnected" },
            "port": state.config.opencode_port
        },
        "config": {
            "model": state.config.model.as_deref().unwrap_or("(default)"),
            "shell_policy": state.config.shell_policy.description(),
            "auth_enabled": state.config.auth_enabled(),
            "bridge_port": state.config.bridge_port
        },
        "proxy_pool": proxy_pool_stats
    })))
}

/// Check DASHBOARD_ADMIN_TOKEN env var against the X-Dashboard-Token header.
/// Returns `Ok(())` if the token is valid or no admin token is configured.
fn check_admin_token(
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let admin_token = match std::env::var("DASHBOARD_ADMIN_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            // If DASHBOARD_ADMIN_TOKEN is unset or empty, fail closed (disable admin access)
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "error",
                    "message": "Dashboard is disabled: admin token is not configured on the server",
                })),
            ));
        }
    };

    let request_token = headers
        .get("X-Dashboard-Token")
        .and_then(|v| v.to_str().ok())
        .or(query_token)
        .unwrap_or("");

    // Fail closed if the request token is empty
    if request_token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "error",
                "message": "Please enter password to login",
            })),
        ));
    }

    // Fail closed if the request token doesn't match the configured token
    if request_token == admin_token {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "error",
                "message": "Invalid password",
            })),
        ))
    }
}
