use super::super::{probe_exit_identity, ExitIdentity};
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

pub(crate) async fn verify_transport_stage(
    verifier: &dyn ProxyVerifier,
    client: &reqwest::Client,
    timeout: Duration,
) -> Result<(), VerificationFailure> {
    tokio::time::timeout(timeout, verifier.verify_transport(client, timeout))
        .await
        .map_err(|_| VerificationFailure::Transport(timeout_message("transport", timeout)))?
        .map_err(VerificationFailure::Transport)
}

pub(crate) async fn verify_identity_stage(
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

pub(crate) async fn verify_route_stage(
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
