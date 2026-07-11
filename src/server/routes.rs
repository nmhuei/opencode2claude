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
        .merge(anthropic_routes(&state))
        .merge(dashboard_routes())
        .merge(rest_api::router())
        .with_state(state)
}

fn anthropic_routes(state: &AppState) -> Router<AppState> {
    let routes = Router::new()
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
}
