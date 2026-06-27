//! OpenCode2Claude — A blazing-fast API bridge connecting Claude Code to any LLM.
//!
//! This binary provides a local HTTP server that translates Anthropic API requests
//! into OpenCode CLI commands, enabling Claude Code to use any LLM provider.

mod config;
mod error;
mod handlers;
mod middleware;
mod opencode;
mod proxy_pool;
mod shell;
mod sse;
mod state;

use clap::Parser;
use config::BridgeConfig;
use state::AppState;

use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Command-line arguments for the OpenCode2Claude bridge.
#[derive(Parser)]
#[command(
    name = "opencode2claude",
    about = "A blazing-fast API bridge connecting Claude Code to OpenCode CLI and any LLM"
)]
struct Cli {
    /// Override bridge port
    #[arg(short = 'p', long)]
    port: Option<u16>,

    /// Override bind address
    #[arg(long)]
    host: Option<String>,

    /// Path to custom TOML config file (default: opencode2claude.toml)
    #[arg(short = 'c', long)]
    config: Option<String>,

    /// Override model
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Override shell policy (disabled, allowlist, unrestricted)
    #[arg(long = "shell-policy")]
    shell_policy: Option<String>,

    /// Print version and exit
    #[arg(short = 'v', long = "version")]
    version: bool,

    /// Tavily search API key override
    #[arg(long)]
    tavily_api_key: Option<String>,

    /// Exa search API key override
    #[arg(long)]
    exa_api_key: Option<String>,

    /// Serper.dev search API key override
    #[arg(long)]
    serper_api_key: Option<String>,

    /// SearXNG instance URL override
    #[arg(long)]
    searxng_url: Option<String>,

    /// SearXNG API key override
    #[arg(long)]
    searxng_api_key: Option<String>,
}

#[tokio::main]
async fn main() {
    // Parse CLI arguments (before logging, so version flag works cleanly)
    let cli = Cli::parse();

    // Handle version flag
    if cli.version {
        println!("opencode2claude v{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Initialize structured logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration with priority: CLI > Env > TOML > Defaults
    let overrides = config::CliOverrides {
        bridge_port: cli.port,
        host: cli.host,
        model: cli.model,
        shell_policy: cli.shell_policy,
        config_path: cli.config,
        tavily_api_key: cli.tavily_api_key,
        exa_api_key: cli.exa_api_key,
        serper_api_key: cli.serper_api_key,
        searxng_url: cli.searxng_url,
        searxng_api_key: cli.searxng_api_key,
    };
    let config = BridgeConfig::from_env_and_cli(overrides);
    let addr = SocketAddr::from((config.host, config.bridge_port));
    let max_body = config.max_body_size;

    // Print startup banner
    info!("╔══════════════════════════════════════════════╗");
    info!(
        "║     OpenCode2Claude Bridge v{}          ║",
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

    // Create shared application state
    let state = AppState::new(config);

    // Build router — apply auth middleware only to API routes, not /health
    let app = Router::new()
        .route("/v1/messages", post(handlers::handle_messages))
        .route("/v1/models", get(handlers::handle_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        .route("/health", get(handlers::handle_health))
        .layer(RequestBodyLimitLayer::new(max_body))
        .with_state(state);

    // Bind listener with proper error handling
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ Failed to bind to {}: {}", addr, e);
            eprintln!(
                "   Hint: Is another process using port {}? Try: lsof -i :{}",
                addr.port(),
                addr.port()
            );
            std::process::exit(1);
        }
    };

    info!("Server started successfully. Waiting for requests...");

    // Serve with graceful shutdown on SIGTERM/SIGINT
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| {
            eprintln!("❌ Server error: {}", e);
            std::process::exit(1);
        });

    info!("Server shut down gracefully.");
}

/// Wait for SIGINT (Ctrl+C) or SIGTERM for graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("Received SIGINT, shutting down..."); },
        _ = terminate => { info!("Received SIGTERM, shutting down..."); },
    }
}
