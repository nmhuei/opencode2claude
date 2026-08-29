//! Round-robin load-balanced routing across primary and protected warm-standby egress nodes.

use super::types::*;
use std::time::Instant;
use tracing::{info, warn};

impl ProxyPool {
    pub fn select_proxy_for_key(
        &mut self,
        _routing_key: &str,
    ) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_round_robin(None)
    }

    /// Round-robin selection: pick the next healthy proxy in cyclic order,
    /// skipping any excluded index (used on retry after a failed proxy).
    fn select_proxy_round_robin(
        &mut self,
        excluded: Option<usize>,
    ) -> Option<(reqwest::Client, String, usize)> {
        if self.proxies.is_empty() {
            return None;
        }

        let now = Instant::now();
        let total = self.proxies.len();

        // --- Pass 0: coalesce concurrent request bursts onto an in-flight primary ---
        //
        // Claude Code commonly emits a user request plus a session-title request
        // at the same time. If the provider rate-limits that burst, spreading
        // those sibling requests across multiple WARP primaries can quarantine
        // multiple exits before recovery starts. Reusing an already-leased
        // healthy primary keeps the burst failure scoped to one node; explicit
        // retries still pass an excluded index and skip this path for that node.
        for index in 0..total {
            if Some(index) == excluded {
                continue;
            }
            let node = &self.proxies[index];
            if node.role == EgressRole::Primary
                && node.active_request_count() > 0
                && node.routing_enabled
                && is_normal_route(node, now, self.require_verified_exit_ip, self.identity_ttl)
            {
                info!(
                    node_id = %node.id,
                    index,
                    active_requests = node.active_request_count(),
                    "round-robin reusing active primary"
                );
                return Some(proxy_selection(node, index));
            }
        }

        // --- Pass 1: round-robin across healthy primaries ---
        for offset in 0..total {
            let index = (self.round_robin_counter + offset) % total;
            if Some(index) == excluded {
                continue;
            }
            let node = &self.proxies[index];
            if node.role == EgressRole::Primary
                && node.routing_enabled
                && is_normal_route(node, now, self.require_verified_exit_ip, self.identity_ttl)
            {
                // Advance counter past this node for next call.
                self.round_robin_counter = (index + 1) % total;
                info!(
                    node_id = %node.id,
                    index,
                    "round-robin selected primary"
                );
                return Some(proxy_selection(node, index));
            }
        }

        // --- Pass 2: round-robin across healthy warm-standby ---
        for offset in 0..total {
            let index = (self.round_robin_counter + offset) % total;
            if Some(index) == excluded {
                continue;
            }
            let node = &self.proxies[index];
            if node.role == EgressRole::WarmStandby
                && is_normal_route(node, now, self.require_verified_exit_ip, self.identity_ttl)
            {
                self.round_robin_counter = (index + 1) % total;
                info!(
                    node_id = %node.id,
                    index,
                    "round-robin selected warm standby"
                );
                return Some(proxy_selection(node, index));
            }
        }

        // --- Pass 3: half-open probe (primary then standby) ---
        for offset in 0..total {
            let index = (self.round_robin_counter + offset) % total;
            if Some(index) == excluded {
                continue;
            }
            let node = &self.proxies[index];
            if is_probe_route(node, now, self.require_verified_exit_ip, self.identity_ttl) {
                self.round_robin_counter = (index + 1) % total;
                warn!(node_id = %node.id, "round-robin using probe route");
                return Some(proxy_selection(node, index));
            }
        }

        None
    }

    pub fn get_client(&mut self, routing_key: &str) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key(routing_key)
    }

    pub fn get_client_excluding(
        &mut self,
        _routing_key: &str,
        excluded_index: usize,
    ) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_round_robin(Some(excluded_index))
    }
}

fn is_normal_route(
    node: &ProxyEntry,
    now: Instant,
    require_verified_exit_ip: bool,
    identity_ttl: std::time::Duration,
) -> bool {
    let identity_required = require_verified_exit_ip || node.role == EgressRole::WarmStandby;
    !node.draining
        && node.health == HealthState::Healthy
        && node.circuit == CircuitState::Closed
        && !node.is_duplicate()
        && !node.provider_rate_limit_active(now)
        && (!identity_required
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
    let identity_required = require_verified_exit_ip || node.role == EgressRole::WarmStandby;
    if node.draining
        || node.recovery_cause == Some(RecoveryCause::RateLimit)
        || node.is_duplicate()
        || node.active_request_count() > 0
        || (identity_required
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
