//! Proxy subsystem lifecycle reconciliation: bootstrap, verify, and publish readiness.

mod recovery;
mod verification;

#[cfg(test)]
mod tests;
pub use verification::{verify_candidate, LiveProxyVerifier, ProxyVerifier, VerificationFailure};

pub(crate) use recovery::{
    failure_attempt_after_cycle, recovery_backoff, sleep_with_heartbeat, unix_now,
};
pub(crate) use verification::{verify_identity_stage, verify_route_stage, verify_transport_stage};

use super::{ProxyPool, ProxySubsystemPhase, ProxySubsystemStatus};
use crate::config::BridgeConfig;
use crate::docker::{ContainerRuntime, ProxySpec};
use crate::observability::Metrics;
use crate::workers::WorkerContext;
use std::sync::Arc;
use tokio::sync::RwLock;

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

pub(crate) async fn reconcile_once(
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
