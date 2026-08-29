//! HTTP server lifecycle with owned workers and bounded graceful shutdown.

use super::args::ServeArgsBridge;
use super::routes::build_router;
use crate::config::{self, BridgeConfig};
use crate::dashboard;
use crate::state::AppState;
use crate::workers::WorkerShutdownError;
use axum::Router;
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
    bind_and_serve(state, config, app, address).await
}

/// Bind the listener and run the serve loop with owned-worker shutdown.
///
/// Split out from [`run_server`] so tests can exercise the bind-failure path
/// against an already-occupied port and observe worker cleanup.
async fn bind_and_serve(
    state: AppState,
    config: BridgeConfig,
    app: Router,
    address: SocketAddr,
) -> Result<(), ServerError> {
    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(source) => {
            // Workers were registered while the state was built (dashboard
            // heartbeat, proxy pool workers). A bind failure must not leak
            // them on callers that reuse this process or runtime.
            if let Err(error) = state
                .workers
                .shutdown(config.runtime.worker_shutdown_timeout)
                .await
            {
                tracing::warn!(?error, "worker shutdown timed out after bind failure");
            }
            return Err(ServerError::Bind { address, source });
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_failure_stops_registered_workers() {
        // Occupy a port so the listener bind inside bind_and_serve fails.
        // AppState construction already registered workers (dashboard
        // heartbeat at minimum); the Bind error path must stop them instead
        // of leaking tracked tasks holding pool/history handles.
        let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("blocker bind");
        let port = blocker.local_addr().expect("blocker addr").port();

        let config = BridgeConfig {
            host: "127.0.0.1".parse().expect("loopback"),
            bridge_port: port,
            ..Default::default()
        };
        let address = SocketAddr::from((config.host, config.bridge_port));

        let state = AppState::new(config.clone());
        // A critical worker registered before the bind attempt: cancelling
        // the registry must stop it, and only the Bind error path can do so.
        state
            .workers
            .spawn_critical("bind-probe", |context| async move {
                context.cancellation().cancelled().await;
                Ok(())
            });

        let result = bind_and_serve(state.clone(), config, Router::new(), address).await;
        assert!(
            matches!(result, Err(ServerError::Bind { .. })),
            "occupied port must surface as ServerError::Bind"
        );
        assert!(
            !state.workers.snapshot().accepting_tasks,
            "bind failure must stop already-registered workers instead of leaking them"
        );
    }
}
