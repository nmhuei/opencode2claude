//! Versioned REST management API.
//!
//! HTTP concerns stay in this module. Authentication, snapshots, redaction, and
//! proxy lifecycle rules are implemented by `crate::management`.

use crate::management::{auth, service};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

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

impl From<service::ManagementError> for ApiError {
    fn from(error: service::ManagementError) -> Self {
        Self::new(error.status, error.code, error.message)
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

fn authorize(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = auth::rest_token().ok_or_else(|| {
        ApiError::unauthorized("REST API is disabled because REST_API_TOKEN is not configured")
    })?;
    let provided = auth::bearer_token(headers)
        .ok_or_else(|| ApiError::unauthorized("Missing or invalid Authorization: Bearer header"))?;

    if auth::token_eq(provided.as_bytes(), expected.as_bytes()) {
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
    let snapshot = service::proxy_snapshot(&state).await;
    let egress_mode = if snapshot.nodes.is_empty() {
        "direct"
    } else {
        "proxy"
    };

    Ok(Json(json!({
        "status": "ok",
        "service": "opencode2api",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": service::uptime_secs(&state),
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
    let snapshot = service::proxy_snapshot(&state).await;

    Ok(Json(json!({
        "policy": snapshot.policy,
        "primary": snapshot.primary,
        "warm_standby": snapshot.warm_standby,
        "nodes": snapshot.nodes,
    })))
}

/// GET /api/v1/config — operational configuration with secret values removed.
async fn config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    let cfg = service::safe_config_snapshot(&state);

    Ok(Json(json!({
        "host": cfg.host,
        "bridge_port": cfg.bridge_port,
        "opencode_port": cfg.opencode_port,
        "model": cfg.model,
        "shell_policy": cfg.shell_policy,
        "max_body_size": cfg.max_body_size,
        "stream_buffer_size": cfg.stream_buffer_size,
        "channel_capacity": cfg.channel_capacity,
        "max_search_loops": cfg.max_search_loops,
        "primary_proxies": cfg.primary_proxies,
        "warm_standby_proxies": cfg.warm_standby_proxies,
        "features": {
            "client_auth_configured": cfg.client_auth_configured,
            "tavily_configured": cfg.tavily_configured,
            "exa_configured": cfg.exa_configured,
            "serper_configured": cfg.serper_configured,
            "searxng_configured": cfg.searxng_configured,
            "searxng_api_key_configured": cfg.searxng_api_key_configured,
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
    let result = service::restart_managed_proxy(&state, port).await?;

    Ok(Json(json!({
        "status": "ok",
        "proxy": result,
    })))
}

/// GET /api/v1/openapi.json
async fn openapi(headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    Ok(Json(openapi_document()))
}

fn openapi_document() -> Value {
    json!({
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
    })
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

    #[tokio::test]
    async fn openapi_requires_bearer_authentication() {
        let _guard = env_lock().lock().await;
        std::env::set_var(auth::REST_API_TOKEN_ENV, "test-rest-token");
        std::env::remove_var(auth::DASHBOARD_TOKEN_ENV);

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
        std::env::remove_var(auth::REST_API_TOKEN_ENV);
    }

    #[tokio::test]
    async fn protected_proxy_restart_returns_forbidden_without_touching_docker() {
        let _guard = env_lock().lock().await;
        std::env::set_var(auth::REST_API_TOKEN_ENV, "test-rest-token");

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
        std::env::remove_var(auth::REST_API_TOKEN_ENV);
    }
}
