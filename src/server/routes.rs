//! HTTP route composition.

use crate::state::AppState;
use crate::{dashboard, handlers, middleware, rest_api};
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(llm_routes(&state))
        .merge(dashboard_routes())
        .merge(rest_api::router())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::observability::request_observability_middleware,
        ))
        .with_state(state)
}

fn llm_routes(state: &AppState) -> Router<AppState> {
    let anthropic = Router::new()
        .route("/v1/messages", post(handlers::handle_messages))
        .route(
            "/v1/messages/count_tokens",
            post(handlers::handle_count_tokens),
        )
        .route("/v1/models", get(handlers::handle_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ));

    let openai = Router::new()
        .route(
            "/v1/chat/completions",
            post(handlers::handle_chat_completions),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::openai_auth_middleware,
        ));

    let routes = anthropic.merge(openai);
    if state.config.max_body_size == 0 {
        info!("request body limit disabled");
        routes.layer(DefaultBodyLimit::disable())
    } else {
        routes
            .layer(DefaultBodyLimit::max(state.config.max_body_size))
            .layer(RequestBodyLimitLayer::new(state.config.max_body_size))
    }
}

fn dashboard_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(dashboard::serve_landing))
        .route("/health", get(handlers::handle_health))
        .route("/health/live", get(handlers::handle_liveness))
        .route("/health/ready", get(handlers::handle_readiness))
        .route("/dashboard", get(dashboard::serve_webui))
        .route("/dashboard/", get(dashboard::serve_webui))
        .route("/dashboard/*path", get(dashboard::serve_webui))
        .route("/api/dashboard/status", get(dashboard::handler_rest_status))
        .route("/api/dashboard/proxies", get(dashboard::handler_proxies))
        .route("/api/dashboard/config", get(dashboard::handler_config))
        .route(
            "/api/dashboard/config/raw",
            get(dashboard::handler_config_raw),
        )
        .route("/api/dashboard/login", post(dashboard::handler_login))
        .route("/api/dashboard/logout", post(dashboard::handler_logout))
        .route(
            "/api/dashboard/auth/status",
            get(dashboard::handler_auth_status),
        )
        .route(
            "/api/dashboard/diagnostics",
            get(dashboard::handler_dashboard_diagnostics),
        )
        .route(
            "/api/dashboard/config/preview",
            post(dashboard::handler_config_preview),
        )
        .route(
            "/api/dashboard/config/save",
            post(dashboard::handler_config_save),
        )
        .route("/api/dashboard/events", get(dashboard::handler_events))
        .route(
            "/api/dashboard/test/stream",
            get(dashboard::handler_test_stream_get).post(dashboard::handler_test_stream_post),
        )
        .route(
            "/api/dashboard/proxy/:port/restart",
            post(dashboard::handler_proxy_restart),
        )
        .route(
            "/api/dashboard/proxy/:port/drain",
            post(dashboard::handler_proxy_drain),
        )
        .route(
            "/api/dashboard/proxy/:port/undrain",
            post(dashboard::handler_proxy_undrain),
        )
        .route(
            "/api/dashboard/control/capabilities",
            get(dashboard::handler_capabilities),
        )
        .route(
            "/api/dashboard/control/models",
            get(dashboard::handler_models),
        )
        .route(
            "/api/dashboard/control/models/select",
            post(dashboard::handler_select_model),
        )
        .route(
            "/api/dashboard/control/doctor",
            get(dashboard::handler_doctor),
        )
        .route(
            "/api/dashboard/control/metrics",
            get(dashboard::handler_metrics),
        )
        .route(
            "/api/dashboard/control/audit",
            get(dashboard::handler_audit),
        )
        .route(
            "/api/dashboard/control/env",
            get(dashboard::handler_environment),
        )
        .route(
            "/api/dashboard/control/api-keys",
            get(dashboard::handler_api_keys).post(dashboard::handler_generate_keys),
        )
        .route(
            "/api/dashboard/control/api-keys/verify",
            post(dashboard::handler_verify_api_key),
        )
        .route(
            "/api/dashboard/control/api-keys/revoke",
            post(dashboard::handler_revoke_keys),
        )
        .route(
            "/api/dashboard/control/api-keys/:id",
            get(dashboard::handler_api_key_detail)
                .patch(dashboard::handler_update_api_key)
                .delete(dashboard::handler_delete_api_key),
        )
        .route(
            "/api/dashboard/control/api-keys/:id/rotate",
            post(dashboard::handler_rotate_api_key),
        )
        .route(
            "/api/dashboard/control/client-config",
            post(dashboard::handler_client_config),
        )
        .route(
            "/api/dashboard/control/history",
            get(dashboard::handler_history_list),
        )
        .route(
            "/api/dashboard/control/history/stats",
            get(dashboard::handler_history_stats),
        )
        .route(
            "/api/dashboard/control/history/settings",
            get(dashboard::handler_history_settings)
                .patch(dashboard::handler_history_settings_update),
        )
        .route(
            "/api/dashboard/control/history/export",
            post(dashboard::handler_history_export),
        )
        .route(
            "/api/dashboard/control/history/purge",
            post(dashboard::handler_history_purge),
        )
        .route(
            "/api/dashboard/control/history/:id/content/:kind",
            get(dashboard::handler_history_content),
        )
        .route(
            "/api/dashboard/control/history/:id",
            get(dashboard::handler_history_detail).delete(dashboard::handler_history_delete),
        )
        .route(
            "/api/dashboard/control/server/logs",
            get(dashboard::handler_server_logs),
        )
        .route(
            "/api/dashboard/control/server/restart",
            post(dashboard::handler_server_restart),
        )
        .route(
            "/api/dashboard/control/server/stop",
            post(dashboard::handler_server_stop),
        )
        .route(
            "/api/dashboard/control/completions/:shell",
            get(dashboard::handler_completion),
        )
        .route(
            "/api/dashboard/control/config/template",
            get(dashboard::handler_config_template),
        )
        .route(
            "/api/dashboard/control/config/init",
            post(dashboard::handler_config_init),
        )
        .route(
            "/api/dashboard/control/proxies/plan",
            get(dashboard::handler_proxy_plan),
        )
        .route(
            "/api/dashboard/control/proxies/restart",
            post(dashboard::handler_proxy_restart_all),
        )
        .route(
            "/api/dashboard/control/proxies/purge",
            post(dashboard::handler_proxy_purge),
        )
        .route(
            "/api/dashboard/control/proxies/logs",
            get(dashboard::handler_proxy_logs),
        )
        .route(
            "/api/dashboard/control/update/check",
            get(dashboard::handler_update_check),
        )
        .route(
            "/api/dashboard/control/update/apply",
            post(dashboard::handler_update_apply),
        )
}
