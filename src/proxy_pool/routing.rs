//! Stable sticky routing across primary and protected warm-standby egress nodes.

use super::types::*;
use std::time::Instant;
use tracing::{info, warn};

/// Explicit FNV-1a rendezvous score. Unlike `DefaultHasher`, this mapping is a
/// compatibility contract across processes and Rust toolchain versions.
pub fn stable_rendezvous_score(key: &str, node_id: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in key
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0xff))
        .chain(node_id.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

impl ProxyPool {
    /// Sticky assignment considers every enabled primary, even when currently
    /// unavailable, so a recovered primary receives its original sessions.
    pub(crate) fn rendezvous_assigned_primary(&self, routing_key: &str) -> Option<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(_, node)| node.role == EgressRole::Primary && node.routing_enabled)
            .max_by_key(|(_, node)| stable_rendezvous_score(routing_key, &node.id))
            .map(|(index, _)| index)
    }

    pub fn select_proxy_for_key(
        &self,
        routing_key: &str,
    ) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key_excluding(routing_key, None)
    }

    fn select_proxy_for_key_excluding(
        &self,
        routing_key: &str,
        excluded: Option<usize>,
    ) -> Option<(reqwest::Client, String, usize)> {
        if self.proxies.is_empty() {
            return None;
        }

        let assigned = self.rendezvous_assigned_primary(routing_key);
        if let Some(index) = assigned.filter(|index| Some(*index) != excluded) {
            let node = &self.proxies[index];
            if is_normal_route(
                node,
                Instant::now(),
                self.require_verified_exit_ip,
                self.identity_ttl,
            ) {
                return Some(proxy_selection(node, index));
            }
            info!(
                node_id = %node.id,
                %routing_key,
                health = ?node.health,
                circuit = node.circuit.label(),
                "sticky primary is unavailable"
            );
        }

        // Always exhaust another healthy primary before protected standby.
        if let Some(index) = self.best_candidate(
            routing_key,
            EgressRole::Primary,
            excluded,
            CandidateKind::Normal,
        ) {
            return Some(proxy_selection(&self.proxies[index], index));
        }

        if let Some(index) = self.best_candidate(
            routing_key,
            EgressRole::WarmStandby,
            excluded,
            CandidateKind::Normal,
        ) {
            return Some(proxy_selection(&self.proxies[index], index));
        }

        // A single request may exercise a half-open node. Open circuits and
        // duplicate exits are never used as degraded application routes.
        if let Some(index) = self.best_candidate(
            routing_key,
            EgressRole::Primary,
            excluded,
            CandidateKind::Probe,
        ) {
            warn!(node_id = %self.proxies[index].id, "using half-open primary route");
            return Some(proxy_selection(&self.proxies[index], index));
        }
        if let Some(index) = self.best_candidate(
            routing_key,
            EgressRole::WarmStandby,
            excluded,
            CandidateKind::Probe,
        ) {
            warn!(node_id = %self.proxies[index].id, "using half-open standby route");
            return Some(proxy_selection(&self.proxies[index], index));
        }

        None
    }

    fn best_candidate(
        &self,
        routing_key: &str,
        role: EgressRole,
        excluded: Option<usize>,
        kind: CandidateKind,
    ) -> Option<usize> {
        let now = Instant::now();
        self.proxies
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                Some(*index) != excluded
                    && node.role == role
                    && (role != EgressRole::Primary || node.routing_enabled)
                    && match kind {
                        CandidateKind::Normal => is_normal_route(
                            node,
                            now,
                            self.require_verified_exit_ip,
                            self.identity_ttl,
                        ),
                        CandidateKind::Probe => is_probe_route(
                            node,
                            now,
                            self.require_verified_exit_ip,
                            self.identity_ttl,
                        ),
                    }
            })
            .max_by_key(|(_, node)| {
                // Rendezvous remains the dominant ordering. The low bits favor
                // the less-loaded node only when scores are extremely close.
                stable_rendezvous_score(routing_key, &node.id)
                    .saturating_sub(node.active_request_count() as u64)
            })
            .map(|(index, _)| index)
    }

    pub fn get_client(&mut self, routing_key: &str) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key(routing_key)
    }

    pub fn get_client_excluding(
        &mut self,
        routing_key: &str,
        excluded_index: usize,
    ) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key_excluding(routing_key, Some(excluded_index))
    }
}

#[derive(Debug, Clone, Copy)]
enum CandidateKind {
    Normal,
    Probe,
}

fn is_normal_route(
    node: &ProxyEntry,
    now: Instant,
    require_verified_exit_ip: bool,
    identity_ttl: std::time::Duration,
) -> bool {
    node.health == HealthState::Healthy
        && node.circuit == CircuitState::Closed
        && !node.is_duplicate()
        && (!require_verified_exit_ip
            || node
                .exit_identity
                .as_ref()
                .is_some_and(|identity| identity.is_fresh(identity_ttl)))
        && !matches!(node.circuit, CircuitState::Open { until } if now < until)
}

fn is_probe_route(
    node: &ProxyEntry,
    now: Instant,
    require_verified_exit_ip: bool,
    identity_ttl: std::time::Duration,
) -> bool {
    if node.is_duplicate()
        || node.active_request_count() > 0
        || (require_verified_exit_ip
            && !node
                .exit_identity
                .as_ref()
                .is_some_and(|identity| identity.is_fresh(identity_ttl)))
    {
        return false;
    }
    node.may_receive_probe_traffic()
        || matches!(node.circuit, CircuitState::Open { until } if now >= until)
}

fn proxy_selection(node: &ProxyEntry, index: usize) -> (reqwest::Client, String, usize) {
    (node.client.clone(), node.url.clone(), index)
}
