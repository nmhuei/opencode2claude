//! Application state shared across all handlers.

use crate::config::BridgeConfig;
use crate::opencode::search::SearchClient;
use crate::proxy_pool::{health_monitor, process_restart_queue, ProxyPool};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
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
}

impl AppState {
    /// Create a new AppState from the given configuration.
    pub fn new(config: BridgeConfig) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to create HTTP client");

        let rate_limiter = std::env::var("BRIDGE_RATE_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|permits| Arc::new(Semaphore::new(permits)));
        let search_client = SearchClient::new(http_client.clone(), &config);

        // Create proxy pool with hot-spare model
        let proxy_pool = if let Some(ref urls) = config.proxies {
            let pool = ProxyPool::new(urls);
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

                pool_arc
            } else {
                Arc::new(RwLock::new(pool))
            }
        } else {
            Arc::new(RwLock::new(ProxyPool::default()))
        };

        Self {
            config: Arc::new(config),
            search_client,
            http_client,
            rate_limiter,
            proxy_pool,
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
            model: None,
            shell_policy: crate::shell::ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 256,
            tavily_api_key: None,
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 5,
            proxies: None,
        };
        let state = AppState::new(config);
        assert_eq!(state.config.bridge_port, 0);
    }
}
