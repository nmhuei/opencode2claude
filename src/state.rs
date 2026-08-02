//! Application state shared across all handlers.

use crate::api_key::ApiKeyRegistry;
use crate::audit::AuditLog;
use crate::config::BridgeConfig;
use crate::dashboard::DashboardEvent;
use crate::docker::{ContainerRuntime, DockerCliRuntime};
use crate::history::HistoryStore;
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use crate::infrastructure::warp::{CliWarpController, WarpController};
use crate::observability::Metrics;
use crate::opencode::search::SearchClient;
use crate::proxy_pool::{health_monitor, identity_monitor, process_restart_queue, ProxyPool};
use crate::workers::WorkerRegistry;
use reqwest::Client;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::sync::Semaphore;
use tracing::info;

/// Shared application state, injected into handlers via Axum's State extractor.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Bridge configuration (shared via Arc for cheap cloning).
    pub config: Arc<BridgeConfig>,
    /// Reusable search client with shared HTTP connection pool.
    pub search_client: SearchClient,
    /// Reusable HTTP client with connection pooling for daemon health checks.
    pub http_client: Client,
    /// Optional global rate limiter semaphore (None = no limit).
    pub rate_limiter: Option<Arc<Semaphore>>,
    /// Hot-reloadable API-key registry with per-client policy and usage state.
    pub api_keys: Arc<RwLock<ApiKeyRegistry>>,
    /// Thread-safe SOCKS5/HTTP proxy pool for multi-agent support.
    pub proxy_pool: Arc<RwLock<ProxyPool>>,
    /// Replaceable container runtime used by management and egress workers.
    pub container_runtime: Arc<dyn ContainerRuntime>,
    /// Optional host-level WARP controller used only by explicit direct mode.
    pub warp_controller: Arc<dyn WarpController>,
    /// Atomic filesystem adapter used by config/runtime management transports.
    pub file_store: Arc<dyn FileStore>,
    /// Owner of critical workers and ephemeral request tasks.
    pub workers: Arc<WorkerRegistry>,
    /// In-process request counters and latency aggregates.
    pub metrics: Arc<Metrics>,
    /// Bounded secret-safe management audit trail.
    pub audit_log: Arc<AuditLog>,
    /// Persistent request, prompt, reasoning and response history.
    pub history: Arc<HistoryStore>,
    /// Broadcast channel for dashboard SSE events.
    pub event_tx: broadcast::Sender<DashboardEvent>,
    /// Unix timestamp (seconds) when the server started.
    pub started_at: Arc<AtomicU64>,
}

impl AppState {
    /// Create a new AppState from the given configuration.
    pub fn new(config: BridgeConfig) -> Self {
        let container_runtime: Arc<dyn ContainerRuntime> =
            Arc::new(DockerCliRuntime::from_config(&config));
        let warp_controller: Arc<dyn WarpController> = Arc::new(CliWarpController::new(
            config.runtime.warp_cli_binary.clone(),
        ));
        Self::new_with_infrastructure(
            config,
            container_runtime,
            warp_controller,
            Arc::new(AtomicFileStore),
        )
    }

    pub fn new_with_container_runtime(
        config: BridgeConfig,
        container_runtime: Arc<dyn ContainerRuntime>,
    ) -> Self {
        let warp_controller: Arc<dyn WarpController> = Arc::new(CliWarpController::new(
            config.runtime.warp_cli_binary.clone(),
        ));
        Self::new_with_infrastructure(
            config,
            container_runtime,
            warp_controller,
            Arc::new(AtomicFileStore),
        )
    }

    pub fn new_with_adapters(
        config: BridgeConfig,
        container_runtime: Arc<dyn ContainerRuntime>,
        warp_controller: Arc<dyn WarpController>,
    ) -> Self {
        Self::new_with_infrastructure(
            config,
            container_runtime,
            warp_controller,
            Arc::new(AtomicFileStore),
        )
    }

    pub fn new_with_infrastructure(
        config: BridgeConfig,
        container_runtime: Arc<dyn ContainerRuntime>,
        warp_controller: Arc<dyn WarpController>,
        file_store: Arc<dyn FileStore>,
    ) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to create HTTP client");

        let rate_limiter = config
            .observability
            .max_concurrent_requests
            .map(|permits| Arc::new(Semaphore::new(permits)));
        let api_keys = Arc::new(RwLock::new(ApiKeyRegistry::load_or_default(
            &config,
            file_store.as_ref(),
        )));
        let workers = Arc::new(WorkerRegistry::new());
        let metrics = Arc::new(Metrics::default());
        let audit_log = Arc::new(AuditLog::default());
        let history = HistoryStore::open(
            config.history.clone(),
            crate::runtime::RuntimePaths::from_config(&config).history_database(),
        );
        let search_client =
            SearchClient::new_with_metrics(http_client.clone(), &config, metrics.clone());

        // Create proxy pool with 2-tier primary + warm-standby model
        // Combine primary (managed) and warm-standby (protected) proxy URLs
        let mut all_urls: Vec<String> = Vec::new();
        if let Some(ref urls) = config.primary_proxies {
            all_urls.extend(urls.iter().cloned());
        }
        if let Some(ref urls) = config.warm_standby_proxies {
            all_urls.extend(urls.iter().cloned());
        }

        let proxy_pool = if !all_urls.is_empty() {
            let mut pool = ProxyPool::new_with_egress_policy(
                &all_urls,
                config.egress.active_proxy_count,
                config.egress.require_verified_exit_ip,
                config.egress.identity_ttl,
            );
            pool.set_max_restart_attempts(config.egress.max_restart_attempts);
            // Spawn background tasks for pool management
            if !pool.proxies.is_empty() {
                let pool_arc = Arc::new(RwLock::new(pool));
                let health_pool = pool_arc.clone();
                let health_interval = config.egress.health_interval;
                workers.spawn_critical("proxy-health", move |context| async move {
                    health_monitor(health_pool, health_interval, context).await
                });
                info!("Proxy pool health monitor registered.");

                let restart_pool = pool_arc.clone();
                let restart_runtime = container_runtime.clone();
                let restart_image = config.runtime.warp_image.clone();
                let restart_interval = config.egress.restart_interval;
                let restart_identity_endpoints = config.egress.identity_endpoints.clone();
                let restart_metrics = metrics.clone();
                workers.spawn_critical("proxy-restart", move |context| async move {
                    process_restart_queue(
                        restart_pool,
                        restart_runtime,
                        restart_image,
                        restart_identity_endpoints,
                        restart_interval,
                        restart_metrics,
                        context,
                    )
                    .await
                });
                info!("Proxy pool restart queue processor registered.");

                if !config.egress.identity_endpoints.is_empty() {
                    let identity_pool = pool_arc.clone();
                    let identity_endpoints = config.egress.identity_endpoints.clone();
                    let identity_interval = config.egress.health_interval;
                    workers.spawn_critical("proxy-identity", move |context| async move {
                        identity_monitor(
                            identity_pool,
                            identity_endpoints,
                            identity_interval,
                            context,
                        )
                        .await
                    });
                    info!("Proxy pool exit-identity monitor registered.");
                }

                pool_arc
            } else {
                Arc::new(RwLock::new(pool))
            }
        } else {
            Arc::new(RwLock::new(ProxyPool::default()))
        };

        // Broadcast channel for dashboard SSE (capacity 256)
        let (event_tx, _) = broadcast::channel(256);

        // Record server start timestamp
        let started_at = Arc::new(AtomicU64::new(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ));

        Self {
            config: Arc::new(config),
            search_client,
            http_client,
            rate_limiter,
            api_keys,
            proxy_pool,
            container_runtime,
            warp_controller,
            file_store,
            workers,
            metrics,
            audit_log,
            history,
            event_tx,
            started_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;

    #[test]
    fn test_app_state_creates_client() {
        let config = BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 256,
            ..Default::default()
        };
        let state = AppState::new(config);
        assert_eq!(state.config.bridge_port, 0);
    }
}
