//! Application state shared across all handlers.

use crate::config::BridgeConfig;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;

/// Shared application state, injected into handlers via Axum's State extractor.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Bridge configuration (shared via Arc for cheap cloning).
    pub config: Arc<BridgeConfig>,
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

        Self {
            config: Arc::new(config),
            http_client,
        }
    }
}
