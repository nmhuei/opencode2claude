//! HTTP bridge server engine running logic.

use crate::config::{self, BridgeConfig};
use crate::dashboard;
use crate::handlers;
use crate::middleware;
use crate::state::AppState;
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
        "║  Bridge:  http://{}{}║",
        addr,
        " ".repeat(27usize.saturating_sub(addr.to_string().len()))
    );
    info!(
        "║  Daemon:  port {}                          ║",
        config.opencode_port
    );
    info!(
        "║  Model:   {}{}║",
        config.model.as_deref().unwrap_or("(auto)"),
        " ".repeat(33usize.saturating_sub(config.model.as_deref().unwrap_or("(auto)").len()))
    );
    info!(
        "║  Shell:   {}{}║",
        config.shell_policy.description(),
        " ".repeat(33usize.saturating_sub(config.shell_policy.description().len()))
    );
    let dashboard_url = format!("http://{}/dashboard", addr);
    info!(
        "║  Dashboard: {}{}║",
        dashboard_url,
        " ".repeat(27usize.saturating_sub(dashboard_url.len()))
    );
    info!(
        "║  Auth:    {}{}║",
        if config.auth_enabled() {
            "enabled"
        } else {
            "disabled"
        },
        " ".repeat(
            33usize.saturating_sub(
                if config.auth_enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
                .len()
            )
        )
    );
    info!("╚══════════════════════════════════════════════╝");
    info!("To use: export ANTHROPIC_BASE_URL=\"http://{}/v1\"", addr);

    let state = AppState::new(config);

    // Spawn dashboard heartbeat task (every 30s)
    dashboard::spawn_heartbeat(state.event_tx.clone());

    let app = Router::new()
        .route("/v1/messages", post(handlers::handle_messages))
        .route(
            "/v1/messages/count_tokens",
            post(handlers::handle_count_tokens),
        )
        .route("/v1/models", get(handlers::handle_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        .route("/", get(dashboard::serve_landing))
        .route("/health", get(handlers::handle_health))
        // Dashboard routes (no auth middleware)
        .route("/dashboard", get(dashboard::serve_webui))
        .route("/dashboard/", get(dashboard::serve_webui))
        .route("/dashboard/*path", get(dashboard::serve_webui))
        .route("/api/dashboard/status", get(dashboard::handler_rest_status))
        .route("/api/dashboard/proxies", get(dashboard::handler_proxies))
        .route("/api/dashboard/config", get(dashboard::handler_config))
        .route("/api/dashboard/login", post(dashboard::handler_login))
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
            "/api/dashboard/proxy/:port/restart",
            post(dashboard::handler_proxy_restart),
        )
        .layer(RequestBodyLimitLayer::new(max_body))
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
