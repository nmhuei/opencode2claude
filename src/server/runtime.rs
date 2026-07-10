//! Foreground server lifecycle.

use super::{build_router, ServeArgsBridge};
use crate::config::BridgeConfig;
use crate::dashboard;
use crate::state::AppState;
use crate::tui;
use std::net::SocketAddr;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use yansi::Paint;

pub async fn run_server(args: ServeArgsBridge) {
    init_tracing();

    let config = BridgeConfig::from_env_and_cli(args.into());
    if let Err(error) = config.validate_security() {
        eprintln!("{error}");
        std::process::exit(1);
    }

    let address = SocketAddr::from((config.host, config.bridge_port));
    log_startup(&config, address);

    let state = AppState::new(config);
    dashboard::spawn_heartbeat(state.event_tx.clone());
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "{} Failed to bind to {}: {}",
                "✗".red().bold(),
                address,
                error
            );
            eprintln!(
                "   Hint: Is another process using port {}? Try: lsof -i :{}",
                address.port(),
                address.port()
            );
            std::process::exit(1);
        });

    info!(%address, "server started");
    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(termination_signal())
        .await
    {
        eprintln!("{} Server error: {}", "✗".red().bold(), error);
        std::process::exit(1);
    }
    info!("server stopped gracefully");
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

fn log_startup(config: &BridgeConfig, address: SocketAddr) {
    info!("╔══════════════════════════════════════════════╗");
    info!(
        "║     OpenCode2API Bridge v{}             ║",
        env!("CARGO_PKG_VERSION")
    );
    info!("╠══════════════════════════════════════════════╣");
    info!(
        "{}",
        tui::box_line("║  Bridge:  http://", &address.to_string(), 48)
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
    info!(
        "{}",
        tui::box_line("║  Dashboard: ", &format!("http://{address}/dashboard"), 48)
    );
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
    info!("To use: export ANTHROPIC_BASE_URL=\"http://{address}/v1\"");
}

async fn termination_signal() {
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
        _ = ctrl_c => info!("received Ctrl+C; stopping"),
        _ = terminate => info!("received SIGTERM; stopping"),
    }
}
