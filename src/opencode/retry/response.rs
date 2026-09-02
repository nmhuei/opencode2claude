//! Upstream response wrapper that owns an egress lease for the body lifetime.

use crate::provider::capacity::{DispatchLease, TokenUsage};
use crate::proxy_pool::{EgressLease, RouteMetadata};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

#[derive(Debug)]
pub(crate) struct LeasedResponse {
    response: reqwest::Response,
    lease: Option<EgressLease>,
    capacity_lease: Option<DispatchLease>,
    route: RouteMetadata,
}

impl LeasedResponse {
    pub fn new(
        response: reqwest::Response,
        lease: Option<EgressLease>,
        route: RouteMetadata,
    ) -> Self {
        Self::new_with_capacity(response, lease, route, None)
    }

    pub fn new_with_capacity(
        response: reqwest::Response,
        lease: Option<EgressLease>,
        route: RouteMetadata,
        capacity_lease: Option<DispatchLease>,
    ) -> Self {
        Self {
            response,
            lease,
            capacity_lease,
            route,
        }
    }

    pub fn route(&self) -> &RouteMetadata {
        &self.route
    }

    pub fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub fn proxy_index(&self) -> Option<usize> {
        self.lease.as_ref().map(EgressLease::index)
    }

    pub async fn text(self) -> Result<String, reqwest::Error> {
        let Self {
            response,
            lease,
            mut capacity_lease,
            route: _,
        } = self;
        let _lease = lease;
        let headers = response.headers().clone();
        let successful = response.status().is_success();
        let result = response.text().await;
        if let Some(capacity) = capacity_lease.as_mut() {
            capacity.observe_headers(&headers, std::time::Instant::now());
            match (&result, successful) {
                (Ok(body), true) => capacity.observe_success(parse_token_usage(body)),
                (Err(_), _) => capacity.fail(
                    crate::provider::adapters::FailureClass::Transport,
                    None,
                    std::time::Instant::now(),
                ),
                _ => {}
            }
        }
        result
    }

    pub(crate) async fn bounded_bytes(
        self,
        max_bytes: usize,
    ) -> Result<Vec<u8>, crate::error::BridgeError> {
        let Self {
            response,
            lease,
            mut capacity_lease,
            route: _,
        } = self;
        let _lease = lease;
        let headers = response.headers().clone();
        let successful = response.status().is_success();
        let mut stream = response.bytes_stream();
        let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
        if let Some(capacity) = capacity_lease.as_mut() {
            capacity.observe_headers(&headers, std::time::Instant::now());
        }
        while let Some(item) = stream.next().await {
            let chunk = item.map_err(|error| {
                if let Some(capacity) = capacity_lease.as_mut() {
                    capacity.fail(
                        crate::provider::adapters::FailureClass::Transport,
                        None,
                        std::time::Instant::now(),
                    );
                }
                crate::error::BridgeError::UpstreamError(format!(
                    "Failed reading upstream response: {error}"
                ))
            })?;
            if body.len().saturating_add(chunk.len()) > max_bytes {
                if successful {
                    if let Some(capacity) = capacity_lease.as_mut() {
                        capacity.observe_success(None);
                    }
                }
                return Err(crate::error::BridgeError::UpstreamError(format!(
                    "Upstream response exceeded configured limit of {max_bytes} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        if successful {
            if let Some(capacity) = capacity_lease.as_mut() {
                capacity.observe_success(parse_token_usage(&String::from_utf8_lossy(&body)));
            }
        }
        Ok(body)
    }

    pub fn bytes_stream(
        self,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>> {
        let Self {
            response,
            lease,
            mut capacity_lease,
            route: _,
        } = self;
        let headers = response.headers().clone();
        let successful = response.status().is_success();
        let mut stream = response.bytes_stream();
        Box::pin(async_stream::stream! {
            let _lease = lease;
            if let Some(capacity) = capacity_lease.as_mut() {
                capacity.observe_headers(&headers, std::time::Instant::now());
            }
            while let Some(item) = stream.next().await {
                match item {
                    Ok(bytes) => yield Ok(bytes),
                    Err(error) => {
                        if let Some(capacity) = capacity_lease.as_mut() {
                            capacity.fail(
                                crate::provider::adapters::FailureClass::Transport,
                                None,
                                std::time::Instant::now(),
                            );
                        }
                        yield Err(error);
                        return;
                    }
                }
            }
            if successful {
                if let Some(capacity) = capacity_lease.as_mut() {
                    capacity.observe_success(None);
                }
            }
        })
    }
}

fn parse_token_usage(body: &str) -> Option<TokenUsage> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let usage = value.get("usage")?;
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(serde_json::Value::as_u64)?;
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(TokenUsage {
        input_tokens,
        output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use crate::proxy_pool::ProxyPool;

    #[test]
    fn lease_is_held_until_wrapper_is_dropped() {
        let pool = ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]);
        let lease = pool.begin_lease(0).expect("lease");
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        drop(lease);
        assert_eq!(pool.proxies[0].active_request_count(), 0);
    }
}

#[cfg(test)]
mod body_lifetime_tests {
    use super::LeasedResponse;
    use crate::provider::types::CapacityDemand;
    use crate::provider::ProviderRegistry;
    use crate::proxy_pool::{ProxyPool, RouteKind, RouteMetadata};
    use futures_util::StreamExt;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn one_response_server(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.expect("head");
            socket.write_all(body).await.expect("body");
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn response_exposes_route_metadata_while_holding_lease() {
        let pool = Arc::new(ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]));
        let lease = pool.begin_lease(0).expect("lease");
        let response = reqwest::get(one_response_server(b"hello").await)
            .await
            .expect("response");
        let route = RouteMetadata {
            kind: RouteKind::Proxy,
            proxy_node: Some("opencode-warp-1".to_string()),
        };
        let leased = LeasedResponse::new(response, Some(lease), route.clone());
        assert_eq!(leased.route(), &route);
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        assert_eq!(leased.text().await.expect("text"), "hello");
        assert_eq!(pool.proxies[0].active_request_count(), 0);
    }

    #[tokio::test]
    async fn text_body_holds_lease_until_consumed() {
        let pool = Arc::new(ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]));
        let lease = pool.begin_lease(0).expect("lease");
        let response = reqwest::get(one_response_server(b"hello").await)
            .await
            .expect("response");
        let leased = LeasedResponse::new(
            response,
            Some(lease),
            RouteMetadata {
                kind: RouteKind::Proxy,
                proxy_node: Some("opencode-warp-1".to_string()),
            },
        );
        assert_eq!(leased.proxy_index(), Some(0));
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        assert_eq!(leased.text().await.expect("text"), "hello");
        assert_eq!(pool.proxies[0].active_request_count(), 0);
    }

    #[tokio::test]
    async fn streaming_body_holds_lease_until_stream_drop() {
        let pool = Arc::new(ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]));
        let lease = pool.begin_lease(0).expect("lease");
        let response = reqwest::get(one_response_server(b"stream-body").await)
            .await
            .expect("response");
        let mut stream = LeasedResponse::new(
            response,
            Some(lease),
            RouteMetadata {
                kind: RouteKind::Proxy,
                proxy_node: Some("opencode-warp-1".to_string()),
            },
        )
        .bytes_stream();
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        let _ = stream.next().await.expect("chunk").expect("bytes");
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        drop(stream);
        assert_eq!(pool.proxies[0].active_request_count(), 0);
    }

    #[tokio::test]
    async fn response_holds_provider_capacity_until_body_is_consumed() {
        let registry = ProviderRegistry::from_legacy(
            "http://127.0.0.1:1",
            "model",
            Some(vec!["secret".into()]),
        );
        let scheduler = crate::provider::CapacityScheduler::from_registry(&registry);
        let routes = registry.resolve_routes("legacy-default").expect("routes");
        let capacity = scheduler
            .admit(
                &routes,
                CapacityDemand::new(1, 1),
                &BTreeSet::new(),
                std::time::Instant::now(),
            )
            .expect("capacity lease");
        assert_eq!(scheduler.summary(std::time::Instant::now()).in_flight, 1);
        let response = reqwest::get(one_response_server(b"hello").await)
            .await
            .expect("response");
        let leased = LeasedResponse::new_with_capacity(
            response,
            None,
            RouteMetadata {
                kind: RouteKind::Direct,
                proxy_node: None,
            },
            Some(capacity),
        );
        assert_eq!(leased.text().await.expect("text"), "hello");
        assert_eq!(scheduler.summary(std::time::Instant::now()).in_flight, 0);
    }

    #[tokio::test]
    async fn bounded_body_reports_actual_usage_to_provider_scheduler() {
        let registry = ProviderRegistry::from_legacy(
            "http://127.0.0.1:1",
            "model",
            Some(vec!["secret".into()]),
        );
        let scheduler = crate::provider::CapacityScheduler::from_registry(&registry);
        let routes = registry.resolve_routes("legacy-default").expect("routes");
        let capacity = scheduler
            .admit(
                &routes,
                CapacityDemand::new(1, 1),
                &BTreeSet::new(),
                std::time::Instant::now(),
            )
            .expect("capacity lease");
        let response = reqwest::get(
            one_response_server(br#"{"usage":{"prompt_tokens":12,"completion_tokens":7}}"#).await,
        )
        .await
        .expect("response");
        let leased = LeasedResponse::new_with_capacity(
            response,
            None,
            RouteMetadata {
                kind: RouteKind::Direct,
                proxy_node: None,
            },
            Some(capacity),
        );
        let body = leased.bounded_bytes(1024).await.expect("body");
        assert!(body.starts_with(br#"{"usage"#));
        let summary = scheduler.summary(std::time::Instant::now());
        assert_eq!(summary.observed_input_tokens, 12);
        assert_eq!(summary.observed_output_tokens, 7);
    }

    #[test]
    fn usage_parser_accepts_openai_and_responses_shapes() {
        let chat =
            super::parse_token_usage(r#"{"usage":{"prompt_tokens":12,"completion_tokens":7}}"#)
                .expect("chat usage");
        assert_eq!(chat.input_tokens, 12);
        assert_eq!(chat.output_tokens, 7);
        let responses =
            super::parse_token_usage(r#"{"usage":{"input_tokens":9,"output_tokens":4}}"#)
                .expect("responses usage");
        assert_eq!(responses.input_tokens, 9);
        assert_eq!(responses.output_tokens, 4);
        assert!(super::parse_token_usage(r#"{"usage":{"prompt_tokens":"bad"}}"#).is_none());
    }
}
