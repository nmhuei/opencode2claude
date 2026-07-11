//! Application state shared across all handlers.

use crate::config::BridgeConfig;
use crate::dashboard::DashboardEvent;
use crate::opencode::search::SearchClient;
use crate::proxy_pool::{health_monitor, identity_monitor, process_restart_queue, ProxyPool};
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
    /// Optional rate limiter semaphore (None = no limit).
    pub rate_limiter: Option<Arc<Semaphore>>,
    /// Thread-safe SOCKS5/HTTP proxy pool for multi-agent support.
    pub proxy_pool: Arc<RwLock<ProxyPool>>,
    /// Broadcast channel for dashboard SSE events.
    pub event_tx: broadcast::Sender<DashboardEvent>,
    /// Unix timestamp (seconds) when the server started.
    pub started_at: Arc<AtomicU64>,
}

impl AppState {
    /// Create a new AppState from the given configuration.
    pub fn new(config: BridgeConfig) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to create HTTP client");

        let rate_limiter = config
            .observability
            .max_concurrent_requests
            .map(|permits| Arc::new(Semaphore::new(permits)));
        let search_client = SearchClient::new(http_client.clone(), &config);

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
            let pool = ProxyPool::new_with_egress_policy(
                &all_urls,
                config.egress.active_proxy_count,
                config.egress.require_verified_exit_ip,
                config.egress.identity_ttl,
            );
            // Spawn background tasks for pool management
            if !pool.proxies.is_empty() {
                let pool_arc = Arc::new(RwLock::new(pool));
                let hc_pool = pool_arc.clone();
                let rq_pool = pool_arc.clone();

                tokio::spawn(async move {
                    health_monitor(hc_pool).await;
                });
                info!("Proxy pool health monitor spawned.");

                tokio::spawn(async move {
                    process_restart_queue(rq_pool).await;
                });
                info!("Proxy pool restart queue processor spawned.");

                if !config.egress.identity_endpoints.is_empty() {
                    let identity_pool = pool_arc.clone();
                    let identity_endpoints = config.egress.identity_endpoints.clone();
                    let identity_interval = config.egress.health_interval;
                    tokio::spawn(async move {
                        identity_monitor(identity_pool, identity_endpoints, identity_interval)
                            .await;
                    });
                    info!("Proxy pool exit-identity monitor spawned.");
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
            proxy_pool,
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
