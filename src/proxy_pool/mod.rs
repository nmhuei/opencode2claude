//! Proxy pool — multi-agent routing with hot-spare failover and health telemetry.
//!
//! ## Architecture
//!
//! Two-tier proxy pool:
//! - **Primary Managed** (40001–40003): normal traffic via Rendezvous hashing
//! - **Warm-Standby Protected** (40004–40005): failover only, CLI-immutable
//!
//! ## Submodules
//!
//! - [`types`](types/index.html) — Core types: enums, structs, constants, helpers
//! - [`routing`](routing/index.html) — Rendezvous hashing, proxy selection, failover
//! - [`maintenance`](maintenance/index.html) — Health tracking, Docker lifecycle, background tasks

pub mod maintenance;
pub mod routing;
pub mod types;

// Re-export public items so callers use `crate::proxy_pool::ProxyPool` etc.
pub use maintenance::*;
pub use routing::*;
pub use types::*;

use reqwest::Client;
use std::time::{Duration, Instant};
use tracing::{info, warn};

impl ProxyPool {
    /// Create pool from a list of proxy URLs.
    /// Reads `BRIDGE_ACTIVE_PROXY_COUNT` from env to determine active/spare split.
    pub fn new(proxies_urls: &[String]) -> Self {
        let mut proxies = Vec::new();
        for url in proxies_urls {
            if let Ok(proxy) = reqwest::Proxy::all(url) {
                if let Ok(client) = Client::builder()
                    .proxy(proxy)
                    .timeout(Duration::from_secs(600))
                    .pool_max_idle_per_host(10)
                    .build()
                {
                    let port = extract_port(url);
                    let cname = container_name(url);
                    proxies.push(ProxyEntry {
                        url: url.clone(),
                        client,
                        status: ProxyStatus::Active,
                        port,
                        container_name: cname,
                        role: if is_protected_proxy_port(port) {
                            ProxyRole::WarmStandby
                        } else {
                            ProxyRole::Primary
                        },
                        lifecycle: if is_protected_proxy_port(port) {
                            ProxyLifecycle::Protected
                        } else {
                            ProxyLifecycle::Managed
                        },
                        consecutive_failures: 0,
                        consecutive_successes: 0,
                    });
                    info!("Added proxy to pool: {}", url);
                } else {
                    warn!("Failed to build reqwest Client for proxy: {}", url);
                }
            } else {
                warn!("Invalid proxy URL: {}", url);
            }
        }

        let total = proxies.len();
        let active_count = if total == 0 {
            0
        } else {
            std::env::var("BRIDGE_ACTIVE_PROXY_COUNT")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or_else(|| total.saturating_sub(1))
                .max(1)
                .min(total)
        };

        // Set indices >= active_count as Spare (but NOT WarmStandby proxies)
        for proxy in proxies.iter_mut().take(total).skip(active_count) {
            if proxy.role != ProxyRole::WarmStandby {
                proxy.status = ProxyStatus::Spare;
            }
        }

        info!(
            "Proxy pool initialized: {} total, {} active, {} spare",
            total,
            active_count,
            total.saturating_sub(active_count)
        );

        Self {
            proxies,
            active_count,
            restart_queue: Vec::new(),
        }
    }

    // ── Status helpers ──

    fn remaining_cooldown(status: &ProxyStatus) -> Duration {
        match status {
            ProxyStatus::Cooldown(until) => until
                .checked_duration_since(Instant::now())
                .unwrap_or_default(),
            ProxyStatus::Active => Duration::ZERO,
            _ => Duration::MAX,
        }
    }

    fn is_usable(status: &ProxyStatus) -> bool {
        matches!(
            status,
            ProxyStatus::Active | ProxyStatus::Spare | ProxyStatus::Cooldown(_)
        )
    }

    /// Drain the restart queue.
    pub fn drain_restart_queue(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.restart_queue)
    }

    // ── Snapshot ──

    /// Build a full health snapshot of the proxy pool.
    pub fn snapshot(&self) -> ProxyPoolStats {
        let mut primary_ports = Vec::new();
        let mut ws_ports = Vec::new();
        let mut primary_healthy = 0usize;
        let mut primary_degraded = 0usize;
        let mut primary_cooldown = 0usize;
        let mut primary_recovering = 0usize;
        let mut primary_dead = 0usize;
        let mut ws_healthy = 0usize;
        let mut ws_degraded = 0usize;
        let mut ws_cooldown = 0usize;
        let mut ws_recovering = 0usize;
        let mut ws_dead = 0usize;
        let mut nodes = Vec::new();

        for p in &self.proxies {
            let status_str = p.status.description().to_string();
            let cooldown_remaining = if let ProxyStatus::Cooldown(until) = p.status {
                Some(
                    until
                        .checked_duration_since(Instant::now())
                        .unwrap_or_default()
                        .as_secs(),
                )
            } else {
                None
            };

            nodes.push(ProxyNodeStats {
                port: p.port,
                role: p.role,
                lifecycle: p.lifecycle,
                status: status_str,
                failure_count: p.consecutive_failures,
                success_count: p.consecutive_successes,
                cooldown_remaining_secs: cooldown_remaining,
            });

            match p.role {
                ProxyRole::Primary => {
                    primary_ports.push(p.port);
                    match p.status {
                        ProxyStatus::Active => primary_healthy += 1,
                        ProxyStatus::Spare => primary_degraded += 1,
                        ProxyStatus::Cooldown(_) => primary_cooldown += 1,
                        ProxyStatus::Dead { .. } => primary_dead += 1,
                        ProxyStatus::Starting => primary_recovering += 1,
                    }
                }
                ProxyRole::WarmStandby => {
                    ws_ports.push(p.port);
                    match p.status {
                        ProxyStatus::Active => ws_healthy += 1,
                        ProxyStatus::Spare => ws_degraded += 1,
                        ProxyStatus::Cooldown(_) => ws_cooldown += 1,
                        ProxyStatus::Dead { .. } => ws_dead += 1,
                        ProxyStatus::Starting => ws_recovering += 1,
                    }
                }
            }
        }

        let total_primary = primary_ports.len();
        let total_ws = ws_ports.len();

        ProxyPoolStats {
            policy: "primary-with-warm-standby".to_string(),
            primary: ProxyTierStats {
                ports: primary_ports,
                total: total_primary,
                healthy: primary_healthy,
                degraded: primary_degraded,
                cooldown: primary_cooldown,
                recovering: primary_recovering,
                dead: primary_dead,
                protected: false,
            },
            warm_standby: ProxyTierStats {
                ports: ws_ports,
                total: total_ws,
                healthy: ws_healthy,
                degraded: ws_degraded,
                cooldown: ws_cooldown,
                recovering: ws_recovering,
                dead: ws_dead,
                protected: true,
            },
            nodes,
        }
    }

    // ── Private ──

    /// Select the proxy closest to cooldown end (degraded mode).
    pub(crate) fn select_degraded(&self) -> Option<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(_, p)| Self::is_usable(&p.status))
            .min_by_key(|(_, p)| Self::remaining_cooldown(&p.status))
            .map(|(i, _)| i)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_urls(count: usize) -> Vec<String> {
        (0..count)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect()
    }

    #[test]
    fn test_proxy_pool_mapping() {
        let urls = make_test_urls(3);
        let mut pool = ProxyPool::new(&urls);
        // 3 proxies → 2 active + 1 spare
        assert_eq!(pool.proxies.len(), 3);
        assert_eq!(pool.active_count, 2);

        // Same API key should always map to same proxy index
        let res1 = pool.get_client("agent-1").unwrap();
        let res2 = pool.get_client("agent-1").unwrap();
        assert_eq!(res1.2, res2.2);

        // Different API keys may map to different indexes
        let res3 = pool.get_client("agent-2").unwrap();
        info!("agent-1 mapped to preferred proxy index {}", res1.2);
        info!("agent-2 mapped to preferred proxy index {}", res3.2);
    }

    #[test]
    fn test_sticky_mapping_stable() {
        let urls = make_test_urls(3);
        let pool = ProxyPool::new(&urls);

        let agent = "sticky-agent-42";
        let first = pool.select_proxy_for_key(agent).unwrap().2;

        for _ in 0..100 {
            let result = pool.select_proxy_for_key(agent).unwrap();
            assert_eq!(
                result.2, first,
                "sticky mapping changed for key '{}' on iteration",
                agent
            );
            assert_eq!(
                pool.proxies[result.2].role,
                ProxyRole::Primary,
                "sticky agent mapped to non-Primary proxy"
            );
        }
    }

    #[test]
    fn test_affected_agent_only_remap() {
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let mut pool = ProxyPool::new(&urls);

        let a_idx = pool.select_proxy_for_key("agent_a").unwrap().2;
        let b_idx = pool.select_proxy_for_key("agent_b").unwrap().2;
        let c_idx = pool.select_proxy_for_key("agent_c").unwrap().2;

        assert_eq!(pool.proxies[a_idx].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[b_idx].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[c_idx].role, ProxyRole::Primary);

        let a_primary = a_idx;
        let b_primary = b_idx;
        let c_primary = c_idx;

        pool.mark_rate_limited(b_primary, Duration::from_secs(300));

        let b_failover = pool.select_proxy_for_key("agent_b").unwrap().2;
        assert_eq!(
            pool.proxies[b_failover].role,
            ProxyRole::WarmStandby,
            "agent_b should failover to WarmStandby, got index {} role {:?}",
            b_failover,
            pool.proxies[b_failover].role
        );

        let a_after = pool.select_proxy_for_key("agent_a").unwrap().2;
        assert_eq!(
            a_after, a_primary,
            "agent_a remapped from primary {} to {}, expected no change",
            a_primary, a_after
        );

        let c_after = pool.select_proxy_for_key("agent_c").unwrap().2;
        assert_eq!(
            c_after, c_primary,
            "agent_c remapped from primary {} to {}, expected no change",
            c_primary, c_after
        );
    }

    #[test]
    fn test_temporary_failover_to_warm_standby() {
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let mut pool = ProxyPool::new(&urls);

        let primary = pool.select_proxy_for_key("failover-agent").unwrap().2;
        assert_eq!(pool.proxies[primary].role, ProxyRole::Primary);

        pool.mark_rate_limited(primary, Duration::from_secs(300));

        let result = pool.select_proxy_for_key("failover-agent").unwrap();
        assert_eq!(
            pool.proxies[result.2].role,
            ProxyRole::WarmStandby,
            "failover should route to WarmStandby, got idx {} role {:?}",
            result.2,
            pool.proxies[result.2].role
        );
    }

    #[test]
    fn test_recovery_returns_to_primary() {
        let urls: Vec<String> = (0..3)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let mut pool = ProxyPool::new(&urls);

        let primary_idx = pool.select_proxy_for_key("recovery-agent").unwrap().2;
        assert_eq!(pool.proxies[primary_idx].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[primary_idx].status, ProxyStatus::Active);

        pool.mark_rate_limited(primary_idx, Duration::from_secs(0));

        let result = pool.select_proxy_for_key("recovery-agent").unwrap();
        assert_eq!(
            result.2, primary_idx,
            "after cooldown expiry, agent should return to original primary {} not {}",
            primary_idx, result.2
        );
    }

    #[test]
    fn test_no_standby_if_selected_primary_healthy() {
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let pool = ProxyPool::new(&urls);

        for key in &["test-a", "test-b", "test-c", "test-d", "test-e"] {
            let result = pool.select_proxy_for_key(key).unwrap();
            assert_eq!(
                pool.proxies[result.2].role,
                ProxyRole::Primary,
                "key '{}' selected standby when primary was healthy",
                key
            );
        }
    }

    #[test]
    fn test_rendezvous_deterministic() {
        let urls: Vec<String> = (0..3)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let _pool = ProxyPool::new(&urls);

        let score1 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40001");
        let score2 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40001");
        assert_eq!(score1, score2, "rendezvous score must be deterministic");

        let score3 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40002");
        assert_ne!(
            score1, score3,
            "different nodes should have different scores"
        );
    }

    #[test]
    fn test_warm_standby_excluded_from_normal_routing() {
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let mut pool = ProxyPool::new(&urls);

        assert_eq!(pool.proxies.len(), 5);
        assert_eq!(pool.proxies[0].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[1].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[2].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[3].role, ProxyRole::WarmStandby);
        assert_eq!(pool.proxies[4].role, ProxyRole::WarmStandby);

        for key in &["alpha", "beta", "gamma", "delta", "epsilon"] {
            let (_, _, idx) = pool.get_client(key).unwrap();
            assert!(
                pool.proxies[idx].role == ProxyRole::Primary,
                "get_client('{}') returned WarmStandby (idx {}), expected Primary",
                key,
                idx
            );
        }

        for i in 0..3 {
            pool.mark_rate_limited(i, Duration::from_secs(300));
        }

        let (_, _, idx) = pool.get_client("failover-test").unwrap();
        assert!(
            pool.proxies[idx].role == ProxyRole::WarmStandby,
            "expected WarmStandby in failover, got role={:?} at idx={}",
            pool.proxies[idx].role,
            idx
        );
    }

    #[test]
    fn test_empty_pool_returns_none() {
        let mut pool = ProxyPool::default();
        assert!(pool.get_client("test").is_none());
    }

    #[test]
    fn test_mark_healthy() {
        let urls = make_test_urls(1);
        let mut pool = ProxyPool::new(&urls);

        pool.mark_rate_limited(0, Duration::from_secs(60));
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));

        pool.mark_healthy(0);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));
    }

    #[test]
    fn test_drain_restart_queue() {
        let urls = make_test_urls(3);
        let mut pool = ProxyPool::new(&urls);
        assert!(pool.drain_restart_queue().is_empty());

        pool.restart_queue.push(0);
        pool.restart_queue.push(1);
        assert_eq!(pool.drain_restart_queue().len(), 2);
        assert!(pool.drain_restart_queue().is_empty());
    }

    #[test]
    fn test_container_name_generation() {
        assert_eq!(
            container_name("socks5://127.0.0.1:40001"),
            "opencode-warp-1"
        );
        assert_eq!(
            container_name("socks5://127.0.0.1:40005"),
            "opencode-warp-5"
        );
        assert_eq!(
            container_name("http://127.0.0.1:9999"),
            "opencode-proxy-9999"
        );
    }

    #[test]
    fn test_extract_port() {
        assert_eq!(extract_port("socks5://127.0.0.1:40001"), 40001);
        assert_eq!(extract_port("http://127.0.0.1:8080/"), 8080);
        assert_eq!(extract_port("invalid"), 0);
    }

    #[test]
    fn test_record_failure_enters_cooldown() {
        let urls = make_test_urls(1);
        let mut pool = ProxyPool::new(&urls);

        assert_eq!(pool.proxies[0].consecutive_failures, 0);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));

        pool.record_failure(0);
        assert_eq!(pool.proxies[0].consecutive_failures, 1);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));

        pool.record_failure(0);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));
    }

    #[test]
    fn test_http_400_does_not_mark_proxy_failed() {
        let urls = make_test_urls(1);
        let mut pool = ProxyPool::new(&urls);

        pool.record_failure(0);
        assert_eq!(pool.proxies[0].consecutive_failures, 1);

        pool.record_success(0);
        assert_eq!(pool.proxies[0].consecutive_failures, 0);
        assert_eq!(pool.proxies[0].consecutive_successes, 1);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));
    }

    #[test]
    fn test_health_json_contains_proxy_pool() {
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let pool = ProxyPool::new(&urls);
        let stats = pool.snapshot();

        assert_eq!(stats.policy, "primary-with-warm-standby");

        assert_eq!(stats.primary.ports, vec![40001, 40002, 40003]);
        assert_eq!(stats.primary.total, 3);
        assert_eq!(stats.primary.healthy, 3);
        assert!(!stats.primary.protected);

        assert_eq!(stats.warm_standby.ports, vec![40004, 40005]);
        assert_eq!(stats.warm_standby.total, 2);
        assert_eq!(stats.warm_standby.healthy, 2);
        assert!(stats.warm_standby.protected);

        assert_eq!(stats.nodes.len(), 5);
        assert_eq!(stats.nodes[0].role, ProxyRole::Primary);
        assert_eq!(stats.nodes[3].role, ProxyRole::WarmStandby);
        assert_eq!(stats.nodes[3].lifecycle, ProxyLifecycle::Protected);
        assert!(stats.nodes[0].cooldown_remaining_secs.is_none());
    }

    #[test]
    fn test_snapshot_shows_cooldown_count() {
        let urls = make_test_urls(5);
        let mut pool = ProxyPool::new(&urls);

        pool.mark_rate_limited(1, Duration::from_secs(60));
        pool.mark_rate_limited(2, Duration::from_secs(120));
        pool.mark_rate_limited(3, Duration::from_secs(300));

        let stats = pool.snapshot();

        assert_eq!(stats.primary.cooldown, 2);
        assert_eq!(stats.primary.healthy, 1);
        assert_eq!(stats.warm_standby.cooldown, 1);
        assert_eq!(stats.warm_standby.healthy, 1);
    }

    #[test]
    fn test_record_success_recovers_after_threshold() {
        let urls = make_test_urls(1);
        let mut pool = ProxyPool::new(&urls);

        pool.record_failure(0);
        pool.record_failure(0);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));
        assert_eq!(pool.proxies[0].consecutive_successes, 0);

        pool.record_success(0);
        assert!(
            matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)),
            "still in cooldown after 1 success"
        );
        assert_eq!(pool.proxies[0].consecutive_successes, 1);

        pool.record_success(0);
        assert!(
            matches!(pool.proxies[0].status, ProxyStatus::Active),
            "recovered after {} successes",
            RECOVERY_SUCCESS_COUNT
        );
        assert_eq!(pool.proxies[0].consecutive_failures, 0);
        assert_eq!(pool.proxies[0].consecutive_successes, 0);
    }
}
