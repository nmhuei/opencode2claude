//! Application state shared across all handlers.

use crate::config::BridgeConfig;
use crate::opencode::search::SearchClient;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;

/// Shared application state, injected into handlers via Axum's State extractor.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Bridge configuration (shared via Arc for cheap cloning).
    pub config: Arc<BridgeConfig>,
    /// Reusable search client with shared HTTP connection pool.
    pub search_client: SearchClient,
    /// Reusable HTTP client with connection pooling for daemon health checks.
    pub http_client: Client,
}

impl AppState {
    /// Create a new AppState from the given configuration.
    pub fn new(config: BridgeConfig) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to create HTTP client");

        let search_client = SearchClient::new(http_client.clone(), &config);

        Self {
            config: Arc::new(config),
            search_client,
            http_client,
        }
    }
}
