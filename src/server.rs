//! HTTP bridge server engine running logic.

use crate::config::{self, BridgeConfig};
use crate::dashboard;
use crate::handlers;
use crate::middleware;
use crate::rest_api;
use crate::state::AppState;
use crate::tui;
use axum::extract::DefaultBodyLimit;
use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use yansi::Paint;

/// Command-line arguments mapping for the server loop.
#[derive(Default, Debug, Clone)]
pub struct ServeArgsBridge {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub config: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub max_body_size: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
}

/// Runs the HTTP bridge server (foreground).
pub async fn run_server(args: ServeArgsBridge) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let overrides = config::CliOverrides {
        bridge_port: args.port,
        host: args.host,
        model: args.model,
        shell_policy: args.shell_policy,
        config_path: args.config,
        max_body_size: args.max_body_size,
        tavily_api_key: args.tavily_api_key,
        exa_api_key: args.exa_api_key,
        serper_api_key: args.serper_api_key,
        searxng_url: args.searxng_url,
        searxng_api_key: args.searxng_api_key,
    };
    let config = BridgeConfig::from_env_and_cli(overrides);
    let addr = SocketAddr::from((config.host, config.bridge_port));

    if let Err(err) = config.validate_security() {
        eprintln!("{}", err);
        std::process::exit(1);
    }

    let max_body = config.max_body_size;

    info!("╔══════════════════════════════════════════════╗");
    info!(
        "║     OpenCode2API Bridge v{}             ║",
        env!("CARGO_PKG_VERSION")
    );
    info!("╠══════════════════════════════════════════════╣");
    info!(
        "{}",
        tui::box_line("║  Bridge:  http://", &addr.to_string(), 48)
    );
    info!(
        "║  Daemon:  port {}                          ║",
        config.opencode_port
    );
    info!(
        "{}",
        tui::box_line(
            "║  Model:   ",
            config.model.as_deref().unwrap_or("(auto)"),
            48
        )
    );
    info!(
        "{}",
        tui::box_line("║  Shell:   ", &config.shell_policy.description(), 48)
    );
    let dashboard_url = format!("http://{}/dashboard", addr);
    info!("{}", tui::box_line("║  Dashboard: ", &dashboard_url, 48));
    info!(
        "{}",
        tui::box_line(
            "║  Auth:    ",
            if config.auth_enabled() {
                "enabled"
            } else {
                "disabled"
            },
            48
        )
    );
    info!("╚══════════════════════════════════════════════╝");
    info!("To use: export ANTHROPIC_BASE_URL=\"http://{}/v1\"", addr);

    let state = AppState::new(config);

    // Spawn dashboard heartbeat task (every 30s)
    dashboard::spawn_heartbeat(state.event_tx.clone());

    // API routes (v1 Messages API) — protected by auth middleware + body limit
    let mut api_routes = Router::new()
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

    if max_body > 0 {
        api_routes = api_routes
            .layer(DefaultBodyLimit::max(max_body))
            .layer(RequestBodyLimitLayer::new(max_body));
    } else {
        info!("Request body limit disabled (max_body_size=0)");
        api_routes = api_routes.layer(DefaultBodyLimit::disable());
    }

    // Dashboard + health routes — no body limit, no auth middleware
    let dashboard_routes = Router::new()
        .route("/", get(dashboard::serve_landing))
        .route("/health", get(handlers::handle_health))
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
        );

    let app = Router::new()
        .merge(api_routes)
        .merge(dashboard_routes)
        .merge(rest_api::router())
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{} Failed to bind to {}: {}", "✗".red().bold(), addr, e);
            eprintln!(
                "   Hint: Is another process using port {}? Try: lsof -i :{}",
                addr.port(),
                addr.port()
            );
            std::process::exit(1);
        }
    };

    info!("Server started successfully. Waiting for requests...");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| {
            eprintln!("{} Server error: {}", "✗".red().bold(), e);
            std::process::exit(1);
        });

    info!("Server shut down gracefully.");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C, shutting down...");
        },
        _ = terminate => {
            info!("Received SIGTERM, shutting down...");
        },
    }
}
