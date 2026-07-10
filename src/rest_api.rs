//! Versioned REST management API.
//!
//! This module is intentionally independent from the browser dashboard API.
//! It uses standard Bearer authentication and returns stable, structured JSON.

use crate::dashboard::DashboardEvent;
use crate::proxy_pool::{is_managed_proxy_port, is_protected_proxy_port};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info};

const REST_API_TOKEN_ENV: &str = "REST_API_TOKEN";
const DASHBOARD_TOKEN_ENV: &str = "DASHBOARD_ADMIN_TOKEN";

/// Build the versioned REST management router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/proxies", get(proxies))
        .route("/api/v1/config", get(config))
        .route("/api/v1/proxies/:port/restart", post(restart_proxy))
        .route("/api/v1/openapi.json", get(openapi))
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Debug, Serialize)]
struct ApiErrorDetail {
    code: &'static str,
    message: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ApiErrorBody {
                error: ApiErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response();

        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer realm=\"opencode2api-rest\""),
            );
        }

        response
    }
}

fn configured_token() -> Option<String> {
    std::env::var(REST_API_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var(DASHBOARD_TOKEN_ENV)
                .ok()
                .filter(|value| !value.is_empty())
        })
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return None;
    }
    Some(token)
}

/// Compare token bytes without returning early on the first mismatch.
fn token_eq(provided: &[u8], expected: &[u8]) -> bool {
    let max_len = provided.len().max(expected.len());
    let mut diff = provided.len() ^ expected.len();

    for index in 0..max_len {
        let left = provided.get(index).copied().unwrap_or_default();
        let right = expected.get(index).copied().unwrap_or_default();
        diff |= usize::from(left ^ right);
    }

    diff == 0
}

fn redact_proxy_url(value: &str) -> String {
    let Some(scheme_end) = value.find("://") else {
        return value.to_string();
    };
    let authority_start = scheme_end + 3;
    let Some(relative_at) = value[authority_start..].find('@') else {
        return value.to_string();
    };
    let at = authority_start + relative_at;
    format!("{}***{}", &value[..authority_start], &value[at..])
}

fn authorize(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = configured_token().ok_or_else(|| {
        ApiError::unauthorized("REST API is disabled because REST_API_TOKEN is not configured")
    })?;

    let provided = bearer_token(headers)
        .ok_or_else(|| ApiError::unauthorized("Missing or invalid Authorization: Bearer header"))?;

    if token_eq(provided.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::unauthorized("Invalid REST API token"))
    }
}

/// GET /api/v1/status
async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;

    let mut pool = state.proxy_pool.write().await;
    pool.recover_expired_cooldowns();
    let snapshot = pool.snapshot();
    drop(pool);

    let uptime_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(state.started_at.load(Ordering::Relaxed));

    let egress_mode = if snapshot.nodes.is_empty() {
        "direct"
    } else {
        "proxy"
    };

    Ok(Json(json!({
        "status": "ok",
        "service": "opencode2api",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime_secs,
        "model": state.config.model,
        "bridge": {
            "host": state.config.host.to_string(),
            "port": state.config.bridge_port,
            "client_auth_enabled": state.config.auth_enabled(),
        },
        "egress": {
            "mode": egress_mode,
            "proxy_pool": snapshot,
        },
    })))
}

/// GET /api/v1/proxies
async fn proxies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;

    let mut pool = state.proxy_pool.write().await;
    pool.recover_expired_cooldowns();
    let snapshot = pool.snapshot();

    Ok(Json(json!({
        "policy": snapshot.policy,
        "primary": snapshot.primary,
        "warm_standby": snapshot.warm_standby,
        "nodes": snapshot.nodes,
    })))
}

/// GET /api/v1/config
///
/// Returns operational configuration only. Secret values are never returned.
async fn config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    let cfg = &state.config;

    let primary_proxies: Vec<String> = cfg
        .primary_proxies
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|value| redact_proxy_url(value))
        .collect();
    let warm_standby_proxies: Vec<String> = cfg
        .warm_standby_proxies
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|value| redact_proxy_url(value))
        .collect();

    Ok(Json(json!({
        "host": cfg.host.to_string(),
        "bridge_port": cfg.bridge_port,
        "opencode_port": cfg.opencode_port,
        "model": cfg.model,
        "shell_policy": cfg.shell_policy.kind(),
        "max_body_size": cfg.max_body_size,
        "stream_buffer_size": cfg.stream_buffer_size,
        "channel_capacity": cfg.channel_capacity,
        "max_search_loops": cfg.max_search_loops,
        "primary_proxies": primary_proxies,
        "warm_standby_proxies": warm_standby_proxies,
        "features": {
            "client_auth_configured": cfg.auth_tokens.as_ref().is_some_and(|tokens| !tokens.is_empty()),
            "tavily_configured": cfg.tavily_api_key.is_some(),
            "exa_configured": cfg.exa_api_key.is_some(),
            "serper_configured": cfg.serper_api_key.is_some(),
            "searxng_configured": cfg.searxng_url.is_some(),
            "searxng_api_key_configured": cfg.searxng_api_key.is_some(),
        }
    })))
}

/// POST /api/v1/proxies/:port/restart
async fn restart_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(port): Path<u16>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;

    if is_protected_proxy_port(port) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "protected_proxy",
            format!("Proxy port {port} is protected and cannot be restarted"),
        ));
    }

    if !is_managed_proxy_port(port) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_proxy_port",
            format!("Proxy port {port} is not a managed primary proxy"),
        ));
    }

    let configured = {
        let pool = state.proxy_pool.read().await;
        pool.proxies.iter().any(|entry| entry.port == port)
    };

    if !configured {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "proxy_not_found",
            format!("Proxy port {port} is not present in the active configuration"),
        ));
    }

    crate::docker::create_container(port).await.map_err(|err| {
        error!(port, error = %err, "REST API failed to restart proxy");
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "proxy_restart_failed",
            format!("Failed to restart proxy port {port}: {err}"),
        )
    })?;

    info!(port, "REST API restarted managed proxy");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let _ = state.event_tx.send(DashboardEvent::ProxyStatus {
        port,
        status: "restarted".to_string(),
        timestamp,
    });

    Ok(Json(json!({
        "status": "ok",
        "proxy": {
            "port": port,
            "action": "restart",
        }
    })))
}

/// GET /api/v1/openapi.json
async fn openapi(headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;

    Ok(Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "OpenCode2API Management API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Versioned REST API for bridge status, safe configuration inspection, and managed proxy operations."
        },
        "servers": [{ "url": "/" }],
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "opaque"
                }
            }
        },
        "security": [{ "bearerAuth": [] }],
        "paths": {
            "/api/v1/status": {
                "get": { "summary": "Get bridge and egress status", "responses": { "200": { "description": "Current status" }, "401": { "description": "Unauthorized" } } }
            },
            "/api/v1/proxies": {
                "get": { "summary": "List proxy pool state", "responses": { "200": { "description": "Proxy pool snapshot" }, "401": { "description": "Unauthorized" } } }
            },
            "/api/v1/config": {
                "get": { "summary": "Get redacted operational configuration", "responses": { "200": { "description": "Safe configuration" }, "401": { "description": "Unauthorized" } } }
            },
            "/api/v1/proxies/{port}/restart": {
                "post": {
                    "summary": "Restart a managed primary proxy",
                    "parameters": [{ "name": "port", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 1, "maximum": 65535 } }],
                    "responses": {
                        "200": { "description": "Proxy restarted" },
                        "400": { "description": "Invalid proxy port" },
                        "401": { "description": "Unauthorized" },
                        "403": { "description": "Protected proxy" },
                        "404": { "description": "Proxy is not configured" },
                        "502": { "description": "Container restart failed" }
                    }
                }
            },
            "/api/v1/openapi.json": {
                "get": { "summary": "Get this OpenAPI document", "responses": { "200": { "description": "OpenAPI document" }, "401": { "description": "Unauthorized" } } }
            }
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use crate::shell::ShellPolicy;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::OnceLock;
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn state() -> AppState {
        AppState::new(BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            model: Some("test-model".to_string()),
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 64,
            tavily_api_key: Some("secret-that-must-not-leak".to_string()),
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 3,
            proxies: None,
            primary_proxies: None,
            warm_standby_proxies: None,
        })
    }

    #[test]
    fn token_comparison_handles_equal_and_different_lengths() {
        assert!(token_eq(b"abc", b"abc"));
        assert!(!token_eq(b"abc", b"abd"));
        assert!(!token_eq(b"abc", b"abcd"));
        assert!(!token_eq(b"", b"abc"));
    }

    #[test]
    fn proxy_credentials_are_redacted() {
        assert_eq!(
            redact_proxy_url("socks5://user:password@127.0.0.1:40001"),
            "socks5://***@127.0.0.1:40001"
        );
        assert_eq!(
            redact_proxy_url("socks5://127.0.0.1:40001"),
            "socks5://127.0.0.1:40001"
        );
    }

    #[tokio::test]
    async fn openapi_requires_bearer_authentication() {
        let _guard = env_lock().lock().await;
        std::env::set_var(REST_API_TOKEN_ENV, "test-rest-token");
        std::env::remove_var(DASHBOARD_TOKEN_ENV);

        let app = router().with_state(state());
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/openapi.json")
                    .header(header::AUTHORIZATION, "Bearer test-rest-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);

        std::env::remove_var(REST_API_TOKEN_ENV);
    }

    #[tokio::test]
    async fn protected_proxy_restart_returns_forbidden_without_touching_docker() {
        let _guard = env_lock().lock().await;
        std::env::set_var(REST_API_TOKEN_ENV, "test-rest-token");

        let response = router()
            .with_state(state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/proxies/40004/restart")
                    .header(header::AUTHORIZATION, "Bearer test-rest-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        std::env::remove_var(REST_API_TOKEN_ENV);
    }
}
