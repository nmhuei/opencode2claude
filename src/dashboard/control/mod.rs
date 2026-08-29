//! Authenticated dashboard control-plane endpoints.

mod access;
mod api_keys;
mod catalog;
mod history;
mod network;
mod system;

pub use access::{
    handler_audit, handler_client_config, handler_completion, handler_config_init,
    handler_config_template, handler_doctor, handler_environment, handler_metrics,
};
pub use api_keys::{
    handler_api_key_detail, handler_api_keys, handler_delete_api_key, handler_generate_keys,
    handler_revoke_keys, handler_rotate_api_key, handler_update_api_key, handler_verify_api_key,
};
pub use catalog::{handler_capabilities, handler_models, handler_select_model};
pub use history::{
    handler_history_content, handler_history_delete, handler_history_detail,
    handler_history_export, handler_history_list, handler_history_purge, handler_history_settings,
    handler_history_settings_update, handler_history_stats,
};
pub use network::{
    handler_proxy_logs, handler_proxy_plan, handler_proxy_purge, handler_proxy_restart_all,
};
pub use system::{
    handler_server_logs, handler_server_restart, handler_server_stop, handler_update_apply,
    handler_update_check,
};

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use serde_json::{json, Value};

pub(super) fn dashboard_error(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({"status":"error","message":message.into()})),
    )
}

/// Stamp `Cache-Control: no-store` onto every `/api/dashboard/control/*`
/// response.
///
/// Control-plane payloads embed live usage counters, key fingerprints, audit
/// trails, and registry paths; a shared or browser cache serving a stale copy
/// misleads operators about credential state. Mirrors the layer already
/// guarding `/api/v1/*` in `rest_api.rs`. Scoped by request path so the
/// router-level attachment in `dashboard_routes()` (src/server/routes.rs)
/// leaves dashboard assets, SSE event streams, and health surfaces untouched.
pub async fn no_store_middleware(request: Request, next: Next) -> Response {
    let is_control = request.uri().path().starts_with("/api/dashboard/control/");
    let mut response = next.run(request).await;
    if is_control {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod no_store_tests {
    //! Executable spec for `no_store_middleware` (Deliverable 3): every JSON
    //! response of the `/api/dashboard/control/*` surface must carry
    //! `Cache-Control: no-store` — success AND error statuses alike — because
    //! these payloads embed live usage counters, fingerprints, and registry
    //! paths that cached proxies would serve stale. The production attach
    //! point is `dashboard_routes()` in `src/server/routes.rs`.
    use super::*;
    use crate::config::{BridgeConfig, ManagementConfig, RuntimeConfig};
    use crate::docker::DockerCliRuntime;
    use crate::infrastructure::file_store::AtomicFileStore;
    use crate::infrastructure::warp::CliWarpController;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{header, HeaderValue, Request};
    use axum::routing::get;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "oc2api-control-nostore-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn state(root: &std::path::Path) -> AppState {
        let config = BridgeConfig {
            primary_proxies: None,
            warm_standby_proxies: None,
            runtime: RuntimeConfig {
                runtime_dir: Some(root.join("runtime")),
                ..BridgeConfig::default().runtime
            },
            management: ManagementConfig {
                config_path: root.join("config.toml"),
                dashboard_token: Some("dash-token".to_string().into()),
                ..BridgeConfig::default().management
            },
            ..Default::default()
        };
        AppState::new_with_infrastructure(
            config,
            Arc::new(DockerCliRuntime::from_config(&BridgeConfig::default())),
            Arc::new(CliWarpController::new("warp-cli")),
            Arc::new(AtomicFileStore),
        )
    }

    /// The exact composition `dashboard_routes()` must apply once wired.
    fn control_router(state: AppState) -> axum::Router {
        axum::Router::new()
            .route(
                "/api/dashboard/control/api-keys",
                get(api_keys::handler_api_keys),
            )
            .route(
                "/api/dashboard/control/api-keys/:id",
                get(api_keys::handler_api_key_detail),
            )
            .route(
                "/api/dashboard/control/capabilities",
                get(catalog::handler_capabilities),
            )
            // Out-of-scope neighbor proving the guard is path-scoped and does
            // not stamp unrelated surfaces.
            .route("/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(no_store_middleware))
            .with_state(state)
    }

    async fn cache_control(app: axum::Router, request: Request<Body>) -> (u16, Option<String>) {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let value = response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|value| value.to_str().unwrap().to_string());
        (status, value)
    }

    fn request(method: &str, uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(
                "x-dashboard-token",
                HeaderValue::from_str(token).expect("static test token"),
            );
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn success_payloads_send_no_store() {
        let root = temp_root("success");
        std::fs::create_dir_all(&root).unwrap();
        let app = control_router(state(&root));

        let (list_status, list_header) = cache_control(
            app.clone(),
            request("GET", "/api/dashboard/control/api-keys", Some("dash-token")),
        )
        .await;
        assert_eq!(list_status, 200);
        assert_eq!(list_header.as_deref(), Some("no-store"));

        let (cap_status, cap_header) = cache_control(
            app.clone(),
            request(
                "GET",
                "/api/dashboard/control/capabilities",
                Some("dash-token"),
            ),
        )
        .await;
        assert_eq!(cap_status, 200);
        assert_eq!(cap_header.as_deref(), Some("no-store"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn error_payloads_send_no_store_too() {
        let root = temp_root("errors");
        std::fs::create_dir_all(&root).unwrap();
        let app = control_router(state(&root));

        let (unauthorized_status, unauthorized_header) = cache_control(
            app.clone(),
            request("GET", "/api/dashboard/control/api-keys", None),
        )
        .await;
        assert_eq!(unauthorized_status, 401);
        assert_eq!(unauthorized_header.as_deref(), Some("no-store"));

        let (missing_status, missing_header) = cache_control(
            app,
            request(
                "GET",
                "/api/dashboard/control/api-key-does-not-exist",
                Some("dash-token"),
            ),
        )
        .await;
        assert_eq!(missing_status, 404);
        assert_eq!(missing_header.as_deref(), Some("no-store"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn non_control_surfaces_are_left_untouched() {
        let root = temp_root("scope");
        std::fs::create_dir_all(&root).unwrap();
        let app = control_router(state(&root));

        let (_, health_header) = cache_control(app, request("GET", "/health", None)).await;
        assert_eq!(
            health_header, None,
            "the guard must stay scoped to /api/dashboard/control/*"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
