//! Request correlation and bounded in-process operational counters.

use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

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
    state
        .metrics
        .record_response(response.status().as_u16(), elapsed_ms);
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
