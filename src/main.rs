//! OpenCode2Claude — A blazing-fast API bridge connecting Claude Code to any LLM.
//!
//! This binary provides a local HTTP server that translates Anthropic API requests
//! into OpenCode CLI commands, enabling Claude Code to use any LLM provider.

mod cli;
mod config;
mod error;
mod handlers;
mod middleware;
mod opencode;
mod pidfile;
mod proxy_pool;
mod runtime;
mod shell;
mod sse;
mod state;
mod supervisor;

use clap::Parser;
use cli::{Command, ServeArgs, StartArgs, StatusArgs, StopArgs};
use config::BridgeConfig;
use state::AppState;

use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();

    match cli.command {
        Some(Command::Serve(args)) => run_server(args).await,
        None => run_server(ServeArgs::default()).await,
        Some(Command::Start(args)) => cmd_start(args),
        Some(Command::Status(args)) => cmd_status(args),
        Some(Command::Stop(args)) => cmd_stop(args),
        Some(_) => {
            println!("Subcommand not yet implemented (coming in Phase 3+)");
        }
    }
}

fn resolve_runtime(args: &StartArgs) -> supervisor::Supervisor {
    let port = args
        .port
        .or_else(|| {
            std::env::var("BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(config::DEFAULT_BRIDGE_PORT);
    let host = args
        .host
        .clone()
        .or_else(|| std::env::var("BRIDGE_HOST").ok())
        .unwrap_or_else(|| config::DEFAULT_HOST.to_string());
    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let paths = runtime::RuntimePaths::new(root);
    supervisor::Supervisor::new(paths, port, host)
}

fn cmd_start(args: StartArgs) {
    let sup = resolve_runtime(&args);
    match sup.start() {
        Ok(()) => {
            let status = sup
                .status()
                .unwrap_or(supervisor::SupervisorStatus::Stopped);
            println!("Bridge started. {}", status);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_status(args: StatusArgs) {
    let sup = resolve_runtime(&args);
    match sup.status() {
        Ok(status) => println!("Bridge: {}", status),
        Err(e) => println!("Bridge: Error — {}", e),
    }
}

fn cmd_stop(args: StopArgs) {
    let sup = resolve_runtime(&args);
    match sup.stop() {
        Ok(()) => println!("Bridge stopped."),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

async fn run_server(args: ServeArgs) {
    // Initialize structured logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration with priority: CLI > Env > TOML > Defaults
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
