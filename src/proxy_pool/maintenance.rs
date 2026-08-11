//! Egress health, circuit transitions, and managed-primary restart queue.

use super::identity::probe_exit_identity;
use super::types::*;
use crate::docker::{ContainerRuntime, DockerError, ProxySpec};
use crate::observability::Metrics;
use crate::workers::WorkerContext;
use futures_util::{stream, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, error, info, warn};

const RATE_LIMIT_ROTATION_RETRY_SECS: u64 = 30;
const MAX_CONCURRENT_PROXY_RECOVERIES: usize = 3;

impl ProxyPool {
    pub fn record_success(&mut self, index: usize) {
        let Some(node) = self.proxies.get_mut(index) else {
            return;
        };
        node.consecutive_failures = 0;
        node.consecutive_successes = node.consecutive_successes.saturating_add(1);

        if (node.circuit == CircuitState::HalfOpen
            || matches!(node.health, HealthState::Recovering | HealthState::Degraded))
            && node.consecutive_successes >= RECOVERY_SUCCESS_COUNT
        {
            mark_node_healthy(node);
            info!(node_id = %node.id, "egress node recovered after successful probes");
        }
    }

    pub fn record_failure(&mut self, index: usize) {
        let Some(node) = self.proxies.get_mut(index) else {
            return;
        };
        node.consecutive_successes = 0;
        node.consecutive_failures = node.consecutive_failures.saturating_add(1);
        if node.recovery_cause != Some(RecoveryCause::RateLimit) {
            node.recovery_cause = Some(RecoveryCause::Transport);
            node.rate_limit_until = None;
            node.quarantined_exit_ip = None;
        }

        if node.consecutive_failures < FAILURE_THRESHOLD {
            node.health = HealthState::Degraded;
            return;
        }

        let until = Instant::now() + Duration::from_secs(COOLDOWN_SECS);
        node.health = HealthState::Unhealthy;
        node.circuit = CircuitState::Open { until };
        node.cooldown_until = Some(until);

        if node.lifecycle == LifecyclePolicy::Managed {
            if !self.restart_queue.contains(&index) {
                self.restart_queue.push(index);
            }
            warn!(
                node_id = %node.id,
                failures = node.consecutive_failures,
                "managed egress node is unhealthy and queued for restart"
            );
        } else {
            warn!(
                node_id = %node.id,
                failures = node.consecutive_failures,
                "protected standby opened its circuit; lifecycle action is forbidden"
            );
        }
    }

    pub fn mark_rate_limited(&mut self, index: usize, duration: Duration) {
        let Some(node) = self.proxies.get_mut(index) else {
            return;
        };
        let requested_until = Instant::now() + duration;
        let until = node
            .rate_limit_until
            .filter(|existing| *existing > requested_until)
            .unwrap_or(requested_until);
        let recovery_already_running = node.recovery_cause == Some(RecoveryCause::RateLimit)
            && node.health == HealthState::Recovering
            && node.restart_attempts > 0;
        node.health = HealthState::Degraded;
        node.circuit = CircuitState::Open { until };
        node.cooldown_until = Some(until);
        node.rate_limit_until = Some(until);
        if let Some(identity) = &node.exit_identity {
            node.quarantined_exit_ip = Some(identity.public_ip.clone());
        }
        node.recovery_cause = Some(RecoveryCause::RateLimit);
        node.consecutive_successes = 0;
        node.consecutive_failures = 0;
        let managed = node.lifecycle == LifecyclePolicy::Managed;
        let rotation_queued =
            managed && !recovery_already_running && !self.restart_queue.contains(&index);
        if rotation_queued {
            self.restart_queue.push(index);
        }
        if managed {
            warn!(
                node_id = %node.id,
                cooldown_secs = duration.as_secs(),
                quarantined_exit_ip = ?node.quarantined_exit_ip,
                rotation_queued,
                "egress circuit opened for rate limit; managed WARP recovery is active"
            );
        } else {
            warn!(
                node_id = %node.id,
                cooldown_secs = duration.as_secs(),
                quarantined_exit_ip = ?node.quarantined_exit_ip,
                "protected standby was quarantined after rate limit; destructive rotation is forbidden"
            );
        }
    }

    pub fn mark_rate_limited_adaptive(&mut self, index: usize, retry_count: u32) {
        let base_secs = 60_u64.saturating_mul(2_u64.pow(retry_count.min(3)));
        let jitter_percent = match index % 4 {
            0 => 100,
            1 => 85,
            2 => 115,
            _ => 95,
        };
        self.mark_rate_limited(
            index,
            Duration::from_secs(base_secs.saturating_mul(jitter_percent) / 100),
        );
    }

    /// Reserve a managed node for an explicit dashboard restart. This removes
    /// the node from normal routing before Docker is touched, clears stale
    /// cooldown/identity state, and prevents the automatic restart worker from
    /// racing the manual operation.
    pub fn begin_manual_restart(&mut self, index: usize) -> Result<(), String> {
        self.can_modify_node(index)?;
        self.restart_queue.retain(|queued| *queued != index);
        let node = self
            .proxies
            .get_mut(index)
            .ok_or_else(|| format!("unknown proxy index {index}"))?;
        node.health = HealthState::Recovering;
        node.circuit = CircuitState::HalfOpen;
        node.cooldown_until = None;
        node.consecutive_failures = 0;
        node.consecutive_successes = 0;
        node.restart_attempts = node.restart_attempts.saturating_add(1);
        node.recovery_cause.get_or_insert(RecoveryCause::Transport);
        node.exit_identity = None;
        node.duplicate_of = None;
        Ok(())
    }

    /// Record a failed explicit restart and hand the node back to the bounded
    /// automatic recovery queue when retry budget remains.
    pub fn mark_manual_restart_failed(&mut self, index: usize) {
        let retry = {
            let Some(node) = self.proxies.get_mut(index) else {
                return;
            };
            let until = Instant::now() + Duration::from_secs(COOLDOWN_SECS);
            node.health = HealthState::Unhealthy;
            node.circuit = CircuitState::Open { until };
            node.cooldown_until = Some(until);
            node.consecutive_successes = 0;
            node.lifecycle == LifecyclePolicy::Managed
                && node.restart_attempts < self.max_restart_attempts
        };
        if retry && !self.restart_queue.contains(&index) {
            self.restart_queue.push(index);
        }
    }

    pub fn mark_healthy(&mut self, index: usize) {
        if let Some(node) = self.proxies.get_mut(index) {
            mark_node_healthy(node);
        }
    }

    /// Move expired open circuits into half-open. This does not claim the node
    /// is healthy; a monitor probe or bounded request must close the circuit.
    pub fn recover_expired_cooldowns(&mut self) -> usize {
        let now = Instant::now();
        let mut transitioned = 0usize;
        let mut deferred_rate_limit_retries = Vec::new();

        for (index, node) in self.proxies.iter_mut().enumerate() {
            if !matches!(node.circuit, CircuitState::Open { until } if now >= until) {
                continue;
            }

            let rate_limit_active = node.rate_limit_until.is_some_and(|until| now < until);
            if node.recovery_cause == Some(RecoveryCause::RateLimit)
                && rate_limit_active
                && node.lifecycle == LifecyclePolicy::Managed
            {
                node.restart_attempts = 0;
                node.circuit = CircuitState::HalfOpen;
                node.health = HealthState::Recovering;
                node.cooldown_until = None;
                node.consecutive_successes = 0;
                deferred_rate_limit_retries.push(index);
                transitioned += 1;
                info!(
                    node_id = %node.id,
                    "deferred WARP identity rotation was requeued"
                );
                continue;
            }

            if node.rate_limit_until.is_some_and(|until| now >= until) {
                node.rate_limit_until = None;
                node.quarantined_exit_ip = None;
                if node.recovery_cause == Some(RecoveryCause::RateLimit) {
                    node.recovery_cause = None;
                }
            }
            node.circuit = CircuitState::HalfOpen;
            node.health = HealthState::Recovering;
            node.cooldown_until = None;
            node.consecutive_successes = 0;
            transitioned += 1;
            info!(node_id = %node.id, "egress circuit transitioned to half-open");
        }

        for index in deferred_rate_limit_retries {
            if !self.restart_queue.contains(&index) {
                self.restart_queue.push(index);
            }
        }
        transitioned
    }
}

fn mark_node_healthy(node: &mut ProxyEntry) {
    node.health = HealthState::Healthy;
    node.circuit = CircuitState::Closed;
    node.cooldown_until = None;
    node.rate_limit_until = None;
    node.quarantined_exit_ip = None;
    node.recovery_cause = None;
    node.consecutive_failures = 0;
    node.consecutive_successes = 0;
    node.restart_attempts = 0;
}

pub async fn process_restart_queue(
    pool: Arc<TokioRwLock<ProxyPool>>,
    runtime: Arc<dyn ContainerRuntime>,
    warp_image: String,
    identity_endpoints: Vec<String>,
    cadence: Duration,
    metrics: Arc<Metrics>,
    context: WorkerContext,
) -> Result<(), String> {
    let mut interval = tokio::time::interval(cadence.max(Duration::from_millis(100)));
    loop {
        tokio::select! {
            _ = context.cancellation().cancelled() => return Ok(()),
            _ = interval.tick() => {
                context.heartbeat();
                let indices = pool.write().await.drain_restart_queue();
                process_restart_batch(
                    indices,
                    pool.clone(),
                    runtime.clone(),
                    warp_image.clone(),
                    identity_endpoints.clone(),
                    metrics.clone(),
                    &context,
                )
                .await;
            }
        }
    }
}

async fn process_restart_batch(
    indices: Vec<usize>,
    pool: Arc<TokioRwLock<ProxyPool>>,
    runtime: Arc<dyn ContainerRuntime>,
    warp_image: String,
    identity_endpoints: Vec<String>,
    metrics: Arc<Metrics>,
    context: &WorkerContext,
) {
    let recoveries = stream::iter(indices.into_iter().map(|index| {
        let pool = pool.clone();
        let runtime = runtime.clone();
        let warp_image = warp_image.clone();
        let identity_endpoints = identity_endpoints.clone();
        let metrics = metrics.clone();
        async move {
            restart_container(
                index,
                pool,
                runtime,
                warp_image,
                identity_endpoints,
                metrics,
            )
            .await;
        }
    }))
    .buffer_unordered(MAX_CONCURRENT_PROXY_RECOVERIES);
    tokio::pin!(recoveries);

    loop {
        tokio::select! {
            _ = context.cancellation().cancelled() => return,
            completed = recoveries.next() => {
                if completed.is_none() {
                    return;
                }
                context.heartbeat();
            }
        }
    }
}

fn health_probe_targets(pool: &ProxyPool) -> Vec<(usize, u16)> {
    let max_restart_attempts = pool.max_restart_attempts.max(1);
    pool.proxies
        .iter()
        .enumerate()
        .filter(|(index, node)| {
            node.role == EgressRole::Primary
                && node.lifecycle == LifecyclePolicy::Managed
                && node.port != 0
                && node.recovery_cause != Some(RecoveryCause::RateLimit)
                && node.restart_attempts < max_restart_attempts
                && !pool.restart_queue.contains(index)
                && !(node.health == HealthState::Recovering && node.restart_attempts > 0)
        })
        .map(|(index, node)| (index, node.port))
        .collect()
}

fn apply_health_probe_result(pool: &mut ProxyPool, index: usize, reachable: bool) {
    let max_restart_attempts = pool.max_restart_attempts.max(1);
    let identity_required = pool.require_verified_exit_ip;
    let identity_ttl = pool.identity_ttl;
    let Some(node) = pool.proxies.get(index) else {
        return;
    };
    if node.role != EgressRole::Primary
        || node.lifecycle != LifecyclePolicy::Managed
        || node.recovery_cause == Some(RecoveryCause::RateLimit)
        || node.restart_attempts >= max_restart_attempts
    {
        return;
    }

    if !reachable {
        pool.record_failure(index);
        return;
    }

    let should_recover = {
        let node = &pool.proxies[index];
        let identity_ready = !identity_required
            || node
                .exit_identity
                .as_ref()
                .is_some_and(|identity| identity.is_fresh(identity_ttl));
        identity_ready
            && (node.recovery_cause == Some(RecoveryCause::Transport)
                || matches!(
                    node.health,
                    HealthState::Unknown
                        | HealthState::Degraded
                        | HealthState::Recovering
                        | HealthState::Unhealthy
                ))
    };

    if should_recover {
        let node_id = pool.proxies[index].id.clone();
        mark_node_healthy(&mut pool.proxies[index]);
        pool.restart_queue.retain(|queued| *queued != index);
        info!(node_id = %node_id, "egress node recovered via TCP probe");
    } else if pool.proxies[index].health == HealthState::Healthy {
        pool.proxies[index].consecutive_failures = 0;
    }
}

pub async fn health_monitor(
    pool: Arc<TokioRwLock<ProxyPool>>,
    cadence: Duration,
    context: WorkerContext,
) -> Result<(), String> {
    let mut interval = tokio::time::interval(cadence.max(Duration::from_millis(100)));
    loop {
        tokio::select! {
            _ = context.cancellation().cancelled() => return Ok(()),
            _ = interval.tick() => {
                context.heartbeat();
                pool.write().await.recover_expired_cooldowns();

                let targets = {
                    let pool = pool.read().await;
                    health_probe_targets(&pool)
                };

                for (index, port) in targets {
                    let reachable = tokio::net::TcpStream::connect(("127.0.0.1", port))
                        .await
                        .is_ok();
                    let mut pool = pool.write().await;
                    apply_health_probe_result(&mut pool, index, reachable);
                }
            }
        }
    }
}

fn node_requires_automatic_restart(node: &ProxyEntry) -> bool {
    node.lifecycle == LifecyclePolicy::Managed
        && (node.recovery_cause.is_some()
            || node.health == HealthState::Unhealthy
            || matches!(node.circuit, CircuitState::Open { .. }))
}

async fn restart_container(
    index: usize,
    pool: Arc<TokioRwLock<ProxyPool>>,
    runtime: Arc<dyn ContainerRuntime>,
    warp_image: String,
    identity_endpoints: Vec<String>,
    metrics: Arc<Metrics>,
) {
    let (port, container_name, restart_attempt, max_restart_attempts, recovery_cause) = {
        let guard = pool.read().await;
        if let Err(reason) = guard.can_modify_node(index) {
            warn!(%reason, "skipping destructive egress lifecycle action");
            drop(guard);
            let mut writable = pool.write().await;
            if writable.proxies.get(index).is_some_and(|node| {
                node.lifecycle == LifecyclePolicy::Managed && node.active_request_count() > 0
            }) && !writable.restart_queue.contains(&index)
            {
                writable.restart_queue.push(index);
            }
            return;
        }
        let Some(node) = guard.proxies.get(index) else {
            return;
        };
        if !node_requires_automatic_restart(node) {
            debug!(node_id = %node.id, "discarding stale automatic restart queue entry");
            return;
        }
        (
            node.port,
            node.container_name.clone(),
            node.restart_attempts.saturating_add(1),
            guard.max_restart_attempts.max(1),
            node.recovery_cause,
        )
    };

    if port == 0 || ensure_not_protected(port).is_err() || restart_attempt > max_restart_attempts {
        return;
    }

    metrics.record_proxy_restart_attempt();

    {
        let mut pool = pool.write().await;
        let node = &mut pool.proxies[index];
        node.health = HealthState::Recovering;
        node.circuit = CircuitState::HalfOpen;
        node.restart_attempts = restart_attempt;
        node.exit_identity = None;
        node.duplicate_of = None;
    }

    let spec = match ProxySpec::new(port, warp_image) {
        Ok(spec) => spec,
        Err(error) => {
            metrics.record_proxy_restart_failure();
            error!(%error, %container_name, "invalid managed proxy specification");
            apply_restart_failure_shared(&pool, index, restart_attempt).await;
            return;
        }
    };

    let lifecycle_result = if recovery_cause == Some(RecoveryCause::RateLimit) {
        info!(
            index,
            port, restart_attempt, "rotating managed WARP registration after rate limit"
        );
        runtime.rotate_managed(&spec).await
    } else {
        match runtime.restart_managed(&spec).await {
            Ok(()) => Ok(()),
            Err(restart_error) => {
                warn!(
                    %restart_error,
                    %container_name,
                    "managed WARP restart failed; attempting bounded recreate fallback"
                );
                runtime
                    .recreate_managed(&spec)
                    .await
                    .map_err(|recreate_error| {
                        DockerError::CommandFailed(format!(
                            "restart failed: {restart_error}; recreate failed: {recreate_error}"
                        ))
                    })
            }
        }
    };

    if let Err(error) = lifecycle_result {
        metrics.record_proxy_restart_failure();
        error!(
            %error,
            %container_name,
            ?recovery_cause,
            "container runtime failed to recover managed proxy"
        );
        apply_restart_failure_shared(&pool, index, restart_attempt).await;
        return;
    }

    match verify_proxy_identity(port, &identity_endpoints).await {
        Ok(identity) => {
            let accepted = {
                let mut pool = pool.write().await;
                accept_recovered_identity(&mut pool, index, identity)
            };
            match accepted {
                Ok(()) => {
                    metrics.record_proxy_restart_success();
                    info!(
                        index,
                        restart_attempt, "managed proxy recovered with a unique exit identity"
                    );
                }
                Err(reason) => {
                    metrics.record_proxy_restart_failure();
                    warn!(index, restart_attempt, %reason, "managed proxy recovery identity was rejected");
                    apply_restart_failure_shared(&pool, index, restart_attempt).await;
                }
            }
        }
        Err(error) => {
            metrics.record_proxy_restart_failure();
            warn!(index, restart_attempt, %error, "managed proxy identity verification failed");
            apply_restart_failure_shared(&pool, index, restart_attempt).await;
        }
    }
}

fn accept_recovered_identity(
    pool: &mut ProxyPool,
    index: usize,
    identity: ExitIdentity,
) -> Result<(), String> {
    let now = Instant::now();
    let node = pool
        .proxies
        .get(index)
        .ok_or_else(|| format!("unknown proxy index {index}"))?;
    let quota_active = node.rate_limit_until.is_some_and(|until| now < until);
    if quota_active
        && node
            .quarantined_exit_ip
            .as_deref()
            .is_some_and(|blocked| blocked == identity.public_ip)
    {
        return Err(format!(
            "WARP restart reused rate-limited exit IP {}",
            identity.public_ip
        ));
    }
    if let Some(owner) = pool
        .proxies
        .iter()
        .enumerate()
        .find_map(|(other_index, other)| {
            // Only another actively routed primary may block a recovered
            // primary. A disabled spare or protected warm standby must yield
            // duplicate ownership to the recovered primary; the deterministic
            // primary-first reconciliation below will mark that node as the
            // duplicate instead of exhausting the recovery budget.
            let actively_routed_primary =
                other.role == EgressRole::Primary && other.routing_enabled;
            (other_index != index
                && actively_routed_primary
                && other
                    .exit_identity
                    .as_ref()
                    .is_some_and(|existing| existing.public_ip == identity.public_ip))
            .then(|| other.id.clone())
        })
    {
        return Err(format!(
            "WARP restart produced duplicate exit IP {} already owned by {owner}",
            identity.public_ip
        ));
    }

    let node = &mut pool.proxies[index];
    node.exit_identity = Some(identity);
    node.duplicate_of = None;
    mark_node_healthy(node);
    pool.restart_queue.retain(|queued| *queued != index);
    pool.suppress_duplicate_exits();
    if pool.proxies[index].duplicate_of.is_some() {
        return Err("recovered exit identity became a duplicate after reconciliation".to_string());
    }
    Ok(())
}

async fn apply_restart_failure_shared(
    pool: &Arc<TokioRwLock<ProxyPool>>,
    index: usize,
    attempt: u32,
) {
    let mut pool = pool.write().await;
    if apply_restart_failure(&mut pool, index, attempt) {
        warn!(index, attempt, "managed proxy restart requeued");
    } else if pool.proxies.get(index).is_some_and(|node| {
        node.recovery_cause == Some(RecoveryCause::RateLimit)
            && node
                .rate_limit_until
                .is_some_and(|until| Instant::now() < until)
            && node.cooldown_until.is_some()
    }) {
        warn!(
            index,
            attempt,
            retry_after_secs = RATE_LIMIT_ROTATION_RETRY_SECS,
            "managed proxy rotation budget exhausted; deferred retry scheduled"
        );
    } else {
        error!(index, attempt, "managed proxy exhausted restart attempts");
    }
}

fn apply_restart_failure(pool: &mut ProxyPool, index: usize, attempt: u32) -> bool {
    let max_restart_attempts = pool.max_restart_attempts.max(1);
    let Some(node) = pool.proxies.get_mut(index) else {
        return false;
    };
    if node.lifecycle == LifecyclePolicy::Protected {
        pool.restart_queue.retain(|queued| *queued != index);
        return false;
    }

    node.restart_attempts = attempt.min(max_restart_attempts);
    node.health = HealthState::Unhealthy;

    if node.recovery_cause == Some(RecoveryCause::RateLimit)
        && node.restart_attempts >= max_restart_attempts
        && node
            .rate_limit_until
            .is_some_and(|until| Instant::now() < until)
    {
        let retry_at = Instant::now() + Duration::from_secs(RATE_LIMIT_ROTATION_RETRY_SECS);
        node.circuit = CircuitState::Open { until: retry_at };
        node.cooldown_until = Some(retry_at);
        pool.restart_queue.retain(|queued| *queued != index);
        return false;
    }

    let transport_until = Instant::now() + Duration::from_secs(COOLDOWN_SECS);
    let until = node
        .rate_limit_until
        .filter(|rate_limit_until| *rate_limit_until > transport_until)
        .unwrap_or(transport_until);
    node.circuit = CircuitState::Open { until };
    node.cooldown_until = Some(until);

    if node.restart_attempts < max_restart_attempts {
        if !pool.restart_queue.contains(&index) {
            pool.restart_queue.push(index);
        }
        true
    } else {
        pool.restart_queue.retain(|queued| *queued != index);
        false
    }
}

async fn verify_proxy_identity(
    port: u16,
    identity_endpoints: &[String],
) -> Result<ExitIdentity, String> {
    let proxy_url = format!("socks5h://127.0.0.1:{port}");
    let proxy =
        reqwest::Proxy::all(&proxy_url).map_err(|error| format!("invalid proxy URL: {error}"))?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| format!("failed to build verification client: {error}"))?;
    let fallback_endpoint = ["https://cloudflare.com/cdn-cgi/trace".to_string()];
    let endpoints = if identity_endpoints.is_empty() {
        fallback_endpoint.as_slice()
    } else {
        identity_endpoints
    };

    let mut last_error = "identity probe did not run".to_string();
    for attempt in 1..=12 {
        match probe_exit_identity(&client, endpoints).await {
            Ok(identity) => return Ok(identity),
            Err(error) => last_error = error,
        }
        if attempt < 12 {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> ProxyPool {
        ProxyPool::new(&[
            "socks5://127.0.0.1:40001".to_string(),
            "socks5://127.0.0.1:40002".to_string(),
            "socks5://127.0.0.1:40003".to_string(),
            "socks5://127.0.0.1:40004".to_string(),
        ])
    }

    fn identity(ip: &str) -> ExitIdentity {
        ExitIdentity {
            public_ip: ip.to_string(),
            provider: Some("cloudflare-warp".to_string()),
            colo: Some("HKG".to_string()),
            verified_at_unix_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    #[test]
    fn restart_attempts_are_independent_from_health_state() {
        let mut pool = pool();
        assert!(apply_restart_failure(&mut pool, 0, 1));
        assert_eq!(pool.proxies[0].restart_attempts, 1);
        assert_eq!(pool.proxies[0].health, HealthState::Unhealthy);
        assert_eq!(pool.drain_restart_queue(), vec![0]);

        assert!(apply_restart_failure(&mut pool, 0, 2));
        assert_eq!(pool.proxies[0].restart_attempts, 2);
        assert_eq!(pool.drain_restart_queue(), vec![0]);

        assert!(!apply_restart_failure(&mut pool, 0, 3));
        assert_eq!(pool.proxies[0].restart_attempts, 3);
        assert!(pool.drain_restart_queue().is_empty());
    }

    #[test]
    fn protected_standby_is_never_queued_for_restart() {
        let mut pool = pool();
        let standby = pool
            .proxies
            .iter()
            .position(|node| node.role == EgressRole::WarmStandby)
            .expect("standby node");
        assert!(!apply_restart_failure(&mut pool, standby, 1));
        assert!(pool.restart_queue.is_empty());
        assert_eq!(pool.proxies[standby].lifecycle, LifecyclePolicy::Protected);
    }

    #[test]
    fn manual_restart_clears_stale_circuit_and_automatic_queue() {
        let mut pool = pool();
        let until = Instant::now() + Duration::from_secs(COOLDOWN_SECS);
        pool.proxies[0].health = HealthState::Degraded;
        pool.proxies[0].circuit = CircuitState::Open { until };
        pool.proxies[0].cooldown_until = Some(until);
        pool.proxies[0].consecutive_failures = FAILURE_THRESHOLD;
        pool.restart_queue.push(0);

        pool.begin_manual_restart(0).expect("manual restart");

        assert_eq!(pool.proxies[0].health, HealthState::Recovering);
        assert_eq!(pool.proxies[0].circuit, CircuitState::HalfOpen);
        assert!(pool.proxies[0].cooldown_until.is_none());
        assert_eq!(pool.proxies[0].consecutive_failures, 0);
        assert_eq!(pool.proxies[0].restart_attempts, 1);
        assert!(pool.restart_queue.is_empty());
    }

    #[test]
    fn manual_restart_failure_requeues_managed_node() {
        let mut pool = pool();
        pool.begin_manual_restart(0).expect("manual restart");
        pool.mark_manual_restart_failed(0);

        assert_eq!(pool.proxies[0].health, HealthState::Unhealthy);
        assert!(matches!(pool.proxies[0].circuit, CircuitState::Open { .. }));
        assert_eq!(pool.restart_queue, vec![0]);
    }

    #[test]
    fn stale_queue_entry_is_not_actionable_after_recovery() {
        let mut pool = pool();
        let node = &mut pool.proxies[0];
        node.health = HealthState::Healthy;
        node.circuit = CircuitState::Closed;
        node.recovery_cause = None;
        assert!(!node_requires_automatic_restart(node));

        node.health = HealthState::Unhealthy;
        node.recovery_cause = Some(RecoveryCause::Transport);
        assert!(node_requires_automatic_restart(node));
    }

    #[test]
    fn health_monitor_probes_healthy_managed_primaries_but_not_protected_standby() {
        let pool = pool();
        let targets = health_probe_targets(&pool);
        let ports: Vec<u16> = targets.into_iter().map(|(_, port)| port).collect();

        assert!(ports.contains(&40001));
        assert!(ports.contains(&40002));
        assert!(ports.contains(&40003));
        assert!(!ports.contains(&40004));
    }

    #[test]
    fn two_failed_health_probes_queue_managed_primary_restart() {
        let mut pool = pool();

        apply_health_probe_result(&mut pool, 0, false);
        assert_eq!(pool.proxies[0].health, HealthState::Degraded);
        assert_eq!(
            pool.proxies[0].recovery_cause,
            Some(RecoveryCause::Transport)
        );
        assert!(pool.restart_queue.is_empty());

        apply_health_probe_result(&mut pool, 0, false);
        assert_eq!(pool.proxies[0].health, HealthState::Unhealthy);
        assert!(matches!(pool.proxies[0].circuit, CircuitState::Open { .. }));
        assert_eq!(pool.restart_queue, vec![0]);
    }

    #[test]
    fn successful_health_probe_clears_transport_failure_without_fake_request_success() {
        let mut pool = pool();
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));

        apply_health_probe_result(&mut pool, 0, false);
        apply_health_probe_result(&mut pool, 0, true);

        assert_eq!(pool.proxies[0].health, HealthState::Healthy);
        assert_eq!(pool.proxies[0].circuit, CircuitState::Closed);
        assert!(pool.proxies[0].recovery_cause.is_none());
        assert_eq!(pool.proxies[0].consecutive_successes, 0);
        assert!(pool.restart_queue.is_empty());
    }

    #[test]
    fn health_monitor_does_not_interfere_with_rate_limit_or_inflight_restart() {
        let mut pool = pool();
        pool.proxies[0].recovery_cause = Some(RecoveryCause::RateLimit);
        pool.proxies[1].health = HealthState::Recovering;
        pool.proxies[1].recovery_cause = Some(RecoveryCause::Transport);
        pool.proxies[1].restart_attempts = 1;

        let targets = health_probe_targets(&pool);
        let indices: Vec<usize> = targets.into_iter().map(|(index, _)| index).collect();
        assert!(!indices.contains(&0));
        assert!(!indices.contains(&1));
    }

    #[test]
    fn configured_restart_attempt_limit_is_honored() {
        let mut pool = pool();
        pool.set_max_restart_attempts(2);
        assert!(apply_restart_failure(&mut pool, 0, 1));
        assert!(!apply_restart_failure(&mut pool, 0, 2));
        assert_eq!(pool.proxies[0].restart_attempts, 2);
        assert!(pool.restart_queue.is_empty());
    }

    #[test]
    fn exhausted_rate_limit_rotation_is_requeued_after_short_delay() {
        let mut pool = pool();
        pool.set_max_restart_attempts(2);
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        pool.drain_restart_queue();

        assert!(apply_restart_failure(&mut pool, 0, 1));
        pool.drain_restart_queue();
        assert!(!apply_restart_failure(&mut pool, 0, 2));
        assert_eq!(pool.proxies[0].restart_attempts, 2);
        assert!(pool.restart_queue.is_empty());
        assert!(pool.recovery_in_progress());
        assert!(pool.proxies[0].rate_limit_until.is_some());

        let expired = Instant::now() - Duration::from_secs(1);
        pool.proxies[0].circuit = CircuitState::Open { until: expired };
        pool.proxies[0].cooldown_until = Some(expired);

        assert_eq!(pool.recover_expired_cooldowns(), 1);
        assert_eq!(pool.restart_queue, vec![0]);
        assert_eq!(pool.proxies[0].restart_attempts, 0);
        assert_eq!(
            pool.proxies[0].recovery_cause,
            Some(RecoveryCause::RateLimit)
        );
        assert!(pool.proxies[0].rate_limit_until.is_some());
        assert_eq!(pool.proxies[0].circuit, CircuitState::HalfOpen);
    }

    #[test]
    fn rate_limit_recovery_rejects_same_exit_before_quota_expiry() {
        let mut pool = pool();
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        pool.drain_restart_queue();

        let error = accept_recovered_identity(&mut pool, 0, identity("1.1.1.1"))
            .expect_err("same exit must remain quarantined");
        assert!(error.contains("reused rate-limited exit IP"));
        assert_eq!(
            pool.proxies[0].recovery_cause,
            Some(RecoveryCause::RateLimit)
        );
    }

    #[test]
    fn rate_limit_recovery_may_reuse_exit_owned_only_by_disabled_spare() {
        let mut pool = pool();
        pool.active_count = 1;
        for (index, node) in pool.proxies.iter_mut().enumerate() {
            node.routing_enabled = index == 0;
        }
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.proxies[1].exit_identity = Some(identity("8.8.8.8"));
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        pool.drain_restart_queue();

        accept_recovered_identity(&mut pool, 0, identity("8.8.8.8"))
            .expect("disabled spare must not block active recovery");
        assert_eq!(pool.proxies[0].health, HealthState::Healthy);
        assert_eq!(pool.proxies[0].circuit, CircuitState::Closed);
        assert_eq!(
            pool.proxies[0]
                .exit_identity
                .as_ref()
                .map(|identity| identity.public_ip.as_str()),
            Some("8.8.8.8")
        );
        assert_eq!(
            pool.proxies[1].duplicate_of.as_deref(),
            Some(pool.proxies[0].id.as_str())
        );
    }

    #[test]
    fn rate_limit_recovery_may_take_exit_owned_by_warm_standby() {
        let mut pool = pool();
        let standby = pool
            .proxies
            .iter()
            .position(|node| node.role == EgressRole::WarmStandby)
            .expect("warm standby");
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.proxies[standby].exit_identity = Some(identity("8.8.8.8"));
        pool.proxies[standby].health = HealthState::Healthy;
        pool.proxies[standby].circuit = CircuitState::Closed;
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        pool.drain_restart_queue();

        accept_recovered_identity(&mut pool, 0, identity("8.8.8.8"))
            .expect("warm standby must yield duplicate ownership to primary");

        assert_eq!(pool.proxies[0].health, HealthState::Healthy);
        assert_eq!(pool.proxies[0].circuit, CircuitState::Closed);
        assert_eq!(
            pool.proxies[0]
                .exit_identity
                .as_ref()
                .map(|identity| identity.public_ip.as_str()),
            Some("8.8.8.8")
        );
        assert_eq!(
            pool.proxies[standby].duplicate_of.as_deref(),
            Some(pool.proxies[0].id.as_str())
        );
    }

    #[test]
    fn rate_limit_recovery_rejects_duplicate_exit() {
        let mut pool = pool();
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.proxies[1].exit_identity = Some(identity("8.8.8.8"));
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        pool.drain_restart_queue();

        let error = accept_recovered_identity(&mut pool, 0, identity("8.8.8.8"))
            .expect_err("duplicate exit must be rejected");
        assert!(error.contains("duplicate exit IP"));
    }

    #[test]
    fn rate_limit_recovery_accepts_new_unique_exit() {
        let mut pool = pool();
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.proxies[1].exit_identity = Some(identity("8.8.8.8"));
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        pool.drain_restart_queue();

        accept_recovered_identity(&mut pool, 0, identity("9.9.9.9"))
            .expect("new unique exit should recover");
        assert_eq!(pool.proxies[0].health, HealthState::Healthy);
        assert_eq!(pool.proxies[0].circuit, CircuitState::Closed);
        assert_eq!(
            pool.proxies[0]
                .exit_identity
                .as_ref()
                .map(|identity| identity.public_ip.as_str()),
            Some("9.9.9.9")
        );
        assert!(pool.proxies[0].rate_limit_until.is_none());
        assert!(pool.proxies[0].quarantined_exit_ip.is_none());
        assert!(pool.proxies[0].recovery_cause.is_none());
    }

    #[test]
    fn restart_failure_preserves_long_rate_limit_deadline() {
        let mut pool = pool();
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.mark_rate_limited(0, Duration::from_secs(3_600));
        let original_deadline = pool.proxies[0].rate_limit_until.expect("deadline");
        pool.drain_restart_queue();

        assert!(apply_restart_failure(&mut pool, 0, 1));
        assert_eq!(pool.proxies[0].rate_limit_until, Some(original_deadline));
        assert!(matches!(
            pool.proxies[0].circuit,
            CircuitState::Open { until } if until == original_deadline
        ));
    }

    #[test]
    fn expired_open_circuit_becomes_half_open_not_healthy() {
        let mut pool = pool();
        pool.proxies[0].circuit = CircuitState::Open {
            until: Instant::now() - Duration::from_secs(1),
        };
        pool.proxies[0].health = HealthState::Degraded;
        assert_eq!(pool.recover_expired_cooldowns(), 1);
        assert_eq!(pool.proxies[0].circuit, CircuitState::HalfOpen);
        assert_eq!(pool.proxies[0].health, HealthState::Recovering);
    }
}

#[cfg(test)]
mod metrics_tests {
    use super::*;
    use crate::docker::{ContainerState, ContainerSummary, DockerError, DockerResult};
    use crate::workers::WorkerRegistry;
    use async_trait::async_trait;
    use tokio::sync::Notify;

    #[derive(Debug)]
    struct FailingRuntime;

    #[async_trait]
    impl ContainerRuntime for FailingRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".to_string())
        }
        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
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
            Err(DockerError::CommandFailed("intentional".to_string()))
        }
        async fn remove_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn restart_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "intentional restart failure".to_string(),
            ))
        }
        async fn stop_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
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

    #[derive(Debug)]
    struct BlockingRuntime {
        first_started: Arc<Notify>,
        second_started: Arc<Notify>,
        release_first: Arc<Notify>,
    }

    impl BlockingRuntime {
        fn new() -> (Self, Arc<Notify>, Arc<Notify>, Arc<Notify>) {
            let first_started = Arc::new(Notify::new());
            let second_started = Arc::new(Notify::new());
            let release_first = Arc::new(Notify::new());
            (
                Self {
                    first_started: first_started.clone(),
                    second_started: second_started.clone(),
                    release_first: release_first.clone(),
                },
                first_started,
                second_started,
                release_first,
            )
        }
    }

    #[async_trait]
    impl ContainerRuntime for BlockingRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".to_string())
        }
        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
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
            Err(DockerError::CommandFailed("intentional".to_string()))
        }
        async fn remove_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn restart_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            match spec.port {
                40001 => {
                    self.first_started.notify_one();
                    self.release_first.notified().await;
                }
                40002 => self.second_started.notify_one(),
                _ => {}
            }
            Err(DockerError::CommandFailed("intentional".to_string()))
        }
        async fn stop_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
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

    #[tokio::test]
    async fn slow_proxy_recovery_does_not_starve_other_queued_nodes() {
        let pool = Arc::new(TokioRwLock::new(ProxyPool::new(&[
            "socks5://127.0.0.1:40001".to_string(),
            "socks5://127.0.0.1:40002".to_string(),
        ])));
        {
            let mut guard = pool.write().await;
            for index in 0..2 {
                guard.proxies[index].health = HealthState::Unhealthy;
                guard.proxies[index].circuit = CircuitState::Open {
                    until: Instant::now() + Duration::from_secs(120),
                };
                guard.proxies[index].recovery_cause = Some(RecoveryCause::Transport);
                guard.restart_queue.push(index);
            }
        }

        let (runtime, first_started, second_started, release_first) = BlockingRuntime::new();
        let workers = WorkerRegistry::new();
        let task_pool = pool.clone();
        workers.spawn_critical("test-proxy-restart", move |context| async move {
            process_restart_queue(
                task_pool,
                Arc::new(runtime),
                "example/warp:test".to_string(),
                Vec::new(),
                Duration::from_secs(60),
                Arc::new(Metrics::default()),
                context,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), first_started.notified())
            .await
            .expect("first recovery should start");
        tokio::time::timeout(Duration::from_millis(200), second_started.notified())
            .await
            .expect("second queued recovery must start without waiting for the first one");

        release_first.notify_one();
        workers
            .shutdown(Duration::from_secs(2))
            .await
            .expect("restart worker should stop cleanly");
    }

    #[tokio::test]
    async fn restart_runtime_failure_is_counted_once() {
        let pool = Arc::new(TokioRwLock::new(ProxyPool::new(&[
            "socks5://127.0.0.1:40001".to_string(),
        ])));
        {
            let mut guard = pool.write().await;
            guard.proxies[0].health = HealthState::Unhealthy;
            guard.proxies[0].circuit = CircuitState::Open {
                until: Instant::now() + Duration::from_secs(120),
            };
            guard.proxies[0].recovery_cause = Some(RecoveryCause::Transport);
        }
        let metrics = Arc::new(Metrics::default());
        restart_container(
            0,
            pool,
            Arc::new(FailingRuntime),
            "example/warp:test".to_string(),
            Vec::new(),
            metrics.clone(),
        )
        .await;
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.proxy_restart_attempts, 1);
        assert_eq!(snapshot.proxy_restart_failures, 1);
        assert_eq!(snapshot.proxy_restart_successes, 0);
    }
}
