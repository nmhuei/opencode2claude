use super::{
    probe_exit_identity, ExitIdentity, ProxyPool, ProxySubsystemPhase, ProxySubsystemStatus,
};
use crate::config::BridgeConfig;
use crate::docker::{ContainerRuntime, ProxySpec};
use crate::observability::Metrics;
use crate::workers::WorkerContext;
use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

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

#[derive(Debug, Clone)]
struct ReconcileCandidate {
    index: usize,
    id: String,
    port: u16,
    client: reqwest::Client,
}

/// Drive the proxy subsystem lifecycle for every proxy-backed egress mode
/// (`proxy` and `hybrid`).
///
/// The loop repeatedly runs a full reconcile cycle: bootstrap configured
/// candidates through the container runtime, then stage transport, identity,
/// and route verification, and finally publish readiness by transitioning
/// `subsystem` (Starting → TransportVerifying → IdentityVerifying →
/// RouteVerifying → Ready, or Degraded with a bounded error + backoff on
/// failure). The cycle body is egress-mode independent — it only ever reads
/// pool/config state — so pure-proxy deployments get the same honest snapshot
/// consumers see in hybrid mode.
///
/// The `hybrid_` prefix is retained from when only hybrid mode spawned this
/// worker; renaming it would churn callers without behavioral value.
pub async fn hybrid_proxy_reconciler(
    pool: Arc<RwLock<ProxyPool>>,
    subsystem: Arc<RwLock<ProxySubsystemStatus>>,
    runtime: Arc<dyn ContainerRuntime>,
    verifier: Arc<dyn ProxyVerifier>,
    config: Arc<BridgeConfig>,
    metrics: Arc<Metrics>,
    context: WorkerContext,
) -> Result<(), String> {
    let mut failure_attempt = 0_u32;
    loop {
        context.heartbeat();
        let cycle = tokio::select! {
            _ = context.cancellation().cancelled() => return Ok(()),
            result = tokio::time::timeout(
                config.egress.bootstrap_timeout,
                reconcile_once(
                    &pool,
                    &subsystem,
                    runtime.as_ref(),
                    verifier.as_ref(),
                    &config,
                    &metrics,
                ),
            ) => match result {
                Ok(result) => result,
                Err(_) => Err(format!(
                    "proxy bootstrap attempt timed out after {}s",
                    config.egress.bootstrap_timeout.as_secs()
                )),
            },
        };

        match cycle {
            Ok(()) => {
                failure_attempt = failure_attempt_after_cycle(failure_attempt, true);
                if sleep_with_heartbeat(&context, config.egress.health_interval).await {
                    return Ok(());
                }
            }
            Err(error) => {
                let backoff = recovery_backoff(
                    failure_attempt,
                    config.egress.recovery_backoff_max,
                    unix_now().wrapping_add(u64::from(failure_attempt)),
                );
                failure_attempt = failure_attempt_after_cycle(failure_attempt, false);
                let backoff_until = unix_now().saturating_add(backoff.as_secs());
                subsystem
                    .write()
                    .await
                    .mark_degraded(error, Some(backoff_until));
                metrics.record_proxy_state_transition();
                if sleep_with_heartbeat(&context, backoff).await {
                    return Ok(());
                }
            }
        }
    }
}

async fn reconcile_once(
    pool: &Arc<RwLock<ProxyPool>>,
    subsystem: &Arc<RwLock<ProxySubsystemStatus>>,
    runtime: &dyn ContainerRuntime,
    verifier: &dyn ProxyVerifier,
    config: &BridgeConfig,
    metrics: &Metrics,
) -> Result<(), String> {
    subsystem
        .write()
        .await
        .transition(ProxySubsystemPhase::Starting, None);
    metrics.record_proxy_state_transition();

    let candidates = {
        let guard = pool.read().await;
        guard
            .proxies
            .iter()
            .enumerate()
            .map(|(index, node)| ReconcileCandidate {
                index,
                id: node.id.clone(),
                port: node.port,
                client: node.client.clone(),
            })
            .collect::<Vec<_>>()
    };
    if candidates.is_empty() {
        return Err("proxy pool has no configured candidates".to_string());
    }

    let mut bootstrapped = Vec::new();
    let mut bootstrap_error = None;
    for candidate in &candidates {
        metrics.record_proxy_bootstrap_attempt();
        match ensure_candidate(runtime, candidate, config).await {
            Ok(()) => {
                metrics.record_proxy_bootstrap_success();
                bootstrapped.push(candidate.clone());
            }
            Err(error) => {
                metrics.record_proxy_bootstrap_failure();
                if bootstrap_error.is_none() {
                    bootstrap_error = Some(error);
                }
                pool.write().await.record_failure(candidate.index);
            }
        }
    }
    if bootstrapped.is_empty() {
        return Err(
            bootstrap_error.unwrap_or_else(|| "no proxy candidate could be started".to_string())
        );
    }

    subsystem
        .write()
        .await
        .transition(ProxySubsystemPhase::TransportVerifying, None);
    metrics.record_proxy_state_transition();
    let mut transport_ready = Vec::new();
    let mut first_error = None;
    for candidate in &bootstrapped {
        match verify_transport_stage(verifier, &candidate.client, config.egress.verify_timeout)
            .await
        {
            Ok(()) => transport_ready.push(candidate.clone()),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
                pool.write().await.record_failure(candidate.index);
            }
        }
    }
    if transport_ready.is_empty() {
        return Err(
            first_error.unwrap_or_else(|| "no proxy passed transport verification".to_string())
        );
    }

    subsystem
        .write()
        .await
        .transition(ProxySubsystemPhase::IdentityVerifying, None);
    metrics.record_proxy_state_transition();
    let mut identity_results = Vec::with_capacity(transport_ready.len());
    let mut identity_successes = 0_usize;
    for candidate in &transport_ready {
        match verify_identity_stage(
            verifier,
            &candidate.client,
            &config.egress.identity_endpoints,
            config.egress.verify_timeout,
        )
        .await
        {
            Ok(identity) => {
                identity_successes += 1;
                identity_results.push((candidate.index, candidate.id.clone(), Ok(identity)));
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
                pool.write().await.record_failure(candidate.index);
                identity_results.push((
                    candidate.index,
                    candidate.id.clone(),
                    Err(error.to_string()),
                ));
            }
        }
    }
    pool.write().await.apply_identity_results(identity_results);
    if identity_successes == 0 {
        return Err(
            first_error.unwrap_or_else(|| "no proxy passed identity verification".to_string())
        );
    }

    let route_candidates = {
        let guard = pool.read().await;
        guard
            .proxies
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.is_closed_and_healthy()
                    && node
                        .exit_identity
                        .as_ref()
                        .is_some_and(|identity| identity.is_fresh(config.egress.identity_ttl))
            })
            .map(|(index, node)| ReconcileCandidate {
                index,
                id: node.id.clone(),
                port: node.port,
                client: node.client.clone(),
            })
            .collect::<Vec<_>>()
    };
    if route_candidates.is_empty() {
        return Err(
            "no unique healthy proxy exit remained after identity verification".to_string(),
        );
    }

    subsystem
        .write()
        .await
        .transition(ProxySubsystemPhase::RouteVerifying, None);
    metrics.record_proxy_state_transition();
    let mut route_successes = 0_usize;
    for candidate in route_candidates {
        match verify_route_stage(
            verifier,
            &candidate.client,
            &config.retry.upstream_base_url,
            config.egress.verify_timeout,
        )
        .await
        {
            Ok(()) => {
                route_successes += 1;
                pool.write().await.record_success(candidate.index);
            }
            Err(error) => {
                metrics.record_proxy_route_probe_failure();
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
                pool.write().await.record_failure(candidate.index);
            }
        }
    }
    if route_successes == 0 {
        return Err(first_error.unwrap_or_else(|| "no proxy passed route verification".to_string()));
    }

    if !pool.read().await.egress_ready(
        config.egress.minimum_unique_exit_ips,
        config.egress.identity_ttl,
    ) {
        return Err(format!(
            "proxy verification passed but fewer than {} unique eligible exit(s) are ready",
            config.egress.minimum_unique_exit_ips
        ));
    }

    subsystem.write().await.mark_ready();
    metrics.record_proxy_state_transition();
    Ok(())
}

async fn ensure_candidate(
    runtime: &dyn ContainerRuntime,
    candidate: &ReconcileCandidate,
    config: &BridgeConfig,
) -> Result<(), String> {
    let spec = ProxySpec::new(candidate.port, config.runtime.warp_image.clone())
        .map_err(|error| format!("proxy {} spec is invalid: {error}", candidate.id))?;
    let timeout = config
        .egress
        .verify_timeout
        .min(config.egress.bootstrap_timeout);
    let state = tokio::time::timeout(timeout, runtime.inspect(&spec))
        .await
        .map_err(|_| format!("Docker inspect timed out for {}", candidate.id))?
        .map_err(|error| format!("Docker inspect failed for {}: {error}", candidate.id))?;

    if !state.exists {
        tokio::time::timeout(timeout, runtime.create_missing(&spec))
            .await
            .map_err(|_| format!("Docker create timed out for {}", candidate.id))?
            .map_err(|error| format!("Docker create failed for {}: {error}", candidate.id))?;
    } else if !state.running {
        tokio::time::timeout(timeout, runtime.start_managed(&spec))
            .await
            .map_err(|_| format!("Docker start timed out for {}", candidate.id))?
            .map_err(|error| format!("Docker start failed for {}: {error}", candidate.id))?;
    }
    Ok(())
}

fn failure_attempt_after_cycle(current: u32, success: bool) -> u32 {
    if success {
        0
    } else {
        current.saturating_add(1)
    }
}

pub(crate) fn recovery_backoff(attempt: u32, max: Duration, jitter_seed: u64) -> Duration {
    let seconds = match attempt {
        0 => 2,
        1 => 5,
        2 => 10,
        3 => 30,
        _ => 60,
    };
    let base = Duration::from_secs(seconds).min(max);
    if jitter_seed == 0 || base.is_zero() {
        return base;
    }
    let max_jitter_ms = (base.as_millis() / 5).min(u128::from(u64::MAX)) as u64;
    if max_jitter_ms == 0 {
        return base;
    }
    let jitter_ms = jitter_seed % (max_jitter_ms + 1);
    base.saturating_add(Duration::from_millis(jitter_ms))
        .min(max)
}

async fn sleep_with_heartbeat(context: &WorkerContext, duration: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        context.heartbeat();
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline.saturating_duration_since(now);
        let slice = remaining.min(Duration::from_secs(5));
        tokio::select! {
            _ = context.cancellation().cancelled() => return true,
            _ = tokio::time::sleep(slice) => {}
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

#[cfg(test)]
mod reconciler_tests {
    use super::*;
    use crate::config::{BridgeConfig, EgressMode};
    use crate::docker::{
        ContainerRuntime, ContainerState, ContainerSummary, DockerError, DockerResult, ProxySpec,
    };
    use crate::observability::Metrics;
    use crate::proxy_pool::{ProxyPool, ProxySubsystemPhase, ProxySubsystemStatus};
    use crate::workers::WorkerRegistry;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::RwLock;

    #[derive(Debug)]
    struct TestRuntime {
        inspect_delay: Duration,
        inspect_error: bool,
    }

    #[async_trait]
    impl ContainerRuntime for TestRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".to_string())
        }

        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
            if !self.inspect_delay.is_zero() {
                tokio::time::sleep(self.inspect_delay).await;
            }
            if self.inspect_error {
                return Err(DockerError::CommandFailed("docker unavailable".to_string()));
            }
            Ok(ContainerState {
                exists: true,
                running: true,
                has_expected_volume: true,
            })
        }

        async fn create_missing(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }

        async fn recreate_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not recreate".to_string(),
            ))
        }

        async fn remove_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not remove".to_string(),
            ))
        }

        async fn restart_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not restart".to_string(),
            ))
        }

        async fn stop_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not stop".to_string(),
            ))
        }

        async fn start_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }

        async fn logs(&self, _spec: &ProxySpec, _tail: usize) -> DockerResult<String> {
            Ok(String::new())
        }

        async fn list(&self, _specs: &[ProxySpec]) -> DockerResult<Vec<ContainerSummary>> {
            Ok(Vec::new())
        }
    }

    #[derive(Debug, Default)]
    struct AlwaysVerifier;

    #[async_trait]
    impl ProxyVerifier for AlwaysVerifier {
        async fn verify_transport(
            &self,
            _client: &reqwest::Client,
            _timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn verify_identity(
            &self,
            _client: &reqwest::Client,
            _endpoints: &[String],
            _timeout: Duration,
        ) -> Result<ExitIdentity, String> {
            Ok(ExitIdentity {
                public_ip: "203.0.113.10".to_string(),
                provider: Some("test".to_string()),
                colo: Some("TST".to_string()),
                verified_at_unix_secs: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            })
        }

        async fn verify_route(
            &self,
            _client: &reqwest::Client,
            _upstream_base_url: &str,
            _timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn hybrid_config() -> Arc<BridgeConfig> {
        let mut config = BridgeConfig::default();
        config.egress.mode = EgressMode::Hybrid;
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);
        config.egress.active_proxy_count = 1;
        config.egress.verify_timeout = Duration::from_millis(50);
        config.egress.bootstrap_timeout = Duration::from_secs(30);
        config.egress.recovery_backoff_max = Duration::from_secs(120);
        Arc::new(config)
    }

    fn pool(config: &BridgeConfig) -> Arc<RwLock<ProxyPool>> {
        let urls = config
            .primary_proxies
            .iter()
            .flatten()
            .chain(config.warm_standby_proxies.iter().flatten())
            .cloned()
            .collect::<Vec<_>>();
        Arc::new(RwLock::new(ProxyPool::new_with_egress_policy(
            &urls,
            config.egress.active_proxy_count,
            config.egress.require_verified_exit_ip,
            config.egress.identity_ttl,
        )))
    }

    #[tokio::test]
    async fn slow_docker_reconcile_is_cancelled_without_waiting_for_inspect() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime: Arc<dyn ContainerRuntime> = Arc::new(TestRuntime {
            inspect_delay: Duration::from_secs(60),
            inspect_error: false,
        });
        let verifier: Arc<dyn ProxyVerifier> = Arc::new(AlwaysVerifier);
        let metrics = Arc::new(Metrics::default());
        let registry = WorkerRegistry::new();
        let task_pool = pool.clone();
        let task_subsystem = subsystem.clone();
        let task_config = config.clone();
        registry.spawn_critical("test-reconcile", move |context| async move {
            hybrid_proxy_reconciler(
                task_pool,
                task_subsystem,
                runtime,
                verifier,
                task_config,
                metrics,
                context,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry
            .shutdown(Duration::from_millis(250))
            .await
            .expect("cancellation must interrupt slow Docker inspect");
    }

    #[tokio::test]
    async fn docker_unavailable_marks_degraded_and_keeps_worker_alive() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime: Arc<dyn ContainerRuntime> = Arc::new(TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: true,
        });
        let verifier: Arc<dyn ProxyVerifier> = Arc::new(AlwaysVerifier);
        let metrics = Arc::new(Metrics::default());
        let registry = WorkerRegistry::new();
        let task_pool = pool.clone();
        let task_subsystem = subsystem.clone();
        let task_config = config.clone();
        registry.spawn_critical("test-reconcile", move |context| async move {
            hybrid_proxy_reconciler(
                task_pool,
                task_subsystem,
                runtime,
                verifier,
                task_config,
                metrics,
                context,
            )
            .await
        });

        for _ in 0..50 {
            if subsystem.read().await.snapshot().phase == ProxySubsystemPhase::Degraded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let snapshot = subsystem.read().await.snapshot();
        assert_eq!(snapshot.phase, ProxySubsystemPhase::Degraded);
        assert!(snapshot.backoff_until_unix_secs.is_some());
        assert!(snapshot
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("docker unavailable")));

        registry
            .shutdown(Duration::from_millis(250))
            .await
            .expect("worker remains cancellable while backing off");
    }

    #[test]
    fn recovery_backoff_is_bounded_and_nonzero() {
        let max = Duration::from_secs(120);
        assert_eq!(recovery_backoff(0, max, 0), Duration::from_secs(2));
        assert_eq!(recovery_backoff(1, max, 0), Duration::from_secs(5));
        assert_eq!(recovery_backoff(2, max, 0), Duration::from_secs(10));
        assert_eq!(recovery_backoff(3, max, 0), Duration::from_secs(30));
        assert_eq!(recovery_backoff(4, max, 0), Duration::from_secs(60));
        assert_eq!(
            recovery_backoff(99, Duration::from_secs(45), 0),
            Duration::from_secs(45)
        );
    }

    #[tokio::test]
    async fn full_reconcile_cycle_marks_subsystem_ready_without_destructive_lifecycle() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
            .await
            .expect("full verification cycle");

        assert_eq!(
            subsystem.read().await.snapshot().phase,
            ProxySubsystemPhase::Ready
        );
        assert!(subsystem.read().await.is_ready());
    }

    #[tokio::test]
    async fn reconcile_cycle_is_mode_independent_for_pure_proxy_egress() {
        // The reconciler body must drive the identical lifecycle in pure
        // proxy mode (state.rs now spawns it there too) with zero changes.
        let mut raw = (*hybrid_config()).clone();
        raw.egress.mode = EgressMode::Proxy;
        let config = Arc::new(raw);
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
            .await
            .expect("pure proxy verification cycle");

        assert_eq!(
            subsystem.read().await.snapshot().phase,
            ProxySubsystemPhase::Ready
        );
    }

    #[test]
    fn successful_cycle_resets_backoff_to_first_step() {
        let attempt = failure_attempt_after_cycle(4, true);
        assert_eq!(attempt, 0);
        assert_eq!(
            recovery_backoff(attempt, Duration::from_secs(120), 0),
            Duration::from_secs(2)
        );
        assert_eq!(failure_attempt_after_cycle(0, false), 1);
    }

    /// Verifier handing every candidate a distinct fresh exit identity so the
    /// duplicate suppression keeps all candidates eligible for route probing.
    #[derive(Debug)]
    struct DistinctIdentityVerifier {
        identity_calls: Arc<AtomicUsize>,
        route_error: Option<String>,
    }

    impl DistinctIdentityVerifier {
        fn route_failure() -> Self {
            Self {
                identity_calls: Arc::new(AtomicUsize::new(0)),
                route_error: Some("upstream route probe failed".to_string()),
            }
        }
    }

    #[async_trait]
    impl ProxyVerifier for DistinctIdentityVerifier {
        async fn verify_transport(
            &self,
            _client: &reqwest::Client,
            _timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn verify_identity(
            &self,
            _client: &reqwest::Client,
            _endpoints: &[String],
            _timeout: Duration,
        ) -> Result<ExitIdentity, String> {
            let sequence = self.identity_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ExitIdentity {
                public_ip: format!("192.0.2.{}", sequence + 1),
                provider: Some("test".to_string()),
                colo: Some("TST".to_string()),
                verified_at_unix_secs: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            })
        }

        async fn verify_route(
            &self,
            _client: &reqwest::Client,
            _upstream_base_url: &str,
            _timeout: Duration,
        ) -> Result<(), String> {
            match &self.route_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    #[tokio::test]
    async fn successful_reconcile_cycle_counts_bootstrap_and_transitions_exactly_once() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
            .await
            .expect("full verification cycle");

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.proxy_bootstrap_attempts, 2,
            "one bootstrap attempt per configured candidate"
        );
        assert_eq!(snapshot.proxy_bootstrap_successes, 2);
        assert_eq!(snapshot.proxy_bootstrap_failures, 0);
        assert_eq!(
            snapshot.proxy_state_transitions, 5,
            "Starting, TransportVerifying, IdentityVerifying, RouteVerifying, Ready"
        );
        assert_eq!(snapshot.proxy_route_probe_failures, 0);
        assert_eq!(snapshot.proxy_duplicate_exit_events, 0);
    }

    #[tokio::test]
    async fn bootstrap_failure_counts_one_attempt_and_failure_and_skips_later_stages() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: true,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        let result =
            reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics).await;
        assert!(result.is_err(), "unavailable runtime must fail the cycle");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.proxy_bootstrap_attempts, 2);
        assert_eq!(snapshot.proxy_bootstrap_successes, 0);
        assert_eq!(
            snapshot.proxy_bootstrap_failures, 2,
            "each failed container bootstrap counts exactly one failure"
        );
        assert_eq!(
            snapshot.proxy_state_transitions, 1,
            "only Starting is applied before bootstrap fails"
        );
        assert_eq!(snapshot.proxy_route_probe_failures, 0);
    }

    #[tokio::test]
    async fn route_probe_failure_counts_one_event_per_failing_candidate() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = DistinctIdentityVerifier::route_failure();
        let metrics = Metrics::default();

        let result =
            reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics).await;
        assert!(
            result.is_err(),
            "all-failed route probes must fail the cycle"
        );

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.proxy_route_probe_failures, 2,
            "each failing candidate probe is exactly one event"
        );
        assert_eq!(
            snapshot.proxy_state_transitions, 4,
            "Starting through RouteVerifying; no Ready after failure"
        );
        assert_eq!(snapshot.proxy_bootstrap_attempts, 2);
        assert_eq!(snapshot.proxy_bootstrap_successes, 2);
        assert_eq!(snapshot.proxy_bootstrap_failures, 0);
    }

    #[tokio::test]
    async fn repeated_reconcile_cycles_accumulate_counters_additively() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        for _ in 0..2 {
            reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
                .await
                .expect("full verification cycle");
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.proxy_bootstrap_attempts, 4);
        assert_eq!(snapshot.proxy_bootstrap_successes, 4);
        assert_eq!(
            snapshot.proxy_state_transitions, 10,
            "two full cycles never double-charge a single transition application"
        );
    }

    #[tokio::test]
    async fn failed_cycle_marks_degraded_with_exactly_one_extra_transition() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime: Arc<dyn ContainerRuntime> = Arc::new(TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: true,
        });
        let verifier: Arc<dyn ProxyVerifier> = Arc::new(AlwaysVerifier);
        let metrics = Arc::new(Metrics::default());
        let registry = WorkerRegistry::new();
        let task_pool = pool.clone();
        let task_subsystem = subsystem.clone();
        let task_config = config.clone();
        let task_metrics = metrics.clone();
        registry.spawn_critical("test-reconcile", move |context| async move {
            hybrid_proxy_reconciler(
                task_pool,
                task_subsystem,
                runtime,
                verifier,
                task_config,
                task_metrics,
                context,
            )
            .await
        });

        for _ in 0..50 {
            if subsystem.read().await.snapshot().phase == ProxySubsystemPhase::Degraded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        registry
            .shutdown(Duration::from_millis(250))
            .await
            .expect("worker remains cancellable while backing off");

        // The first backoff step exceeds the poll window, so exactly one cycle
        // has completed: Starting + Degraded transitions and one failed
        // container-bootstrap outcome per configured candidate.
        let snapshot = metrics.snapshot();
        assert_eq!(
            subsystem.read().await.snapshot().phase,
            ProxySubsystemPhase::Degraded
        );
        assert_eq!(
            snapshot.proxy_bootstrap_attempts, 2,
            "primary and warm standby are both bootstrapped"
        );
        assert_eq!(snapshot.proxy_bootstrap_failures, 2);
        assert_eq!(snapshot.proxy_bootstrap_successes, 0);
        assert_eq!(
            snapshot.proxy_state_transitions, 2,
            "Starting plus the degraded mark"
        );
    }
}
