//! Rendezvous hashing and proxy selection for multi-agent routing.
//!
//! Implements the Phase 5 routing contract:
//! 1. Primary-first: use 40001-40003 for normal traffic
//! 2. WarmStandby failover: 40004-40005 only when primary is unhealthy
//! 3. Affected-agent-only remap: healthy primaries keep their agents
//! 4. Rendezvous hashing for stable sticky determinism

use super::types::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tracing::{info, warn};

// ── Stable hash helpers ──

/// Deterministic 64-bit score for Rendezvous hashing.
///
/// Uses DefaultHasher (std). Deterministic within the same process execution
/// but may vary across Rust versions. Replace with sha2/blake3 for truly
/// stable cross-build determinism.
pub fn stable_rendezvous_score(key: &str, node_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    node_id.hash(&mut hasher);
    hasher.finish()
}

// ── Proxy selection impl ──

impl ProxyPool {
    /// Returns the rendezvous-assigned primary for a routing key,
    /// considering ALL primaries regardless of health status.
    /// Ensures sticky assignment: even if a primary is on cooldown,
    /// the key still maps to the same slot, enabling correct failover.
    pub(crate) fn rendezvous_assigned_primary(&self, routing_key: &str) -> Option<usize> {
        let all_primaries: Vec<usize> = self
            .proxies
            .iter()
            .enumerate()
            .filter(|(_, p)| p.role == ProxyRole::Primary)
            .map(|(i, _)| i)
            .collect();
        if all_primaries.is_empty() {
            return None;
        }
        all_primaries
            .iter()
            .copied()
            .max_by_key(|idx| stable_rendezvous_score(routing_key, &self.proxies[*idx].url))
    }

    /// Select a proxy for the given routing key following the Phase 5 routing contract:
    ///
    /// 1. Use Primary proxies 40001–40003 for normal traffic.
    /// 2. Use WarmStandby proxies 40004–40005 only when the selected primary
    ///    is unhealthy (cooldown/dead).
    /// 3. Affected-agent-only remap: failure of one primary does NOT remap
    ///    agents assigned to healthy primaries.
    /// 4. Rendezvous hashing for stable sticky determinism.
    /// 5. Complies with cooldown/recovery policy.
    ///
    /// Returns `(Client, proxy_url, index)` or `None` if no proxy is available.
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
        if let Some(primary_idx) = assigned.filter(|index| Some(*index) != excluded) {
            let entry = &self.proxies[primary_idx];
            if proxy_is_healthy(entry) {
                return Some(proxy_selection(entry, primary_idx));
            }
            info!(
                primary_index = primary_idx,
                proxy_url = %entry.url,
                %routing_key,
                status = ?entry.status,
                "assigned primary unavailable"
            );
        }

        // A retry must remain primary-first. If the sticky primary is excluded,
        // choose the best other healthy primary before consuming standby capacity.
        if assigned == excluded {
            if let Some(index) =
                self.best_healthy_candidate(routing_key, ProxyRole::Primary, excluded)
            {
                return Some(proxy_selection(&self.proxies[index], index));
            }
        }

        if let Some(index) =
            self.best_healthy_candidate(routing_key, ProxyRole::WarmStandby, excluded)
        {
            return Some(proxy_selection(&self.proxies[index], index));
        }

        if let Some(index) = self.select_degraded_excluding(excluded) {
            warn!(
                %routing_key,
                proxy_index = index,
                proxy_url = %self.proxies[index].url,
                "all preferred proxies unavailable; using degraded route"
            );
            return Some(proxy_selection(&self.proxies[index], index));
        }

        None
    }

    fn best_healthy_candidate(
        &self,
        routing_key: &str,
        role: ProxyRole,
        excluded: Option<usize>,
    ) -> Option<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(index, proxy)| {
                Some(*index) != excluded && proxy.role == role && proxy_is_healthy(proxy)
            })
            .max_by_key(|(_, proxy)| stable_rendezvous_score(routing_key, &proxy.url))
            .map(|(index, _)| index)
    }

    fn select_degraded_excluding(&self, excluded: Option<usize>) -> Option<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(index, proxy)| {
                Some(*index) != excluded
                    && matches!(
                        proxy.status,
                        ProxyStatus::Active | ProxyStatus::Spare | ProxyStatus::Cooldown(_)
                    )
            })
            .min_by_key(|(_, proxy)| match proxy.status {
                ProxyStatus::Cooldown(until) => until
                    .checked_duration_since(std::time::Instant::now())
                    .unwrap_or_default(),
                ProxyStatus::Active => std::time::Duration::ZERO,
                _ => std::time::Duration::MAX,
            })
            .map(|(index, _)| index)
    }

    /// Legacy compatibility: selects proxy for a routing key.
    /// Delegates to `select_proxy_for_key`.
    pub fn get_client(&mut self, api_key: &str) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key(api_key)
    }

    /// Select a proxy excluding a specific index (for retry failover).
    /// Uses the same primary-first, WarmStandby-failover policy but skips
    /// the excluded index.
    pub fn get_client_excluding(
        &mut self,
        api_key: &str,
        exclude_idx: usize,
    ) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key_excluding(api_key, Some(exclude_idx))
    }
}

fn proxy_is_healthy(proxy: &ProxyEntry) -> bool {
    match proxy.status {
        ProxyStatus::Active => true,
        ProxyStatus::Cooldown(until) => std::time::Instant::now() >= until,
        ProxyStatus::Spare | ProxyStatus::Dead { .. } | ProxyStatus::Starting => false,
    }
}

fn proxy_selection(proxy: &ProxyEntry, index: usize) -> (reqwest::Client, String, usize) {
    (proxy.client.clone(), proxy.url.clone(), index)
}
