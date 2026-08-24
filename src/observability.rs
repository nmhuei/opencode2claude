//! Request correlation and bounded in-process operational counters.

use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct SearchProviderMetricsSnapshot {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub no_results: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub requests_total: u64,
    pub responses_2xx: u64,
    pub responses_4xx: u64,
    pub responses_5xx: u64,
    pub active_requests: usize,
    pub peak_active_requests: usize,
    pub latency_total_ms: u64,
    pub latency_max_ms: u64,
    pub generated_request_ids: u64,
    pub streams_started: u64,
    pub streams_completed: u64,
    pub streams_cancelled: u64,
    pub streams_failed: u64,
    pub active_streams: usize,
    pub peak_active_streams: usize,
    pub retry_transport: u64,
    pub retry_timeout: u64,
    pub retry_rate_limit: u64,
    pub retry_provider_client: u64,
    pub retry_provider_server: u64,
    pub retry_malformed_response: u64,
    pub model_fallbacks: u64,
    pub native_tool_calls: u64,
    pub encoded_fallback_candidates: u64,
    pub encoded_native_retries: u64,
    pub encoded_fallback_tool_calls: u64,
    pub encoded_fallback_rejections: u64,
    pub literal_marker_suppressions: u64,
    pub proxy_restart_attempts: u64,
    pub proxy_restart_successes: u64,
    pub proxy_restart_failures: u64,
    pub search_tavily: SearchProviderMetricsSnapshot,
    pub search_exa: SearchProviderMetricsSnapshot,
    pub search_serper: SearchProviderMetricsSnapshot,
    pub search_searxng: SearchProviderMetricsSnapshot,
    pub search_duckduckgo: SearchProviderMetricsSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryMetricClass {
    Transport,
    Timeout,
    RateLimit,
    ProviderClient,
    ProviderServer,
    MalformedResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProtocolMetricClass {
    NativeToolCall,
    EncodedCandidate,
    EncodedNativeRetry,
    EncodedFallbackToolCall,
    EncodedFallbackRejection,
    LiteralMarkerSuppression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMetricProvider {
    Tavily,
    Exa,
    Serper,
    SearXng,
    DuckDuckGo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMetricOutcome {
    Success,
    Failure,
    NoResults,
}

#[derive(Debug, Default)]
struct SearchProviderCounters {
    attempts: AtomicU64,
    successes: AtomicU64,
    failures: AtomicU64,
    no_results: AtomicU64,
}

impl SearchProviderCounters {
    fn snapshot(&self) -> SearchProviderMetricsSnapshot {
        SearchProviderMetricsSnapshot {
            attempts: self.attempts.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            no_results: self.no_results.load(Ordering::Relaxed),
        }
    }

    fn record(&self, outcome: SearchMetricOutcome) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        match outcome {
            SearchMetricOutcome::Success => {
                self.successes.fetch_add(1, Ordering::Relaxed);
            }
            SearchMetricOutcome::Failure => {
                self.failures.fetch_add(1, Ordering::Relaxed);
            }
            SearchMetricOutcome::NoResults => {
                self.no_results.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Metrics {
    requests_total: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    active_requests: AtomicUsize,
    peak_active_requests: AtomicUsize,
    latency_total_ms: AtomicU64,
    latency_max_ms: AtomicU64,
    generated_request_ids: AtomicU64,
    request_sequence: AtomicU64,
    streams_started: AtomicU64,
    streams_completed: AtomicU64,
    streams_cancelled: AtomicU64,
    streams_failed: AtomicU64,
    active_streams: AtomicUsize,
    peak_active_streams: AtomicUsize,
    retry_transport: AtomicU64,
    retry_timeout: AtomicU64,
    retry_rate_limit: AtomicU64,
    retry_provider_client: AtomicU64,
    retry_provider_server: AtomicU64,
    retry_malformed_response: AtomicU64,
    model_fallbacks: AtomicU64,
    native_tool_calls: AtomicU64,
    encoded_fallback_candidates: AtomicU64,
    encoded_native_retries: AtomicU64,
    encoded_fallback_tool_calls: AtomicU64,
    encoded_fallback_rejections: AtomicU64,
    literal_marker_suppressions: AtomicU64,
    proxy_restart_attempts: AtomicU64,
    proxy_restart_successes: AtomicU64,
    proxy_restart_failures: AtomicU64,
    search_tavily: SearchProviderCounters,
    search_exa: SearchProviderCounters,
    search_serper: SearchProviderCounters,
    search_searxng: SearchProviderCounters,
    search_duckduckgo: SearchProviderCounters,
}

impl Metrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            responses_2xx: self.responses_2xx.load(Ordering::Relaxed),
            responses_4xx: self.responses_4xx.load(Ordering::Relaxed),
            responses_5xx: self.responses_5xx.load(Ordering::Relaxed),
            active_requests: self.active_requests.load(Ordering::Relaxed),
            peak_active_requests: self.peak_active_requests.load(Ordering::Relaxed),
            latency_total_ms: self.latency_total_ms.load(Ordering::Relaxed),
            latency_max_ms: self.latency_max_ms.load(Ordering::Relaxed),
            generated_request_ids: self.generated_request_ids.load(Ordering::Relaxed),
            streams_started: self.streams_started.load(Ordering::Relaxed),
            streams_completed: self.streams_completed.load(Ordering::Relaxed),
            streams_cancelled: self.streams_cancelled.load(Ordering::Relaxed),
            streams_failed: self.streams_failed.load(Ordering::Relaxed),
            active_streams: self.active_streams.load(Ordering::Relaxed),
            peak_active_streams: self.peak_active_streams.load(Ordering::Relaxed),
            retry_transport: self.retry_transport.load(Ordering::Relaxed),
            retry_timeout: self.retry_timeout.load(Ordering::Relaxed),
            retry_rate_limit: self.retry_rate_limit.load(Ordering::Relaxed),
            retry_provider_client: self.retry_provider_client.load(Ordering::Relaxed),
            retry_provider_server: self.retry_provider_server.load(Ordering::Relaxed),
            retry_malformed_response: self.retry_malformed_response.load(Ordering::Relaxed),
            model_fallbacks: self.model_fallbacks.load(Ordering::Relaxed),
            native_tool_calls: self.native_tool_calls.load(Ordering::Relaxed),
            encoded_fallback_candidates: self.encoded_fallback_candidates.load(Ordering::Relaxed),
            encoded_native_retries: self.encoded_native_retries.load(Ordering::Relaxed),
            encoded_fallback_tool_calls: self.encoded_fallback_tool_calls.load(Ordering::Relaxed),
            encoded_fallback_rejections: self.encoded_fallback_rejections.load(Ordering::Relaxed),
            literal_marker_suppressions: self.literal_marker_suppressions.load(Ordering::Relaxed),
            proxy_restart_attempts: self.proxy_restart_attempts.load(Ordering::Relaxed),
            proxy_restart_successes: self.proxy_restart_successes.load(Ordering::Relaxed),
            proxy_restart_failures: self.proxy_restart_failures.load(Ordering::Relaxed),
            search_tavily: self.search_tavily.snapshot(),
            search_exa: self.search_exa.snapshot(),
            search_serper: self.search_serper.snapshot(),
            search_searxng: self.search_searxng.snapshot(),
            search_duckduckgo: self.search_duckduckgo.snapshot(),
        }
    }

    pub fn begin_stream(self: &Arc<Self>) -> StreamMetricsGuard {
        self.streams_started.fetch_add(1, Ordering::Relaxed);
        let active = self.active_streams.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_active_streams
            .fetch_max(active, Ordering::Relaxed);
        StreamMetricsGuard {
            metrics: self.clone(),
            outcome: StreamOutcome::Failed,
            finished: false,
        }
    }

    pub fn record_retry(&self, class: RetryMetricClass) {
        let counter = match class {
            RetryMetricClass::Transport => &self.retry_transport,
            RetryMetricClass::Timeout => &self.retry_timeout,
            RetryMetricClass::RateLimit => &self.retry_rate_limit,
            RetryMetricClass::ProviderClient => &self.retry_provider_client,
            RetryMetricClass::ProviderServer => &self.retry_provider_server,
            RetryMetricClass::MalformedResponse => &self.retry_malformed_response,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_model_fallback(&self) {
        self.model_fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tool_protocol(&self, class: ToolProtocolMetricClass, count: u64) {
        if count == 0 {
            return;
        }
        let counter = match class {
            ToolProtocolMetricClass::NativeToolCall => &self.native_tool_calls,
            ToolProtocolMetricClass::EncodedCandidate => &self.encoded_fallback_candidates,
            ToolProtocolMetricClass::EncodedNativeRetry => &self.encoded_native_retries,
            ToolProtocolMetricClass::EncodedFallbackToolCall => &self.encoded_fallback_tool_calls,
            ToolProtocolMetricClass::EncodedFallbackRejection => &self.encoded_fallback_rejections,
            ToolProtocolMetricClass::LiteralMarkerSuppression => &self.literal_marker_suppressions,
        };
        counter.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_proxy_restart_attempt(&self) {
        self.proxy_restart_attempts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_proxy_restart_success(&self) {
        self.proxy_restart_successes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_proxy_restart_failure(&self) {
        self.proxy_restart_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_search(&self, provider: SearchMetricProvider, outcome: SearchMetricOutcome) {
        match provider {
            SearchMetricProvider::Tavily => self.search_tavily.record(outcome),
            SearchMetricProvider::Exa => self.search_exa.record(outcome),
            SearchMetricProvider::Serper => self.search_serper.record(outcome),
            SearchMetricProvider::SearXng => self.search_searxng.record(outcome),
            SearchMetricProvider::DuckDuckGo => self.search_duckduckgo.record(outcome),
        }
    }

    fn begin_request(&self) -> ActiveRequestGuard<'_> {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        let active = self.active_requests.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_active_requests
            .fetch_max(active, Ordering::Relaxed);
        ActiveRequestGuard { metrics: self }
    }

    fn record_response(&self, status: u16, elapsed_ms: u64) {
        match status {
            200..=299 => {
                self.responses_2xx.fetch_add(1, Ordering::Relaxed);
            }
            400..=499 => {
                self.responses_4xx.fetch_add(1, Ordering::Relaxed);
            }
            500..=599 => {
                self.responses_5xx.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.latency_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);
        self.latency_max_ms.fetch_max(elapsed_ms, Ordering::Relaxed);
    }

    fn next_request_id(&self) -> String {
        self.generated_request_ids.fetch_add(1, Ordering::Relaxed);
        let sequence = self.request_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        format!("req-{:x}-{sequence:x}", std::process::id())
    }
}

#[derive(Debug, Clone, Copy)]
enum StreamOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug)]
pub struct StreamMetricsGuard {
    metrics: Arc<Metrics>,
    outcome: StreamOutcome,
    finished: bool,
}

impl StreamMetricsGuard {
    pub fn completed(&mut self) {
        self.outcome = StreamOutcome::Completed;
        self.finish();
    }

    pub fn cancelled(&mut self) {
        self.outcome = StreamOutcome::Cancelled;
        self.finish();
    }

    pub fn failed(&mut self) {
        self.outcome = StreamOutcome::Failed;
        self.finish();
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.metrics.active_streams.fetch_sub(1, Ordering::AcqRel);
        match self.outcome {
            StreamOutcome::Completed => {
                self.metrics
                    .streams_completed
                    .fetch_add(1, Ordering::Relaxed);
            }
            StreamOutcome::Cancelled => {
                self.metrics
                    .streams_cancelled
                    .fetch_add(1, Ordering::Relaxed);
            }
            StreamOutcome::Failed => {
                self.metrics.streams_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for StreamMetricsGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

struct ActiveRequestGuard<'a> {
    metrics: &'a Metrics,
}

impl Drop for ActiveRequestGuard<'_> {
    fn drop(&mut self) {
        self.metrics.active_requests.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone)]
pub struct RequestId(pub String);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RequestCompletionLog {
    request_id: String,
    method: String,
    path: String,
    status: u16,
    elapsed_ms: u64,
}

fn request_completion_log(
    request_id: String,
    method: impl ToString,
    path: String,
    status: u16,
    elapsed_ms: u64,
) -> RequestCompletionLog {
    RequestCompletionLog {
        request_id,
        method: method.to_string(),
        path,
        status,
        elapsed_ms,
    }
}

pub async fn request_observability_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let _guard = state.metrics.begin_request();
    let header_name = state
        .config
        .observability
        .request_id_header
        .parse::<HeaderName>()
        .unwrap_or_else(|_| HeaderName::from_static("x-request-id"));

    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let request_id = request
        .headers()
        .get(&header_name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_request_id(value))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| state.metrics.next_request_id());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(header_name, value);
    }
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let status = response.status().as_u16();
    state.metrics.record_response(status, elapsed_ms);
    let log = request_completion_log(request_id, method, path, status, elapsed_ms);
    tracing::info!(
        request_id = %log.request_id,
        method = %log.method,
        path = %log.path,
        status = log.status,
        elapsed_ms = log.elapsed_ms,
        "http request completed"
    );
    response
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',' && byte != b';')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    #[test]
    fn rejects_unsafe_or_oversized_request_ids() {
        assert!(valid_request_id("client-123"));
        assert!(!valid_request_id("has space"));
        assert!(!valid_request_id("a,b"));
        assert!(!valid_request_id(&"x".repeat(129)));
    }

    #[test]
    fn request_log_record_has_a_strict_secret_free_field_whitelist() {
        let log = request_completion_log(
            "capture-123".to_string(),
            "POST",
            "/v1/messages".to_string(),
            200,
            42,
        );
        let encoded = serde_json::to_value(&log).unwrap();
        assert_eq!(encoded["request_id"], "capture-123");
        assert_eq!(encoded["method"], "POST");
        assert_eq!(encoded["path"], "/v1/messages");
        assert_eq!(encoded["status"], 200);
        assert_eq!(encoded["elapsed_ms"], 42);
        assert_eq!(encoded.as_object().unwrap().len(), 5);
        assert!(encoded.get("authorization").is_none());
        assert!(encoded.get("headers").is_none());
        assert!(encoded.get("body").is_none());
    }

    #[test]
    fn operational_counters_have_stable_terminal_semantics() {
        let metrics = Arc::new(Metrics::default());
        {
            let mut stream = metrics.begin_stream();
            stream.completed();
        }
        {
            let mut stream = metrics.begin_stream();
            stream.cancelled();
        }
        {
            let _stream = metrics.begin_stream();
        }
        metrics.record_retry(RetryMetricClass::RateLimit);
        metrics.record_model_fallback();
        metrics.record_tool_protocol(ToolProtocolMetricClass::NativeToolCall, 2);
        metrics.record_tool_protocol(ToolProtocolMetricClass::EncodedCandidate, 3);
        metrics.record_tool_protocol(ToolProtocolMetricClass::EncodedNativeRetry, 1);
        metrics.record_tool_protocol(ToolProtocolMetricClass::EncodedFallbackToolCall, 1);
        metrics.record_tool_protocol(ToolProtocolMetricClass::EncodedFallbackRejection, 1);
        metrics.record_tool_protocol(ToolProtocolMetricClass::LiteralMarkerSuppression, 1);
        metrics.record_proxy_restart_attempt();
        metrics.record_proxy_restart_failure();
        metrics.record_search(SearchMetricProvider::Exa, SearchMetricOutcome::Success);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.streams_started, 3);
        assert_eq!(snapshot.streams_completed, 1);
        assert_eq!(snapshot.streams_cancelled, 1);
        assert_eq!(snapshot.streams_failed, 1);
        assert_eq!(snapshot.active_streams, 0);
        assert_eq!(snapshot.retry_rate_limit, 1);
        assert_eq!(snapshot.model_fallbacks, 1);
        assert_eq!(snapshot.native_tool_calls, 2);
        assert_eq!(snapshot.encoded_fallback_candidates, 3);
        assert_eq!(snapshot.encoded_native_retries, 1);
        assert_eq!(snapshot.encoded_fallback_tool_calls, 1);
        assert_eq!(snapshot.encoded_fallback_rejections, 1);
        assert_eq!(snapshot.literal_marker_suppressions, 1);
        assert_eq!(snapshot.proxy_restart_attempts, 1);
        assert_eq!(snapshot.proxy_restart_failures, 1);
        assert_eq!(snapshot.search_exa.successes, 1);
    }

    #[tokio::test]
    async fn middleware_echoes_valid_id_and_records_metrics() {
        let state = AppState::new(BridgeConfig {
            primary_proxies: None,
            warm_standby_proxies: None,
            ..Default::default()
        });
        let app = Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                request_observability_middleware,
            ))
            .with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ok")
                    .header("x-request-id", "client-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()["x-request-id"], "client-123");
        let snapshot = state.metrics.snapshot();
        assert_eq!(snapshot.requests_total, 1);
        assert_eq!(snapshot.responses_2xx, 1);
        assert_eq!(snapshot.active_requests, 0);
    }
}
