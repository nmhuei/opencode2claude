//! Typed search-provider orchestration and fallback policy.

use super::providers;
use super::types::{
    format_search_context, SearchError, SearchErrorKind, SearchPolicy, SearchProviderKind,
    SearchQuery, SearchResult,
};
use crate::config::BridgeConfig;
use crate::observability::{Metrics, SearchMetricOutcome, SearchMetricProvider};
use futures_util::future::BoxFuture;
use reqwest::Client;
use std::fmt;
use std::sync::Arc;
use tracing::{info, warn};

/// One fallback-chain step: the provider identity (for metrics and errors)
/// plus its lazy request future.
type SearchAttempt<'a> = (
    SearchProviderKind,
    BoxFuture<'a, Result<Vec<SearchResult>, SearchError>>,
);

#[derive(Clone)]
pub struct SearchClient {
    pub(super) client: Client,
    /// Dedicated client for keyless scrapers with a same-host-only redirect
    /// policy; isolated so the shared `client` keeps stock behavior.
    pub(super) scraper_client: Client,
    pub(super) tavily_key: Option<String>,
    pub(super) exa_key: Option<String>,
    pub(super) serper_key: Option<String>,
    pub(super) searxng_key: Option<String>,
    pub(super) searxng_url: Option<String>,
    tavily_url: String,
    exa_url: String,
    serper_url: String,
    duckduckgo_url: String,
    yahoo_url: String,
    pub(super) policy: SearchPolicy,
    metrics: Option<Arc<Metrics>>,
}

// Provider credentials are held as plain strings for request building; the
// Debug output must never render them (config-layer redaction is undone by
// `.expose()` at construction time).
impl fmt::Debug for SearchClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchClient")
            .field("client", &self.client)
            .field("scraper_client", &self.scraper_client)
            .field(
                "tavily_key",
                &self.tavily_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("exa_key", &self.exa_key.as_ref().map(|_| "[REDACTED]"))
            .field(
                "serper_key",
                &self.serper_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "searxng_key",
                &self.searxng_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("searxng_url", &self.searxng_url)
            .field("tavily_url", &self.tavily_url)
            .field("exa_url", &self.exa_url)
            .field("serper_url", &self.serper_url)
            .field("duckduckgo_url", &self.duckduckgo_url)
            .field("yahoo_url", &self.yahoo_url)
            .field("policy", &self.policy)
            .field("metrics", &self.metrics.is_some())
            .finish()
    }
}

impl SearchClient {
    pub fn new(client: Client, config: &BridgeConfig) -> Self {
        Self::build(client, config, None)
    }

    pub fn new_with_metrics(client: Client, config: &BridgeConfig, metrics: Arc<Metrics>) -> Self {
        Self::build(client, config, Some(metrics))
    }

    fn build(client: Client, config: &BridgeConfig, metrics: Option<Arc<Metrics>>) -> Self {
        let scraper_client = Client::builder()
            .redirect(providers::scraper_redirect_policy())
            .build()
            .expect("failed to build search scraper HTTP client");
        Self {
            client,
            scraper_client,
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
            searxng_key: config
                .searxng_api_key
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
                chain_budget: config.search.chain_budget,
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

    /// Providers in strict fallback priority; keyless providers always
    /// present, keyed providers only when their credential is configured.
    /// Futures are lazy — nothing runs until the driver awaits them.
    fn attempts<'a>(&'a self, query: &'a SearchQuery) -> Vec<SearchAttempt<'a>> {
        let mut attempts: Vec<SearchAttempt<'a>> = Vec::new();
        if let Some(key) = &self.tavily_key {
            attempts.push((
                SearchProviderKind::Tavily,
                Box::pin(providers::tavily(
                    &self.client,
                    query,
                    key,
                    &self.tavily_url,
                    &self.policy,
                )),
            ));
        }
        if let Some(key) = &self.exa_key {
            attempts.push((
                SearchProviderKind::Exa,
                Box::pin(providers::exa(
                    &self.client,
                    query,
                    key,
                    &self.exa_url,
                    &self.policy,
                )),
            ));
        }
        if let Some(key) = &self.serper_key {
            attempts.push((
                SearchProviderKind::Serper,
                Box::pin(providers::serper(
                    &self.client,
                    query,
                    key,
                    &self.serper_url,
                    &self.policy,
                )),
            ));
        }
        if let Some(url) = &self.searxng_url {
            attempts.push((
                SearchProviderKind::SearXng,
                Box::pin(providers::searxng(
                    &self.scraper_client,
                    query,
                    url,
                    self.searxng_key.as_deref(),
                    &self.policy,
                )),
            ));
        }
        attempts.push((
            SearchProviderKind::DuckDuckGo,
            Box::pin(providers::duckduckgo(
                &self.scraper_client,
                query,
                &self.duckduckgo_url,
                &self.policy,
            )),
        ));
        attempts.push((
            SearchProviderKind::Yahoo,
            Box::pin(providers::yahoo(
                &self.scraper_client,
                query,
                &self.yahoo_url,
                &self.policy,
            )),
        ));
        attempts
    }

    pub async fn search_results(&self, raw_query: &str) -> Result<Vec<SearchResult>, SearchError> {
        let query = SearchQuery::new(raw_query, self.policy.max_results)?;
        let started = std::time::Instant::now();
        let mut last_error = None;

        for (provider, future) in self.attempts(&query) {
            // Hard wall-clock cap on the serial walk: once the chain budget
            // is gone, stop. Providers never attempted stay unrecorded.
            let Some(remaining) = self.policy.chain_budget.checked_sub(started.elapsed()) else {
                break;
            };
            match tokio::time::timeout(remaining, future).await {
                Ok(Ok(results)) if !results.is_empty() => {
                    self.record_search(provider, SearchMetricOutcome::Success);
                    return Ok(results);
                }
                Ok(Ok(_)) => {
                    self.record_search(provider, SearchMetricOutcome::NoResults);
                    no_results(provider, &mut last_error);
                }
                Ok(Err(error)) => {
                    self.record_search(provider, SearchMetricOutcome::Failure);
                    record_failure(error, &mut last_error);
                }
                Err(_elapsed) => {
                    // The deadline cut this provider mid-flight — distinct
                    // from the provider's own per-request timeout.
                    self.record_search(provider, SearchMetricOutcome::Failure);
                    record_failure(
                        SearchError::new(
                            provider,
                            SearchErrorKind::BudgetExhausted,
                            format!(
                                "chain budget of {:?} exhausted before a response arrived",
                                self.policy.chain_budget
                            ),
                        ),
                        &mut last_error,
                    );
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SearchError::new(
                SearchProviderKind::DuckDuckGo,
                SearchErrorKind::BudgetExhausted,
                "search chain budget exhausted before any provider completed",
            )
        }))
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
            SearchProviderKind::DuckDuckGo => SearchMetricProvider::DuckDuckGo,
            SearchProviderKind::Yahoo => SearchMetricProvider::Yahoo,
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
