//! Upstream response wrapper that owns an egress lease for the body lifetime.

use crate::proxy_pool::EgressLease;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

#[derive(Debug)]
pub(crate) struct LeasedResponse {
    response: reqwest::Response,
    lease: Option<EgressLease>,
}

impl LeasedResponse {
    pub fn new(response: reqwest::Response, lease: Option<EgressLease>) -> Self {
        Self { response, lease }
    }

    pub fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub fn proxy_index(&self) -> Option<usize> {
        self.lease.as_ref().map(EgressLease::index)
    }

    pub async fn text(self) -> Result<String, reqwest::Error> {
        let Self { response, lease } = self;
        let _lease = lease;
        response.text().await
    }

    pub fn bytes_stream(
        self,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>> {
        let Self { response, lease } = self;
        let mut stream = response.bytes_stream();
        Box::pin(async_stream::stream! {
            let _lease = lease;
            while let Some(item) = stream.next().await {
                yield item;
            }
        })
    }
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
    use crate::proxy_pool::ProxyPool;
    use futures_util::StreamExt;
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
    async fn text_body_holds_lease_until_consumed() {
        let pool = Arc::new(ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]));
        let lease = pool.begin_lease(0).expect("lease");
        let response = reqwest::get(one_response_server(b"hello").await)
            .await
            .expect("response");
        let leased = LeasedResponse::new(response, Some(lease));
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
        let mut stream = LeasedResponse::new(response, Some(lease)).bytes_stream();
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        let _ = stream.next().await.expect("chunk").expect("bytes");
        assert_eq!(pool.proxies[0].active_request_count(), 1);
        drop(stream);
        assert_eq!(pool.proxies[0].active_request_count(), 0);
    }
}
