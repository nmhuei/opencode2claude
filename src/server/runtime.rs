//! HTTP server lifecycle with owned workers and bounded graceful shutdown.

use super::args::ServeArgsBridge;
use super::routes::build_router;
use crate::config::{self, BridgeConfig};
use crate::dashboard;
use crate::state::AppState;
use crate::workers::WorkerShutdownError;
use std::future::IntoFuture;
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("invalid server configuration: {0}")]
    Configuration(String),
    #[error("failed to bind {address}: {source}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("HTTP server failed: {0}")]
    Serve(#[from] std::io::Error),
    #[error("graceful HTTP shutdown exceeded {0:?}")]
    GracefulTimeout(std::time::Duration),
    #[error(transparent)]
    WorkerShutdown(#[from] WorkerShutdownError),
}

pub async fn run_server(args: ServeArgsBridge) -> Result<(), ServerError> {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .try_init();

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
        egress_mode: args.egress_mode,
    };
    let config = BridgeConfig::from_env_and_cli(overrides);
    config
        .validate_security()
        .map_err(ServerError::Configuration)?;
    let address = SocketAddr::from((config.host, config.bridge_port));

    log_startup(&config, address);
    let state = AppState::new(config.clone());
    let heartbeat_tx = state.event_tx.clone();
    state
        .workers
        .spawn_critical("dashboard-heartbeat", move |context| async move {
            dashboard::run_heartbeat(heartbeat_tx, context).await
        });

    let app = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|source| ServerError::Bind { address, source })?;
    info!("Server started successfully. Waiting for requests...");

    let shutdown = CancellationToken::new();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .into_future();
    tokio::pin!(server);

    let serve_result = tokio::select! {
        result = &mut server => result.map_err(ServerError::Serve),
        _ = shutdown_signal() => {
            state.workers.cancel();
            shutdown.cancel();
            match tokio::time::timeout(
                config.runtime.server_shutdown_timeout,
                &mut server,
            ).await {
                Ok(result) => result.map_err(ServerError::Serve),
                Err(_) => Err(ServerError::GracefulTimeout(
                    config.runtime.server_shutdown_timeout,
                )),
            }
        }
    };

    // Always stop owned workers, including bind/serve-error paths after state
    // creation. No detached application worker is allowed past this point.
    state
        .workers
        .shutdown(config.runtime.worker_shutdown_timeout)
        .await?;
    serve_result?;
    info!("Server and all owned workers shut down gracefully.");
    Ok(())
}

fn log_startup(config: &BridgeConfig, address: SocketAddr) {
    info!(
        "◆ OpenCode2API v{} · gateway ready",
        env!("CARGO_PKG_VERSION")
    );
    info!("  Endpoint   http://{address}");
    info!("  Dashboard  http://{address}/dashboard");
    info!("  Model      {}", config.model.as_deref().unwrap_or("auto"));
    info!("  Shell      {}", config.shell_policy.description());
    info!(
        "  Auth       {}",
        if config.auth_enabled() {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!("  Upstream   port {}", config.opencode_port);
    info!(r#"  Claude Code: export ANTHROPIC_BASE_URL="http://{address}""#);
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl+C; starting graceful shutdown"),
        _ = terminate => info!("Received SIGTERM; starting graceful shutdown"),
    }
}
