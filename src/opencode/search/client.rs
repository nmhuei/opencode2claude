//! Typed search-provider orchestration and fallback policy.

use super::providers;
use super::types::{
    format_search_context, SearchError, SearchErrorKind, SearchPolicy, SearchProviderKind,
    SearchQuery, SearchResult,
};
use crate::config::BridgeConfig;
use crate::observability::{Metrics, SearchMetricOutcome, SearchMetricProvider};
use reqwest::Client;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct SearchClient {
    pub(super) client: Client,
    pub(super) tavily_key: Option<String>,
    pub(super) exa_key: Option<String>,
    pub(super) serper_key: Option<String>,
    pub(super) searxng_url: Option<String>,
    tavily_url: String,
    exa_url: String,
    serper_url: String,
    duckduckgo_url: String,
    yahoo_url: String,
    policy: SearchPolicy,
    metrics: Option<Arc<Metrics>>,
}

impl SearchClient {
    pub fn new(client: Client, config: &BridgeConfig) -> Self {
        Self::build(client, config, None)
    }

    pub fn new_with_metrics(client: Client, config: &BridgeConfig, metrics: Arc<Metrics>) -> Self {
        Self::build(client, config, Some(metrics))
    }

    fn build(client: Client, config: &BridgeConfig, metrics: Option<Arc<Metrics>>) -> Self {
        Self {
            client,
            tavily_key: config
                .tavily_api_key
                .as_ref()
                .map(|value| value.expose().to_string()),
            exa_key: config
                .exa_api_key
                .as_ref()
                .map(|value| value.expose().to_string()),
            serper_key: config
                .serper_api_key
                .as_ref()
                .map(|value| value.expose().to_string()),
            searxng_url: config.searxng_url.clone(),
            tavily_url: config.search.tavily_url.clone(),
            exa_url: config.search.exa_url.clone(),
            serper_url: config.search.serper_url.clone(),
            duckduckgo_url: config.search.duckduckgo_url.clone(),
            yahoo_url: config.search.yahoo_url.clone(),
            policy: SearchPolicy {
                max_results: config.search.max_results,
                max_snippet_chars: config.search.max_snippet_chars,
                max_response_bytes: config.search.max_response_bytes,
                request_timeout: config.search.request_timeout,
                allow_private_searxng: config.search.allow_private_searxng,
            },
            metrics,
        }
    }

    /// Compatibility formatter used by the protocol search interception path.
    pub async fn search(&self, raw_query: &str) -> String {
        match self.search_results(raw_query).await {
            Ok(results) => format_search_context(&results, "No search results found."),
            Err(error) => format!("Web search failed: {error}"),
        }
    }

    pub async fn search_results(&self, raw_query: &str) -> Result<Vec<SearchResult>, SearchError> {
        let query = SearchQuery::new(raw_query, self.policy.max_results)?;
        let mut last_error = None;

        if let Some(key) = &self.tavily_key {
            match providers::tavily(&self.client, &query, key, &self.tavily_url, &self.policy).await
            {
                Ok(results) if !results.is_empty() => {
                    self.record_search(SearchProviderKind::Tavily, SearchMetricOutcome::Success);
                    return Ok(results);
                }
                Ok(_) => {
                    self.record_search(SearchProviderKind::Tavily, SearchMetricOutcome::NoResults);
                    no_results(SearchProviderKind::Tavily, &mut last_error);
                }
                Err(error) => {
                    self.record_search(SearchProviderKind::Tavily, SearchMetricOutcome::Failure);
                    record_failure(error, &mut last_error);
                }
            }
        }
        if let Some(key) = &self.exa_key {
            match providers::exa(&self.client, &query, key, &self.exa_url, &self.policy).await {
                Ok(results) if !results.is_empty() => {
                    self.record_search(SearchProviderKind::Exa, SearchMetricOutcome::Success);
                    return Ok(results);
                }
                Ok(_) => {
                    self.record_search(SearchProviderKind::Exa, SearchMetricOutcome::NoResults);
                    no_results(SearchProviderKind::Exa, &mut last_error);
                }
                Err(error) => {
                    self.record_search(SearchProviderKind::Exa, SearchMetricOutcome::Failure);
                    record_failure(error, &mut last_error);
                }
            }
        }
        if let Some(key) = &self.serper_key {
            match providers::serper(&self.client, &query, key, &self.serper_url, &self.policy).await
            {
                Ok(results) if !results.is_empty() => {
                    self.record_search(SearchProviderKind::Serper, SearchMetricOutcome::Success);
                    return Ok(results);
                }
                Ok(_) => {
                    self.record_search(SearchProviderKind::Serper, SearchMetricOutcome::NoResults);
                    no_results(SearchProviderKind::Serper, &mut last_error);
                }
                Err(error) => {
                    self.record_search(SearchProviderKind::Serper, SearchMetricOutcome::Failure);
                    record_failure(error, &mut last_error);
                }
            }
        }
        if let Some(url) = &self.searxng_url {
            match providers::searxng(&self.client, &query, url, &self.policy).await {
                Ok(results) if !results.is_empty() => {
                    self.record_search(SearchProviderKind::SearXng, SearchMetricOutcome::Success);
                    return Ok(results);
                }
                Ok(_) => {
                    self.record_search(SearchProviderKind::SearXng, SearchMetricOutcome::NoResults);
                    no_results(SearchProviderKind::SearXng, &mut last_error);
                }
                Err(error) => {
                    self.record_search(SearchProviderKind::SearXng, SearchMetricOutcome::Failure);
                    record_failure(error, &mut last_error);
                }
            }
        }

        info!(provider = "DuckDuckGo", "attempting web search");
        match providers::duckduckgo(&self.client, &query, &self.duckduckgo_url, &self.policy).await
        {
            Ok(results) if !results.is_empty() => {
                self.record_search(SearchProviderKind::DuckDuckGo, SearchMetricOutcome::Success);
                return Ok(results);
            }
            Ok(_) => {
                self.record_search(
                    SearchProviderKind::DuckDuckGo,
                    SearchMetricOutcome::NoResults,
                );
                no_results(SearchProviderKind::DuckDuckGo, &mut last_error);
            }
            Err(error) => {
                self.record_search(SearchProviderKind::DuckDuckGo, SearchMetricOutcome::Failure);
                record_failure(error, &mut last_error);
            }
        }

        info!(provider = "Yahoo", "attempting web search");
        match providers::yahoo(&self.client, &query, &self.yahoo_url, &self.policy).await {
            Ok(results) if !results.is_empty() => {
                self.record_search(SearchProviderKind::Yahoo, SearchMetricOutcome::Success);
                Ok(results)
            }
            Ok(_) => {
                self.record_search(SearchProviderKind::Yahoo, SearchMetricOutcome::NoResults);
                no_results(SearchProviderKind::Yahoo, &mut last_error);
                Err(last_error.unwrap_or_else(|| {
                    SearchError::new(
                        SearchProviderKind::Yahoo,
                        SearchErrorKind::NoResults,
                        "all configured providers returned no results",
                    )
                }))
            }
            Err(error) => {
                self.record_search(SearchProviderKind::Yahoo, SearchMetricOutcome::Failure);
                record_failure(error, &mut last_error);
                Err(last_error.unwrap_or_else(|| {
                    SearchError::new(
                        SearchProviderKind::Yahoo,
                        SearchErrorKind::NoResults,
                        "all configured providers failed",
                    )
                }))
            }
        }
    }

    fn record_search(&self, provider: SearchProviderKind, outcome: SearchMetricOutcome) {
        let Some(metrics) = &self.metrics else {
            return;
        };
        let provider = match provider {
            SearchProviderKind::Tavily => SearchMetricProvider::Tavily,
            SearchProviderKind::Exa => SearchMetricProvider::Exa,
            SearchProviderKind::Serper => SearchMetricProvider::Serper,
            SearchProviderKind::SearXng => SearchMetricProvider::SearXng,
            SearchProviderKind::DuckDuckGo | SearchProviderKind::Yahoo => {
                SearchMetricProvider::DuckDuckGo
            }
        };
        metrics.record_search(provider, outcome);
    }
}

fn record_failure(error: SearchError, last_error: &mut Option<SearchError>) {
    warn!(
        provider = %error.provider,
        kind = %error.kind,
        message = %error.message,
        "search provider failed; continuing fallback chain"
    );
    *last_error = Some(error);
}

fn no_results(provider: SearchProviderKind, last_error: &mut Option<SearchError>) {
    let error = SearchError::new(
        provider,
        SearchErrorKind::NoResults,
        "provider returned no results",
    );
    info!(provider = %provider, "search provider returned no results; continuing fallback chain");
    *last_error = Some(error);
}
