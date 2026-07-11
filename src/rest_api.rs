//! Versioned REST management API backed by shared typed DTOs.

use crate::audit::AuditOutcome;
use crate::management::{auth, config_apply, dto, service};
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/proxies", get(proxies))
        .route("/api/v1/config", get(config))
        .route("/api/v1/config/preview", post(preview_config))
        .route("/api/v1/config/apply", post(apply_config))
        .route("/api/v1/proxies/:port/restart", post(restart_proxy))
        .route("/api/v1/metrics", get(metrics))
        .route("/api/v1/audit", get(audit_events))
        .route("/api/v1/openapi.json", get(openapi))
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

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<dto::StatusResponse>, ApiError> {
    authorize(&headers, &state)?;
    let snapshot = service::proxy_snapshot(&state).await;
    let workers = state.workers.snapshot();
    let (egress_mode, egress_ready, unique_verified_exits) = {
        let pool = state.proxy_pool.read().await;
        match state.config.egress.mode {
            crate::config::EgressMode::Direct => ("direct".to_string(), true, 0),
            crate::config::EgressMode::Proxy => (
                "proxy".to_string(),
                pool.egress_ready(
                    state.config.egress.minimum_unique_exit_ips,
                    state.config.egress.identity_ttl,
                ),
                pool.verified_unique_exit_count_fresh(state.config.egress.identity_ttl),
            ),
        }
    };
    Ok(Json(dto::StatusResponse {
        status: "ok".to_string(),
        service: "opencode2api".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: service::uptime_secs(&state),
        model: state.config.model.clone(),
        bridge: dto::BridgeSummary {
            host: state.config.host.to_string(),
            port: state.config.bridge_port,
            client_auth_enabled: state.config.auth_enabled(),
        },
        egress: dto::EgressSummary {
            mode: egress_mode,
            ready: egress_ready,
            unique_verified_exits,
            proxy_pool: snapshot,
        },
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
    Ok(Json(service::safe_config_snapshot(&state).into()))
}

async fn preview_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<dto::ConfigDocumentRequest>,
) -> Result<Json<dto::ConfigPreviewResponse>, ApiError> {
    authorize(&headers, &state)?;
    let plan = config_apply::preview_config(&state, &request.content)?;
    Ok(Json(config_apply::preview_response(&plan)))
}

async fn apply_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<dto::ConfigDocumentRequest>,
) -> Result<Json<dto::ConfigApplyResponse>, ApiError> {
    authorize(&headers, &state)?;
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
    Path(port): Path<u16>,
) -> Result<Json<dto::ProxyActionResponse>, ApiError> {
    authorize(&headers, &state)?;
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
