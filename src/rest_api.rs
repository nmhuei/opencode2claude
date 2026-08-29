//! Versioned REST management API backed by shared typed DTOs.

use crate::audit::AuditOutcome;
use crate::management::{auth, config_apply, dto, service};
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{rejection::JsonRejection, Extension, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MANAGEMENT_PATHS: &[(&str, &str)] = &[
    ("/api/v1/status", "/api/v1/status"),
    ("/api/v1/proxies", "/api/v1/proxies"),
    ("/api/v1/config", "/api/v1/config"),
    ("/api/v1/config/preview", "/api/v1/config/preview"),
    ("/api/v1/config/apply", "/api/v1/config/apply"),
    (
        "/api/v1/proxies/:port/restart",
        "/api/v1/proxies/{port}/restart",
    ),
    (
        "/api/v1/proxies/:port/drain",
        "/api/v1/proxies/{port}/drain",
    ),
    (
        "/api/v1/proxies/:port/undrain",
        "/api/v1/proxies/{port}/undrain",
    ),
    ("/api/v1/metrics", "/api/v1/metrics"),
    ("/api/v1/audit", "/api/v1/audit"),
    ("/api/v1/openapi.json", "/api/v1/openapi.json"),
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(MANAGEMENT_PATHS[0].0, get(status))
        .route(MANAGEMENT_PATHS[1].0, get(proxies))
        .route(MANAGEMENT_PATHS[2].0, get(config))
        .route(MANAGEMENT_PATHS[3].0, post(preview_config))
        .route(MANAGEMENT_PATHS[4].0, post(apply_config))
        .route(MANAGEMENT_PATHS[5].0, post(restart_proxy))
        .route(MANAGEMENT_PATHS[6].0, post(drain_proxy))
        .route(MANAGEMENT_PATHS[7].0, post(undrain_proxy))
        .route(MANAGEMENT_PATHS[8].0, get(metrics))
        .route(MANAGEMENT_PATHS[9].0, get(audit_events))
        .route(MANAGEMENT_PATHS[10].0, get(openapi))
        .layer(axum::middleware::from_fn(no_store))
}

/// Status/config/audit payloads change with live pool and configuration
/// state; cached copies mislead operators and dashboards.
async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
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
            Json(dto::ApiErrorBody {
                error: dto::ApiErrorDetail {
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

fn authorize(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    let expected = state.config.management.rest_token().ok_or_else(|| {
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

/// Bound extractor rejection text so malformed input cannot push unbounded
/// framework diagnostics into an API response.
fn bounded_rejection_message(error: &JsonRejection) -> String {
    const MAX_REJECTION_MESSAGE_BYTES: usize = 300;
    let text = error.body_text();
    if text.len() <= MAX_REJECTION_MESSAGE_BYTES {
        return text;
    }
    let mut end = MAX_REJECTION_MESSAGE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<dto::StatusResponse>, ApiError> {
    authorize(&headers, &state)?;
    let egress = service::egress_operational_snapshot(&state).await;
    let workers = state.workers.snapshot();
    Ok(Json(dto::StatusResponse {
        status: "ok".to_string(),
        service: "opencode2api".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: service::uptime_secs(&state),
        model: state.config.model.clone(),
        bridge: dto::BridgeSummary {
            host: state.config.host.to_string(),
            port: state.config.bridge_port,
            client_auth_enabled: state.api_keys.read().await.configured(),
        },
        egress: egress.into(),
        workers,
    }))
}

async fn proxies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<dto::ProxiesResponse>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(service::proxy_snapshot(&state).await.into()))
}

async fn config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<dto::ConfigResponse>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(service::safe_config_snapshot(&state).await.into()))
}

async fn preview_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<dto::ConfigDocumentRequest>, JsonRejection>,
) -> Result<Json<dto::ConfigPreviewResponse>, ApiError> {
    authorize(&headers, &state)?;
    let Json(request) = payload.map_err(|error| {
        ApiError::new(
            error.status(),
            "invalid_request_body",
            bounded_rejection_message(&error),
        )
    })?;
    let plan = config_apply::preview_config(&state, &request.content)?;
    Ok(Json(config_apply::preview_response(&plan)))
}

async fn apply_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    payload: Result<Json<dto::ConfigDocumentRequest>, JsonRejection>,
) -> Result<Json<dto::ConfigApplyResponse>, ApiError> {
    authorize(&headers, &state)?;
    let Json(request) = payload.map_err(|error| {
        ApiError::new(
            error.status(),
            "invalid_request_body",
            bounded_rejection_message(&error),
        )
    })?;
    let correlation = request_id.map(|Extension(value)| value.0);
    match config_apply::apply_config(&state, &request.content) {
        Ok(result) => {
            state.audit_log.record(
                "rest",
                "config_apply",
                "configuration",
                AuditOutcome::Success,
                correlation,
                BTreeMap::from([
                    (
                        "changed_key_count".to_string(),
                        result.changed_keys.len().to_string(),
                    ),
                    (
                        "restart_required".to_string(),
                        result.restart_required.to_string(),
                    ),
                    (
                        "rollback_performed".to_string(),
                        result.rollback_performed.to_string(),
                    ),
                ]),
            );
            let _ = state
                .event_tx
                .send(crate::dashboard::DashboardEvent::ConfigSaved {
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string(),
                });
            Ok(Json(result))
        }
        Err(error) => {
            state.audit_log.record(
                "rest",
                "config_apply",
                "configuration",
                AuditOutcome::Failure,
                correlation,
                BTreeMap::from([("error_code".to_string(), error.code.to_string())]),
            );
            Err(error.into())
        }
    }
}

async fn restart_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Path(port): Path<String>,
) -> Result<Json<dto::ProxyActionResponse>, ApiError> {
    authorize(&headers, &state)?;
    // Parse the segment manually so a malformed port renders the shared
    // ApiErrorBody JSON instead of axum's plain-text path rejection.
    let port: u16 = port.trim().parse().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_proxy_port",
            "path parameter must be an integer proxy port between 1 and 65535",
        )
    })?;
    let correlation = request_id.map(|Extension(value)| value.0);
    match service::restart_managed_proxy(&state, port).await {
        Ok(proxy) => {
            state.audit_log.record(
                "rest",
                "proxy_restart",
                format!("proxy:{port}"),
                AuditOutcome::Success,
                correlation,
                BTreeMap::new(),
            );
            Ok(Json(dto::ProxyActionResponse {
                status: "ok".to_string(),
                proxy,
            }))
        }
        Err(error) => {
            state.audit_log.record(
                "rest",
                "proxy_restart",
                format!("proxy:{port}"),
                AuditOutcome::Failure,
                correlation,
                BTreeMap::from([("error_code".to_string(), error.code.to_string())]),
            );
            Err(error.into())
        }
    }
}

async fn drain_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Path(port): Path<String>,
) -> Result<Json<dto::ProxyDrainResponse>, ApiError> {
    set_proxy_drain(state, headers, request_id, port, true).await
}

async fn undrain_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Path(port): Path<String>,
) -> Result<Json<dto::ProxyDrainResponse>, ApiError> {
    set_proxy_drain(state, headers, request_id, port, false).await
}

async fn set_proxy_drain(
    state: AppState,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    port: String,
    draining: bool,
) -> Result<Json<dto::ProxyDrainResponse>, ApiError> {
    authorize(&headers, &state)?;
    let port: u16 = port.trim().parse().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_proxy_port",
            "path parameter must be an integer proxy port between 1 and 65535",
        )
    })?;
    let correlation = request_id.map(|Extension(value)| value.0);
    let action = if draining {
        "proxy_drain"
    } else {
        "proxy_undrain"
    };
    match service::set_managed_proxy_drain(&state, port, draining).await {
        Ok(proxy) => {
            state.audit_log.record(
                "rest",
                action,
                format!("proxy:{port}"),
                AuditOutcome::Success,
                correlation,
                BTreeMap::from([(
                    "active_requests".to_string(),
                    proxy.active_requests.to_string(),
                )]),
            );
            Ok(Json(dto::ProxyDrainResponse {
                status: "ok".to_string(),
                proxy,
            }))
        }
        Err(error) => {
            state.audit_log.record(
                "rest",
                action,
                format!("proxy:{port}"),
                AuditOutcome::Failure,
                correlation,
                BTreeMap::from([("error_code".to_string(), error.code.to_string())]),
            );
            Err(error.into())
        }
    }
}

async fn metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<dto::MetricsResponse>, ApiError> {
    authorize(&headers, &state)?;
    if !state.config.observability.metrics_enabled {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "metrics_disabled",
            "Metrics are disabled by configuration",
        ));
    }
    Ok(Json(dto::MetricsResponse {
        metrics: state.metrics.snapshot(),
        workers: state.workers.snapshot(),
    }))
}

async fn audit_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<dto::AuditEventsResponse>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(dto::AuditEventsResponse {
        events: state.audit_log.snapshot(100),
    }))
}

async fn openapi(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(openapi_document()))
}

fn response_schema<T: dto::ApiSchema>() -> Value {
    json!({
        "description": "Success",
        "content": {"application/json": {"schema": dto::schema_ref::<T>()}}
    })
}

fn request_schema<T: dto::ApiSchema>() -> Value {
    json!({
        "required": true,
        "content": {"application/json": {"schema": dto::schema_ref::<T>()}}
    })
}

fn openapi_document() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "OpenCode2API Management API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Versioned, authenticated management contract generated from shared DTO schemas."
        },
        "servers": [{"url":"/"}],
        "components": {
            "securitySchemes": {
                "bearerAuth": {"type":"http","scheme":"bearer","bearerFormat":"opaque"}
            },
            "schemas": dto::schema_components()
        },
        "security": [{"bearerAuth":[]}],
        "paths": {
            "/api/v1/status": {"get":{"summary":"Get bridge status","responses":{"200":response_schema::<dto::StatusResponse>(),"401":{"description":"Unauthorized"}}}},
            "/api/v1/proxies": {"get":{"summary":"List proxy state","responses":{"200":response_schema::<dto::ProxiesResponse>(),"401":{"description":"Unauthorized"}}}},
            "/api/v1/config": {"get":{"summary":"Get redacted resolved configuration","responses":{"200":response_schema::<dto::ConfigResponse>(),"401":{"description":"Unauthorized"}}}},
            "/api/v1/config/preview": {"post":{"summary":"Validate and preview an atomic config merge","requestBody":request_schema::<dto::ConfigDocumentRequest>(),"responses":{"200":response_schema::<dto::ConfigPreviewResponse>(),"400":{"description":"Invalid configuration"},"401":{"description":"Unauthorized"}}}},
            "/api/v1/config/apply": {"post":{"summary":"Atomically apply validated configuration","requestBody":request_schema::<dto::ConfigDocumentRequest>(),"responses":{"200":response_schema::<dto::ConfigApplyResponse>(),"400":{"description":"Invalid configuration"},"401":{"description":"Unauthorized"},"500":{"description":"Write or verification failure"}}}},
            "/api/v1/proxies/{port}/restart": {"post":{"summary":"Restart a managed primary proxy","parameters":[{"name":"port","in":"path","required":true,"schema":{"type":"integer","minimum":1,"maximum":65535}}],"responses":{"200":response_schema::<dto::ProxyActionResponse>(),"400":{"description":"Invalid port"},"401":{"description":"Unauthorized"},"403":{"description":"Protected proxy"},"404":{"description":"Not configured"},"409":{"description":"Proxy busy"},"502":{"description":"Container failure"}}}},
            "/api/v1/proxies/{port}/drain": {"post":{"summary":"Stop assigning fresh traffic to a managed primary while existing leases finish","parameters":[{"name":"port","in":"path","required":true,"schema":{"type":"integer","minimum":1,"maximum":65535}}],"responses":{"200":response_schema::<dto::ProxyDrainResponse>(),"400":{"description":"Invalid port"},"401":{"description":"Unauthorized"},"403":{"description":"Protected proxy"},"404":{"description":"Not configured"}}}},
            "/api/v1/proxies/{port}/undrain": {"post":{"summary":"Cancel a managed primary drain without changing health state","parameters":[{"name":"port","in":"path","required":true,"schema":{"type":"integer","minimum":1,"maximum":65535}}],"responses":{"200":response_schema::<dto::ProxyDrainResponse>(),"400":{"description":"Invalid port"},"401":{"description":"Unauthorized"},"403":{"description":"Protected proxy"},"404":{"description":"Not configured"}}}},
            "/api/v1/metrics": {"get":{"summary":"Get bounded in-process counters and worker state","responses":{"200":response_schema::<dto::MetricsResponse>(),"401":{"description":"Unauthorized"},"404":{"description":"Metrics disabled"}}}},
            "/api/v1/audit": {"get":{"summary":"Get recent secret-safe management audit events","responses":{"200":response_schema::<dto::AuditEventsResponse>(),"401":{"description":"Unauthorized"}}}},
            "/api/v1/openapi.json": {"get":{"summary":"Get this OpenAPI document","responses":{"200":{"description":"OpenAPI 3.1 document"},"401":{"description":"Unauthorized"}}}}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use crate::management::dto::ApiSchema;
    use crate::shell::ShellPolicy;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn state() -> AppState {
        AppState::new(BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            model: Some("test-model".to_string()),
            shell_policy: ShellPolicy::Disabled,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 64,
            tavily_api_key: Some("secret-that-must-not-leak".into()),
            primary_proxies: None,
            warm_standby_proxies: None,
            max_search_loops: 3,
            management: crate::config::ManagementConfig {
                rest_api_token: Some("test-rest-token".into()),
                ..BridgeConfig::default().management
            },
            ..Default::default()
        })
    }

    fn authorized(path: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer test-rest-token")
            .body(Body::empty())
            .unwrap()
    }

    /// Hybrid state with a configured primary pool. Identity endpoints are
    /// cleared so no background monitor probes external services from tests.
    fn hybrid_pool_state() -> AppState {
        AppState::new(BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            model: Some("test-model".to_string()),
            shell_policy: ShellPolicy::Disabled,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 64,
            primary_proxies: Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
            max_search_loops: 3,
            egress: crate::config::EgressConfig {
                mode: crate::config::EgressMode::Hybrid,
                identity_endpoints: Vec::new(),
                ..BridgeConfig::default().egress
            },
            management: crate::config::ManagementConfig {
                rest_api_token: Some("test-rest-token".into()),
                ..BridgeConfig::default().management
            },
            ..Default::default()
        })
    }

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn exit_identity(
        public_ip: &str,
        verified_at_unix_secs: u64,
    ) -> crate::proxy_pool::ExitIdentity {
        crate::proxy_pool::ExitIdentity {
            public_ip: public_ip.to_string(),
            provider: Some("cloudflare-warp".to_string()),
            colo: Some("SIN".to_string()),
            verified_at_unix_secs,
        }
    }

    async fn status_egress_json(state: AppState) -> Value {
        let app = router().with_state(state);
        let response = app.oneshot(authorized("/api/v1/status")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        body["egress"].clone()
    }

    #[tokio::test]
    async fn hybrid_status_derives_unique_verified_exits_from_pool() {
        let state = hybrid_pool_state();
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].exit_identity = Some(exit_identity("203.0.113.10", unix_now()));
        }
        let egress = status_egress_json(state).await;
        assert_eq!(egress["mode"], "hybrid");
        assert_eq!(
            egress["unique_verified_exits"], 1,
            "hybrid must report the real verified unique exit count, not a constant"
        );
    }

    #[tokio::test]
    async fn hybrid_status_ignores_stale_exit_identities() {
        let ttl_secs = BridgeConfig::default().egress.identity_ttl.as_secs();
        let state = hybrid_pool_state();
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].exit_identity = Some(exit_identity(
                "203.0.113.10",
                unix_now().saturating_sub(ttl_secs + 60),
            ));
        }
        let egress = status_egress_json(state).await;
        assert_eq!(
            egress["unique_verified_exits"], 0,
            "stale identities must not be reported as verified exits"
        );
    }

    #[tokio::test]
    async fn hybrid_status_keeps_gateway_ready_without_verified_exits() {
        let egress = status_egress_json(hybrid_pool_state()).await;
        assert_eq!(egress["mode"], "hybrid");
        assert_eq!(
            egress["ready"], true,
            "hybrid stays gateway-ready via the direct fallback even with no proxy evidence"
        );
        assert_eq!(egress["unique_verified_exits"], 0);
    }

    #[tokio::test]
    async fn hybrid_status_reports_shared_subsystem_and_drain_aware_active_route() {
        let mut state = hybrid_pool_state();
        state.proxy_subsystem = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::proxy_pool::ProxySubsystemStatus::starting(),
        ));
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].health = crate::proxy_pool::HealthState::Healthy;
            pool.proxies[0].circuit = crate::proxy_pool::CircuitState::Closed;
            pool.proxies[0].exit_identity = Some(exit_identity("203.0.113.10", unix_now()));
        }
        state.proxy_subsystem.write().await.mark_ready();
        let egress = status_egress_json(state.clone()).await;
        assert_eq!(egress["active_route"], "proxy");
        assert_eq!(egress["proxy_subsystem"]["phase"], "ready");
        assert_eq!(egress["minimum_unique_exit_ips"], 1);

        state.proxy_pool.write().await.begin_drain(0).unwrap();
        let egress = status_egress_json(state).await;
        assert_eq!(egress["ready"], true, "hybrid gateway remains direct-ready");
        assert_eq!(egress["active_route"], "direct");
        assert_eq!(egress["proxy_pool"]["nodes"][0]["draining"], true);
    }

    #[tokio::test]
    async fn proxy_drain_round_trips_and_is_visible_in_snapshot() {
        let state = hybrid_pool_state();
        let app = router().with_state(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/proxies/40001/drain")
                    .header(header::AUTHORIZATION, "Bearer test-rest-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["proxy"]["draining"], true);
        assert_eq!(body["proxy"]["action"], "drain");
        assert!(state.proxy_pool.read().await.proxies[0].draining);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/proxies/40001/undrain")
                    .header(header::AUTHORIZATION, "Bearer test-rest-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!state.proxy_pool.read().await.proxies[0].draining);
    }

    #[tokio::test]
    async fn management_json_surfaces_send_no_store() {
        // Status/config payloads change with pool and config state; cached
        // copies mislead operators and dashboards about live topology.
        let app = router().with_state(state());
        for path in [
            "/api/v1/status",
            "/api/v1/proxies",
            "/api/v1/config",
            "/api/v1/openapi.json",
        ] {
            let response = app.clone().oneshot(authorized(path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["cache-control"],
                "no-store",
                "{path} must send Cache-Control: no-store"
            );
        }
    }

    #[tokio::test]
    async fn malformed_path_port_returns_typed_json_error() {
        // A non-numeric port segment must yield the shared ApiErrorBody shape
        // instead of axum's plain-text rejection, so machine clients can read
        // a stable error code.
        let response = router()
            .with_state(state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/proxies/abc/restart")
                    .header(header::AUTHORIZATION, "Bearer test-rest-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "invalid_proxy_port");
    }

    #[tokio::test]
    async fn malformed_preview_body_returns_typed_json_error() {
        // Malformed JSON keeps 400; a missing content-type keeps 415 — but
        // both must render the typed ApiErrorBody instead of plain text.
        for (content_type, expected_status) in [
            (Some("application/json"), StatusCode::BAD_REQUEST),
            (None, StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ] {
            let mut builder = Request::builder()
                .method("POST")
                .uri("/api/v1/config/preview");
            if let Some(value) = content_type {
                builder = builder.header(header::CONTENT_TYPE, value);
            }
            let response = router()
                .with_state(state())
                .oneshot(
                    builder
                        .header(header::AUTHORIZATION, "Bearer test-rest-token")
                        .body(Body::from("{ not json"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected_status);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], "invalid_request_body");
        }
    }

    #[tokio::test]
    async fn openapi_requires_auth_and_references_registered_dtos() {
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
        let response = app
            .oneshot(authorized("/api/v1/openapi.json"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(body["components"]["schemas"][dto::StatusResponse::NAME].is_object());
        assert_eq!(
            body["paths"]["/api/v1/metrics"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/MetricsResponse"
        );
        assert_eq!(
            body["paths"]["/api/v1/audit"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/AuditEventsResponse"
        );
    }

    #[test]
    fn openapi_covers_every_registered_management_path() {
        let document = openapi_document();
        let paths = document["paths"]
            .as_object()
            .expect("OpenAPI paths must be an object");
        let documented = paths
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let registered = MANAGEMENT_PATHS
            .iter()
            .map(|(_, documented)| *documented)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            documented, registered,
            "runtime management routes and OpenAPI paths drifted"
        );
    }

    #[tokio::test]
    async fn protected_proxy_restart_returns_forbidden_without_touching_docker() {
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
    }

    #[tokio::test]
    async fn failed_management_mutation_is_recorded_without_secret_content() {
        let state = state();
        let app = router().with_state(state.clone());
        let response = app
            .clone()
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

        let response = app.oneshot(authorized("/api/v1/audit")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["events"][0]["action"], "proxy_restart");
        assert_eq!(body["events"][0]["outcome"], "failure");
        let encoded = serde_json::to_string(&body).unwrap();
        assert!(!encoded.contains("test-rest-token"));
        assert!(!encoded.contains("secret-that-must-not-leak"));
    }

    #[tokio::test]
    async fn metrics_endpoint_is_typed_and_authenticated() {
        let app = router().with_state(state());
        let response = app.oneshot(authorized("/api/v1/metrics")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(body["metrics"]["requests_total"].is_number());
        assert!(body["workers"]["workers"].is_array());
    }
}
