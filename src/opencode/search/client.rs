//! Search-provider orchestration and fallback policy.

use super::providers;
use crate::config::BridgeConfig;
use reqwest::Client;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct SearchClient {
    pub(super) client: Client,
    pub(super) tavily_key: Option<String>,
    pub(super) exa_key: Option<String>,
    pub(super) serper_key: Option<String>,
    pub(super) searxng_url: Option<String>,
}

impl SearchClient {
    pub fn new(client: Client, config: &BridgeConfig) -> Self {
        Self {
            client,
            tavily_key: config.tavily_api_key.clone(),
            exa_key: config.exa_api_key.clone(),
            serper_key: config.serper_api_key.clone(),
            searxng_url: config.searxng_url.clone(),
        }
    }

    pub async fn search(&self, query: &str) -> String {
        if let Some(key) = &self.tavily_key {
            if let Some(result) =
                attempt("Tavily", providers::tavily(&self.client, query, key).await)
            {
                return result;
            }
        }
        if let Some(key) = &self.exa_key {
            if let Some(result) = attempt("Exa", providers::exa(&self.client, query, key).await) {
                return result;
            }
        }
        if let Some(key) = &self.serper_key {
            if let Some(result) =
                attempt("Serper", providers::serper(&self.client, query, key).await)
            {
                return result;
            }
        }
        if let Some(url) = &self.searxng_url {
            if let Some(result) = attempt(
                "SearXNG",
                providers::searxng(&self.client, query, url).await,
            ) {
                return result;
            }
        }

        info!(provider = "DuckDuckGo", "attempting web search");
        providers::duckduckgo(&self.client, query)
            .await
            .unwrap_or_else(|error| error)
    }
}

fn attempt(provider: &str, result: Result<String, String>) -> Option<String> {
    info!(%provider, "attempting web search");
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            warn!(%provider, %error, "search provider failed; continuing fallback chain");
            None
        }
    }
}
