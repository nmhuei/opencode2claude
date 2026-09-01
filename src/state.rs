//! Application state shared across all handlers.

use crate::api_key::ApiKeyRegistry;
use crate::audit::AuditLog;
use crate::config::BridgeConfig;
use crate::dashboard::DashboardEvent;
use crate::docker::{ContainerRuntime, DockerCliRuntime};
use crate::handlers::ShellDelegations;
use crate::history::HistoryStore;
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use crate::infrastructure::warp::{DisabledWarpController, WarpController};
use crate::observability::Metrics;
use crate::opencode::search::SearchClient;
use crate::proxy_pool::{
    health_monitor, hybrid_proxy_reconciler, identity_monitor, process_restart_queue,
    LiveProxyVerifier, ProxyPool, ProxySubsystemStatus,
};
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
    /// Coarse proxy subsystem readiness used by hybrid route selection.
    pub proxy_subsystem: Arc<RwLock<ProxySubsystemStatus>>,
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
    /// Single-use tickets binding echoed local-shell results to bridge-issued delegations.
    pub shell_delegations: Arc<ShellDelegations>,
    /// Broadcast channel for dashboard SSE events.
    pub event_tx: broadcast::Sender<DashboardEvent>,
    /// Unix timestamp (seconds) when the server started.
    pub started_at: Arc<AtomicU64>,
    /// Shared atomic round-robin index for upstream API keys.
    pub upstream_key_index: Arc<std::sync::atomic::AtomicUsize>,
}

/// Upper bound on the TCP/TLS connect phase of the shared HTTP client.
///
/// The client backs direct-route upstream traffic, daemon health checks and
/// the search fallback chain. Without this bound a blackholed host holds each
/// task until the 600s total timeout expires, piling hung tasks against the
/// rate limiter; the total timeout alone must not be the connect guard.
pub(crate) const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

impl AppState {
    /// Create a new AppState from the given configuration.
    pub fn new(config: BridgeConfig) -> Self {
        let container_runtime: Arc<dyn ContainerRuntime> =
            Arc::new(DockerCliRuntime::from_config(&config));
        let warp_controller: Arc<dyn WarpController> = Arc::new(DisabledWarpController);
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
        let warp_controller: Arc<dyn WarpController> = Arc::new(DisabledWarpController);
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
        let config = Arc::new(config);
        let http_client = Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
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

        let egress_uses_pool = matches!(
            config.egress.mode,
            crate::config::EgressMode::Proxy | crate::config::EgressMode::Hybrid
        );

        // Whether a usable pool actually materialized. Unparseable URLs are
        // silently dropped by the pool constructor, so this is judged on the
        // resulting pool rather than the raw URL list.
        let (proxy_pool, proxy_subsystem) = if egress_uses_pool && !all_urls.is_empty() {
            let mut pool = ProxyPool::new_with_egress_policy(
                &all_urls,
                config.egress.active_proxy_count,
                config.egress.require_verified_exit_ip,
                config.egress.identity_ttl,
            );
            pool.set_max_restart_attempts(config.egress.max_restart_attempts);

            if !pool.proxies.is_empty() {
                let pool_arc = Arc::new(RwLock::new(pool));

                // Subsystem lifecycle starts as Starting only when a
                // reconciler will actually run; workers below own it from
                // here on.
                let proxy_subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));

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

                // Every proxy-backed egress mode (pure proxy and hybrid)
                // needs the reconciler driving ProxySubsystemStatus; without
                // it the snapshot stays Starting forever and readiness
                // reporters would have to guess from pool heuristics.
                if egress_uses_pool {
                    let reconcile_pool = pool_arc.clone();
                    let reconcile_subsystem = proxy_subsystem.clone();
                    let reconcile_runtime = container_runtime.clone();
                    let reconcile_config = config.clone();
                    let reconcile_metrics = metrics.clone();
                    let verifier = Arc::new(LiveProxyVerifier);
                    workers.spawn_critical("proxy-reconcile", move |context| async move {
                        hybrid_proxy_reconciler(
                            reconcile_pool,
                            reconcile_subsystem,
                            reconcile_runtime,
                            verifier,
                            reconcile_config,
                            reconcile_metrics,
                            context,
                        )
                        .await
                    });
                    info!("Proxy subsystem reconciler registered.");
                }

                (pool_arc, proxy_subsystem)
            } else {
                tracing::warn!(
                    configured_urls = all_urls.len(),
                    "no configured proxy URL could be loaded; proxy egress starts disabled"
                );
                (
                    Arc::new(RwLock::new(pool)),
                    Arc::new(RwLock::new(ProxySubsystemStatus::disabled())),
                )
            }
        } else {
            (
                Arc::new(RwLock::new(ProxyPool::default())),
                Arc::new(RwLock::new(ProxySubsystemStatus::disabled())),
            )
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
            config,
            search_client,
            http_client,
            rate_limiter,
            api_keys,
            proxy_pool,
            proxy_subsystem,
            container_runtime,
            warp_controller,
            file_store,
            workers,
            metrics,
            audit_log,
            history,
            shell_delegations: Arc::new(ShellDelegations::new()),
            event_tx,
            started_at,
            upstream_key_index: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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

    #[derive(Debug, Default)]
    struct FailingRuntime;

    #[async_trait::async_trait]
    impl crate::docker::ContainerRuntime for FailingRuntime {
        async fn daemon_version(&self) -> crate::docker::DockerResult<String> {
            Ok("test".to_string())
        }
        async fn inspect(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<crate::docker::ContainerState> {
            Err(crate::docker::DockerError::CommandFailed(
                "test runtime unavailable".to_string(),
            ))
        }
        async fn create_missing(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<()> {
            Ok(())
        }
        async fn recreate_managed(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<()> {
            Ok(())
        }
        async fn remove_managed(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<()> {
            Ok(())
        }
        async fn restart_managed(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<()> {
            Ok(())
        }
        async fn stop_managed(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<()> {
            Ok(())
        }
        async fn start_managed(
            &self,
            _spec: &crate::docker::ProxySpec,
        ) -> crate::docker::DockerResult<()> {
            Ok(())
        }
        async fn logs(
            &self,
            _spec: &crate::docker::ProxySpec,
            _tail: usize,
        ) -> crate::docker::DockerResult<String> {
            Ok(String::new())
        }
        async fn list(
            &self,
            _specs: &[crate::docker::ProxySpec],
        ) -> crate::docker::DockerResult<Vec<crate::docker::ContainerSummary>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn hybrid_constructs_pool_and_registers_reconcile_worker_but_starts_not_ready() {
        let mut config = BridgeConfig::default();
        config.egress.mode = crate::config::EgressMode::Hybrid;
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);
        config.egress.active_proxy_count = 1;

        let state = AppState::new_with_container_runtime(config, Arc::new(FailingRuntime));
        assert_eq!(state.proxy_pool.read().await.proxies.len(), 2);
        assert!(!state.proxy_subsystem.read().await.is_ready());

        let names = state
            .workers
            .snapshot()
            .workers
            .into_iter()
            .map(|worker| worker.name)
            .collect::<std::collections::HashSet<_>>();
        for expected in [
            "proxy-health",
            "proxy-identity",
            "proxy-restart",
            "proxy-reconcile",
        ] {
            assert!(names.contains(expected), "missing worker {expected}");
        }
    }

    #[tokio::test]
    async fn proxy_mode_drives_subsystem_lifecycle_via_reconcile_worker() {
        // Pure-proxy deployments must not leave ProxySubsystemStatus stuck in
        // Starting forever: the same reconciler that drives the lifecycle in
        // hybrid mode has to run here too. Identity endpoints are cleared so
        // no worker ever probes an external service from this test.
        let mut config = BridgeConfig::default();
        config.egress.mode = crate::config::EgressMode::Proxy;
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);
        config.egress.active_proxy_count = 1;
        config.egress.identity_endpoints = Vec::new();

        let state = AppState::new_with_container_runtime(config, Arc::new(FailingRuntime));
        assert_eq!(state.proxy_pool.read().await.proxies.len(), 2);
        assert!(!state.proxy_subsystem.read().await.is_ready());

        let names = state
            .workers
            .snapshot()
            .workers
            .into_iter()
            .map(|worker| worker.name)
            .collect::<std::collections::HashSet<_>>();
        for expected in ["proxy-health", "proxy-restart", "proxy-reconcile"] {
            assert!(names.contains(expected), "missing worker {expected}");
        }

        // With the container runtime unavailable the very first reconcile
        // cycle must degrade the subsystem instead of leaving it Starting.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if state.proxy_subsystem.read().await.snapshot().phase
                == crate::proxy_pool::ProxySubsystemPhase::Degraded
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "pure-proxy subsystem stayed out of the reconciled lifecycle"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let snapshot = state.proxy_subsystem.read().await.snapshot();
        assert_eq!(
            snapshot.phase,
            crate::proxy_pool::ProxySubsystemPhase::Degraded
        );
        assert!(snapshot.last_error.is_some());

        state
            .workers
            .shutdown(Duration::from_millis(500))
            .await
            .expect("reconcile worker must stay cancellable in pure-proxy mode");
    }

    #[tokio::test]
    async fn direct_mode_does_not_register_proxy_pool_or_workers() {
        let mut config = BridgeConfig::default();
        config.egress.mode = crate::config::EgressMode::Direct;
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);

        let state = AppState::new(config);
        assert!(
            state.proxy_pool.read().await.proxies.is_empty(),
            "direct mode must not create a live proxy pool"
        );
        let snapshot = state.workers.snapshot();
        assert!(
            snapshot
                .workers
                .iter()
                .all(|worker| !worker.name.starts_with("proxy-")),
            "direct mode must not register proxy health/restart/identity workers"
        );
    }

    #[tokio::test]
    async fn hybrid_mode_without_proxies_reports_disabled_subsystem() {
        // The shipped default posture is Hybrid egress with no proxy URLs at
        // all. No pool exists and no reconciler will ever run, so advertising
        // "Starting" forever would be a lie; the subsystem must report
        // Disabled exactly like Direct mode does.
        let config = BridgeConfig::default();
        assert_eq!(config.egress.mode, crate::config::EgressMode::Hybrid);
        assert!(config.primary_proxies.is_none());

        let state = AppState::new(config);
        let snapshot = state.proxy_subsystem.read().await.snapshot();
        assert_eq!(
            snapshot.phase,
            crate::proxy_pool::ProxySubsystemPhase::Disabled,
            "hybrid mode with an empty proxy pool must not stay in Starting forever"
        );
    }

    #[tokio::test]
    async fn hybrid_mode_with_only_unparseable_proxies_disables_subsystem() {
        // Proxy URLs that fail to parse are silently dropped by the pool
        // constructor. The spawn gate must judge on the resulting pool, not
        // on the raw URL list, or the subsystem sticks in Starting while no
        // worker ever runs.
        let mut config = BridgeConfig::default();
        config.egress.mode = crate::config::EgressMode::Hybrid;
        config.primary_proxies = Some(vec!["this is not a proxy url".to_string()]);

        let state = AppState::new_with_container_runtime(config, Arc::new(FailingRuntime));
        assert!(state.proxy_pool.read().await.proxies.is_empty());
        let snapshot = state.proxy_subsystem.read().await.snapshot();
        assert_eq!(
            snapshot.phase,
            crate::proxy_pool::ProxySubsystemPhase::Disabled,
            "a pool that parsed to zero proxies must disable the subsystem instead of sticking in Starting"
        );
        let workers = state.workers.snapshot();
        assert!(
            workers
                .workers
                .iter()
                .all(|worker| !worker.name.starts_with("proxy-")),
            "no proxy worker may register for a pool with zero usable proxies"
        );
    }

    #[test]
    fn http_client_bounds_connect_phase() {
        // The shared client backs direct-route upstream traffic, daemon
        // health checks and the search chain. A blackholed TCP connect must
        // be cut by the connect timeout long before the 600s total timeout,
        // or hung tasks pile up against the rate limiter.
        assert_eq!(
            HTTP_CONNECT_TIMEOUT,
            Duration::from_secs(10),
            "shared HTTP client connect phase must stay bounded"
        );
    }

    #[tokio::test]
    async fn production_state_forbids_host_warp_reconnect() {
        let state = AppState::new(BridgeConfig::default());
        let error = state
            .warp_controller
            .reconnect()
            .await
            .expect_err("host WARP control must remain disabled");
        assert!(error.to_string().contains("forbidden"));
    }
}
