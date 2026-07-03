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

    /// Select the best WarmStandby failover for a routing key via Rendezvous hashing.
    pub(crate) fn rendezvous_warm_standby(&self, routing_key: &str) -> Option<usize> {
        let candidates: Vec<usize> = self
            .proxies
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.role == ProxyRole::WarmStandby
                    && matches!(p.status, ProxyStatus::Active | ProxyStatus::Spare)
            })
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates
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
        if self.proxies.is_empty() {
            return None;
        }

        // Step 1: Rendezvous → assigned primary (all primaries, healthy or not)
        let assigned = self.rendezvous_assigned_primary(routing_key);

        if let Some(primary_idx) = assigned {
            let entry = &self.proxies[primary_idx];

            // If cooldown has expired, the proxy is healthy again
            let is_healthy = match entry.status {
                ProxyStatus::Active => true,
                ProxyStatus::Cooldown(until) => std::time::Instant::now() >= until,
                _ => false,
            };

            if is_healthy {
                return Some((entry.client.clone(), entry.url.clone(), primary_idx));
            }

            // Primary is unhealthy → step 2: failover to WarmStandby
            info!(
                "Rendezvous primary #{} ({}) for key '{}' is unavailable (status={:?}). Failing over to WarmStandby.",
                primary_idx, entry.url, routing_key, entry.status
            );
        }

        // Step 2: Rendezvous → assigned WarmStandby
        if let Some(standby_idx) = self.rendezvous_warm_standby(routing_key) {
            let entry = &self.proxies[standby_idx];
            let is_healthy = match entry.status {
                ProxyStatus::Active => true,
                ProxyStatus::Cooldown(until) => std::time::Instant::now() >= until,
                _ => false,
            };

            if is_healthy {
                return Some((entry.client.clone(), entry.url.clone(), standby_idx));
            }
        }

        // Step 3: Degraded — pick any usable proxy
        if let Some(idx) = self.select_degraded() {
            warn!(
                "CRITICAL: All proxies unavailable for key '{}'. Degraded mode, using proxy #{} ({})",
                routing_key, idx, self.proxies[idx].url
            );
            return Some((
                self.proxies[idx].client.clone(),
                self.proxies[idx].url.clone(),
                idx,
            ));
        }

        None
    }

    /// Legacy compatibility: selects proxy for a routing key.
    /// Delegates to `select_proxy_for_key`.
    pub fn get_client(&mut self, api_key: &str) -> Option<(reqwest::Client, String, usize)> {
        self.select_proxy_for_key(api_key)
    }

    /// Select a proxy excluding a specific index (for retry failover).
    /// Actively skips the excluded proxy, picking the next-best alternative:
    /// 1. If excluded index == rendezvous primary → try a different primary, then WarmStandby, then degraded
    /// 2. If excluded index != rendezvous primary → standard selection
    pub fn get_client_excluding(
        &mut self,
        api_key: &str,
        exclude_idx: usize,
    ) -> Option<(reqwest::Client, String, usize)> {
        if self.proxies.is_empty() || exclude_idx >= self.proxies.len() {
            return None;
        }

        // Get the rendezvous-assigned primary for this key
        let assigned = self.rendezvous_assigned_primary(api_key);

        match assigned {
            Some(primary_idx) if primary_idx == exclude_idx => {
                // The excluded proxy is our assigned primary.
                // Check if another primary is available (not the excluded one and healthy).
                let alt_primary = self.proxies.iter().enumerate().find(|(i, p)| {
                    *i != exclude_idx
                        && p.role == ProxyRole::Primary
                        && matches!(p.status, ProxyStatus::Active)
                });

                if let Some((idx, entry)) = alt_primary {
                    return Some((entry.client.clone(), entry.url.clone(), idx));
                }

                // No healthy alternative primary → try WarmStandby
                if let Some(standby_idx) = self.rendezvous_warm_standby(api_key) {
                    let entry = &self.proxies[standby_idx];
                    if matches!(entry.status, ProxyStatus::Active) {
                        info!(
                            "Excluded proxy #{} is the rendezvous primary for key '{}'. Failing over to WarmStandby #{}.",
                            exclude_idx, api_key, standby_idx
                        );
                        return Some((entry.client.clone(), entry.url.clone(), standby_idx));
                    }
                }

                // Last resort: degraded mode (any usable proxy)
                if let Some(idx) = self.select_degraded() {
                    warn!(
                        "CRITICAL: No healthy alternative to excluded proxy #{} for key '{}'. Using degraded proxy #{}.",
                        exclude_idx, api_key, idx
                    );
                    return Some((
                        self.proxies[idx].client.clone(),
                        self.proxies[idx].url.clone(),
                        idx,
                    ));
                }

                None
            }
            _ => {
                // Excluded index is not our primary — use standard selection
                self.select_proxy_for_key(api_key)
            }
        }
    }
}
