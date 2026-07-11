//! Egress health, circuit transitions, and managed-primary restart queue.

use super::types::*;
use crate::docker::{ContainerRuntime, ProxySpec};
use crate::workers::WorkerContext;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{error, info, warn};

const MAX_RESTART_ATTEMPTS: u32 = 3;

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
        let until = Instant::now() + duration;
        node.health = HealthState::Degraded;
        node.circuit = CircuitState::Open { until };
        node.cooldown_until = Some(until);
        node.consecutive_successes = 0;
        warn!(node_id = %node.id, cooldown_secs = duration.as_secs(), "egress circuit opened for rate limit");
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
        for node in &mut self.proxies {
            if matches!(node.circuit, CircuitState::Open { until } if now >= until) {
                node.circuit = CircuitState::HalfOpen;
                node.health = HealthState::Recovering;
                node.cooldown_until = None;
                node.consecutive_successes = 0;
                transitioned += 1;
                info!(node_id = %node.id, "egress circuit transitioned to half-open");
            }
        }
        transitioned
    }
}

fn mark_node_healthy(node: &mut ProxyEntry) {
    node.health = HealthState::Healthy;
    node.circuit = CircuitState::Closed;
    node.cooldown_until = None;
    node.consecutive_failures = 0;
    node.consecutive_successes = 0;
    node.restart_attempts = 0;
}

pub async fn process_restart_queue(
    pool: Arc<TokioRwLock<ProxyPool>>,
    runtime: Arc<dyn ContainerRuntime>,
    warp_image: String,
    cadence: Duration,
    context: WorkerContext,
) -> Result<(), String> {
    let mut interval = tokio::time::interval(cadence.max(Duration::from_millis(100)));
    loop {
        tokio::select! {
            _ = context.cancellation().cancelled() => return Ok(()),
            _ = interval.tick() => {
                context.heartbeat();
                let indices = pool.write().await.drain_restart_queue();
                for index in indices {
                    restart_container(index, pool.clone(), runtime.clone(), warp_image.clone()).await;
                }
            }
        }
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

                let targets: Vec<(usize, u16)> = {
                    let pool = pool.read().await;
                    pool.proxies
                        .iter()
                        .enumerate()
                        .filter(|(_, node)| {
                            matches!(
                                node.health,
                                HealthState::Recovering | HealthState::Unhealthy
                            ) && !node.circuit.is_open(Instant::now())
                        })
                        .map(|(index, node)| (index, node.port))
                        .collect()
                };

                for (index, port) in targets {
                    if port == 0 {
                        continue;
                    }
                    if tokio::net::TcpStream::connect(("127.0.0.1", port))
                        .await
                        .is_ok()
                    {
                        let mut pool = pool.write().await;
                        if let Some(node) = pool.proxies.get_mut(index) {
                            if matches!(
                                node.health,
                                HealthState::Recovering | HealthState::Unhealthy
                            ) {
                                mark_node_healthy(node);
                                info!(node_id = %node.id, "egress node recovered via TCP probe");
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn restart_container(
    index: usize,
    pool: Arc<TokioRwLock<ProxyPool>>,
    runtime: Arc<dyn ContainerRuntime>,
    warp_image: String,
) {
    let (port, container_name, restart_attempt) = {
        let guard = pool.read().await;
        if let Err(reason) = guard.can_modify_node(index) {
            warn!(%reason, "skipping destructive egress lifecycle action");
            // A leased node is retried later; protected nodes are never requeued.
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
        let node = &guard.proxies[index];
        (
            node.port,
            node.container_name.clone(),
            node.restart_attempts.saturating_add(1),
        )
    };

    if port == 0 || ensure_not_protected(port).is_err() {
        return;
    }

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
            error!(%error, %container_name, "invalid managed proxy specification");
            apply_restart_failure_shared(&pool, index, restart_attempt).await;
            return;
        }
    };

    if let Err(error) = runtime.recreate_managed(&spec).await {
        error!(%error, %container_name, "container runtime failed to recreate managed proxy");
        apply_restart_failure_shared(&pool, index, restart_attempt).await;
        return;
    }

    if verify_proxy_socks(port).await {
        let mut pool = pool.write().await;
        if let Some(node) = pool.proxies.get_mut(index) {
            mark_node_healthy(node);
        }
    } else {
        apply_restart_failure_shared(&pool, index, restart_attempt).await;
    }
}

async fn apply_restart_failure_shared(
    pool: &Arc<TokioRwLock<ProxyPool>>,
    index: usize,
    attempt: u32,
) {
    let mut pool = pool.write().await;
    if apply_restart_failure(&mut pool, index, attempt) {
        warn!(index, attempt, "managed proxy restart requeued");
    } else {
        error!(index, attempt, "managed proxy exhausted restart attempts");
    }
}

fn apply_restart_failure(pool: &mut ProxyPool, index: usize, attempt: u32) -> bool {
    let Some(node) = pool.proxies.get_mut(index) else {
        return false;
    };
    if node.lifecycle == LifecyclePolicy::Protected {
        pool.restart_queue.retain(|queued| *queued != index);
        return false;
    }

    node.restart_attempts = attempt.min(MAX_RESTART_ATTEMPTS);
    node.health = HealthState::Unhealthy;
    let until = Instant::now() + Duration::from_secs(COOLDOWN_SECS);
    node.circuit = CircuitState::Open { until };
    node.cooldown_until = Some(until);

    if node.restart_attempts < MAX_RESTART_ATTEMPTS {
        if !pool.restart_queue.contains(&index) {
            pool.restart_queue.push(index);
        }
        true
    } else {
        pool.restart_queue.retain(|queued| *queued != index);
        false
    }
}

async fn verify_proxy_socks(port: u16) -> bool {
    let proxy_url = format!("socks5h://127.0.0.1:{port}");
    let Ok(proxy) = reqwest::Proxy::all(&proxy_url) else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return false;
    };

    for attempt in 1..=12 {
        if client
            .get("https://cloudflare.com/cdn-cgi/trace")
            .send()
            .await
            .is_ok()
        {
            return true;
        }
        if attempt < 12 {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    false
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
