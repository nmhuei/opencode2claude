use super::{probe_exit_identity, ExitIdentity};
use async_trait::async_trait;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationFailure {
    Transport(String),
    Identity(String),
    Route(String),
}

impl fmt::Display for VerificationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "transport verification failed: {message}"),
            Self::Identity(message) => write!(f, "identity verification failed: {message}"),
            Self::Route(message) => write!(f, "route verification failed: {message}"),
        }
    }
}

#[async_trait]
pub trait ProxyVerifier: Send + Sync + fmt::Debug {
    async fn verify_transport(
        &self,
        client: &reqwest::Client,
        timeout: Duration,
    ) -> Result<(), String>;

    async fn verify_identity(
        &self,
        client: &reqwest::Client,
        endpoints: &[String],
        timeout: Duration,
    ) -> Result<ExitIdentity, String>;

    async fn verify_route(
        &self,
        client: &reqwest::Client,
        upstream_base_url: &str,
        timeout: Duration,
    ) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct LiveProxyVerifier;

#[async_trait]
impl ProxyVerifier for LiveProxyVerifier {
    async fn verify_transport(
        &self,
        client: &reqwest::Client,
        _timeout: Duration,
    ) -> Result<(), String> {
        let response = client
            .get("https://cloudflare.com/cdn-cgi/trace")
            .send()
            .await
            .map_err(|error| format!("HTTP-through-proxy request failed: {error}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "HTTP-through-proxy returned HTTP {}",
                response.status()
            ))
        }
    }

    async fn verify_identity(
        &self,
        client: &reqwest::Client,
        endpoints: &[String],
        _timeout: Duration,
    ) -> Result<ExitIdentity, String> {
        probe_exit_identity(client, endpoints).await
    }

    async fn verify_route(
        &self,
        client: &reqwest::Client,
        upstream_base_url: &str,
        _timeout: Duration,
    ) -> Result<(), String> {
        let url = format!("{}/models", upstream_base_url.trim_end_matches('/'));
        client
            .get(url)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| format!("upstream route probe failed: {error}"))
    }
}

pub async fn verify_candidate(
    verifier: &dyn ProxyVerifier,
    client: &reqwest::Client,
    identity_endpoints: &[String],
    upstream_base_url: &str,
    timeout: Duration,
) -> Result<ExitIdentity, VerificationFailure> {
    verify_transport_stage(verifier, client, timeout).await?;
    let identity = verify_identity_stage(verifier, client, identity_endpoints, timeout).await?;
    verify_route_stage(verifier, client, upstream_base_url, timeout).await?;
    Ok(identity)
}

async fn verify_transport_stage(
    verifier: &dyn ProxyVerifier,
    client: &reqwest::Client,
    timeout: Duration,
) -> Result<(), VerificationFailure> {
    tokio::time::timeout(timeout, verifier.verify_transport(client, timeout))
        .await
        .map_err(|_| VerificationFailure::Transport(timeout_message("transport", timeout)))?
        .map_err(VerificationFailure::Transport)
}

async fn verify_identity_stage(
    verifier: &dyn ProxyVerifier,
    client: &reqwest::Client,
    endpoints: &[String],
    timeout: Duration,
) -> Result<ExitIdentity, VerificationFailure> {
    tokio::time::timeout(
        timeout,
        verifier.verify_identity(client, endpoints, timeout),
    )
    .await
    .map_err(|_| VerificationFailure::Identity(timeout_message("identity", timeout)))?
    .map_err(VerificationFailure::Identity)
}

async fn verify_route_stage(
    verifier: &dyn ProxyVerifier,
    client: &reqwest::Client,
    upstream_base_url: &str,
    timeout: Duration,
) -> Result<(), VerificationFailure> {
    tokio::time::timeout(
        timeout,
        verifier.verify_route(client, upstream_base_url, timeout),
    )
    .await
    .map_err(|_| VerificationFailure::Route(timeout_message("route", timeout)))?
    .map_err(VerificationFailure::Route)
}

fn timeout_message(stage: &str, timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        format!(
            "{stage} verification timed out after {}s",
            timeout.as_secs()
        )
    } else {
        format!(
            "{stage} verification timed out after {}ms",
            timeout.as_millis()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug)]
    struct FakeVerifier {
        transport_error: Option<String>,
        identity_error: Option<String>,
        route_error: Option<String>,
        delay: Duration,
        transport_calls: Arc<AtomicUsize>,
        identity_calls: Arc<AtomicUsize>,
        route_calls: Arc<AtomicUsize>,
    }

    impl FakeVerifier {
        fn success() -> Self {
            Self {
                transport_error: None,
                identity_error: None,
                route_error: None,
                delay: Duration::ZERO,
                transport_calls: Arc::new(AtomicUsize::new(0)),
                identity_calls: Arc::new(AtomicUsize::new(0)),
                route_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn identity_error(message: &str) -> Self {
            let mut verifier = Self::success();
            verifier.identity_error = Some(message.to_string());
            verifier
        }

        fn transport_error(message: &str) -> Self {
            let mut verifier = Self::success();
            verifier.transport_error = Some(message.to_string());
            verifier
        }

        fn route_error(message: &str) -> Self {
            let mut verifier = Self::success();
            verifier.route_error = Some(message.to_string());
            verifier
        }

        fn with_delay(delay: Duration) -> Self {
            let mut verifier = Self::success();
            verifier.delay = delay;
            verifier
        }

        fn identity_calls(&self) -> usize {
            self.identity_calls.load(Ordering::Relaxed)
        }

        fn route_calls(&self) -> usize {
            self.route_calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl ProxyVerifier for FakeVerifier {
        async fn verify_transport(
            &self,
            _client: &reqwest::Client,
            _timeout: Duration,
        ) -> Result<(), String> {
            self.transport_calls.fetch_add(1, Ordering::Relaxed);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.transport_error.clone().map_or(Ok(()), Err)
        }

        async fn verify_identity(
            &self,
            _client: &reqwest::Client,
            _endpoints: &[String],
            _timeout: Duration,
        ) -> Result<ExitIdentity, String> {
            self.identity_calls.fetch_add(1, Ordering::Relaxed);
            self.identity_error.clone().map_or_else(
                || {
                    Ok(ExitIdentity {
                        public_ip: "203.0.113.10".to_string(),
                        provider: Some("test".to_string()),
                        colo: Some("TST".to_string()),
                        verified_at_unix_secs: 1,
                    })
                },
                Err,
            )
        }

        async fn verify_route(
            &self,
            _client: &reqwest::Client,
            _upstream_base_url: &str,
            _timeout: Duration,
        ) -> Result<(), String> {
            self.route_calls.fetch_add(1, Ordering::Relaxed);
            self.route_error.clone().map_or(Ok(()), Err)
        }
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn endpoints() -> Vec<String> {
        vec!["https://identity.invalid".to_string()]
    }

    #[tokio::test]
    async fn staged_verification_never_reaches_route_after_identity_failure() {
        let verifier = FakeVerifier::identity_error("warp=off");
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(result, Err(VerificationFailure::Identity(_))));
        assert_eq!(verifier.route_calls(), 0);
    }

    #[tokio::test]
    async fn transport_failure_short_circuits_identity_and_route() {
        let verifier = FakeVerifier::transport_error("socks dead");
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(result, Err(VerificationFailure::Transport(_))));
        assert_eq!(verifier.identity_calls(), 0);
        assert_eq!(verifier.route_calls(), 0);
    }

    #[tokio::test]
    async fn route_failure_is_reported_after_identity_passes() {
        let verifier = FakeVerifier::route_error("tls failed");
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(result, Err(VerificationFailure::Route(_))));
        assert_eq!(verifier.identity_calls(), 1);
        assert_eq!(verifier.route_calls(), 1);
    }

    #[tokio::test]
    async fn stage_timeout_is_bounded() {
        let verifier = FakeVerifier::with_delay(Duration::from_secs(60));
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(10),
        )
        .await;
        assert!(
            matches!(result, Err(VerificationFailure::Transport(message)) if message.contains("10ms"))
        );
    }

    #[tokio::test]
    async fn staged_verification_full_pass_returns_identity() {
        let verifier = FakeVerifier::success();
        let identity = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await
        .expect("verification");
        assert_eq!(identity.public_ip, "203.0.113.10");
        assert_eq!(verifier.route_calls(), 1);
    }
}
