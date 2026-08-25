//! Egress-node domain types.
//!
//! Serving role, health, circuit state, lifecycle policy, identity, and load are
//! deliberately independent. A node can be a healthy protected standby, a
//! recovering managed primary, or a healthy duplicate exit without overloading
//! one enum with unrelated dimensions.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub const FAILURE_THRESHOLD: u32 = 2;
pub const RECOVERY_SUCCESS_COUNT: u32 = 2;
pub const COOLDOWN_SECS: u64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressRole {
    Primary,
    WarmStandby,
}

pub type ProxyRole = EgressRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteKind {
    Direct,
    Proxy,
    Standby,
    DirectHybridFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteMetadata {
    pub kind: RouteKind,
    pub proxy_node: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePolicy {
    Managed,
    Protected,
}

pub type ProxyLifecycle = LifecyclePolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
    Recovering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCause {
    Transport,
    RateLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open { until: Instant },
    HalfOpen,
}

impl CircuitState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open { .. } => "open",
            Self::HalfOpen => "half_open",
        }
    }

    pub fn is_open(self, now: Instant) -> bool {
        matches!(self, Self::Open { until } if now < until)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExitIdentity {
    pub public_ip: String,
    pub provider: Option<String>,
    pub colo: Option<String>,
    pub verified_at_unix_secs: u64,
}

impl ExitIdentity {
    pub fn is_fresh(&self, ttl: std::time::Duration) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.verified_at_unix_secs) <= ttl.as_secs()
    }
}

#[derive(Debug)]
pub struct ProxyEntry {
    pub id: String,
    pub url: String,
    pub client: Client,
    pub port: u16,
    pub container_name: String,
    pub role: EgressRole,
    pub lifecycle: LifecyclePolicy,
    /// Whether this primary belongs to the normal serving set. Standby role is
    /// governed separately and is never converted into a managed primary.
    pub routing_enabled: bool,
    pub health: HealthState,
    pub circuit: CircuitState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub restart_attempts: u32,
    pub cooldown_until: Option<Instant>,
    /// Original upstream quota deadline, preserved across WARP restart attempts.
    pub rate_limit_until: Option<Instant>,
    /// Exit IP that received the rate limit and must not be reused before quota expiry.
    pub quarantined_exit_ip: Option<String>,
    pub recovery_cause: Option<RecoveryCause>,
    pub exit_identity: Option<ExitIdentity>,
    /// Node ID that already owns the same verified exit identity.
    pub duplicate_of: Option<String>,
    pub(super) active_requests: Arc<AtomicUsize>,
}

impl ProxyEntry {
    pub fn health_label(&self) -> &'static str {
        match self.health {
            HealthState::Unknown => "unknown",
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Unhealthy => "unhealthy",
            HealthState::Recovering => "recovering",
        }
    }

    pub fn is_duplicate(&self) -> bool {
        self.duplicate_of.is_some()
    }

    pub fn is_closed_and_healthy(&self) -> bool {
        self.health == HealthState::Healthy
            && self.circuit == CircuitState::Closed
            && !self.is_duplicate()
    }

    pub fn provider_rate_limit_active(&self, now: Instant) -> bool {
        self.rate_limit_until.is_some_and(|until| now < until)
    }

    pub fn may_receive_probe_traffic(&self) -> bool {
        self.circuit == CircuitState::HalfOpen && !self.is_duplicate()
    }

    pub fn active_request_count(&self) -> usize {
        self.active_requests.load(Ordering::Acquire)
    }

    pub fn acquire_lease(&self, index: usize) -> EgressLease {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        EgressLease {
            index,
            counter: self.active_requests.clone(),
        }
    }
}

#[derive(Debug)]
pub struct EgressLease {
    index: usize,
    counter: Arc<AtomicUsize>,
}

impl EgressLease {
    pub fn index(&self) -> usize {
        self.index
    }
}

impl Drop for EgressLease {
    fn drop(&mut self) {
        let _ = self
            .counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(1))
            });
    }
}

#[derive(Debug)]
pub struct ProxyPool {
    pub proxies: Vec<ProxyEntry>,
    pub active_count: usize,
    pub restart_queue: Vec<usize>,
    pub require_verified_exit_ip: bool,
    pub identity_ttl: std::time::Duration,
    pub max_restart_attempts: u32,
    /// Round-robin counter for per-request IP load balancing.
    pub round_robin_counter: usize,
}

impl Default for ProxyPool {
    fn default() -> Self {
        Self {
            proxies: Vec::new(),
            active_count: 0,
            restart_queue: Vec::new(),
            require_verified_exit_ip: false,
            identity_ttl: std::time::Duration::from_secs(300),
            max_restart_attempts: 3,
            round_robin_counter: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyNodeStats {
    pub id: String,
    pub port: u16,
    pub role: EgressRole,
    pub lifecycle: LifecyclePolicy,
    pub routing_enabled: bool,
    pub health: HealthState,
    pub circuit: String,
    /// Compatibility label retained for existing dashboard clients.
    pub status: String,
    pub failure_count: u32,
    pub success_count: u32,
    pub restart_attempts: u32,
    pub cooldown_remaining_secs: Option<u64>,
    pub recovery_cause: Option<RecoveryCause>,
    pub quarantined_exit_ip: Option<String>,
    pub active_requests: usize,
    pub exit_identity: Option<ExitIdentity>,
    pub duplicate_of: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyTierStats {
    pub ports: Vec<u16>,
    pub total: usize,
    pub healthy: usize,
    pub degraded: usize,
    pub cooldown: usize,
    pub recovering: usize,
    pub dead: usize,
    pub protected: bool,
    pub unique_verified_exits: usize,
    pub active_requests: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyPoolStats {
    pub policy: String,
    pub primary: ProxyTierStats,
    pub warm_standby: ProxyTierStats,
    pub nodes: Vec<ProxyNodeStats>,
}

pub fn extract_port(url: &str) -> u16 {
    url.rsplit(':')
        .next()
        .and_then(|value| value.trim_end_matches('/').parse().ok())
        .unwrap_or(0)
}

pub fn container_name(url: &str) -> String {
    let port = extract_port(url);
    if (40001..=40099).contains(&port) {
        format!("opencode-warp-{}", port - 40000)
    } else {
        format!("opencode-proxy-{}", port)
    }
}

pub fn is_protected_proxy_port(port: u16) -> bool {
    matches!(port, 40004 | 40005)
}

pub fn is_managed_proxy_port(port: u16) -> bool {
    matches!(port, 40001..=40003)
}

pub fn ensure_not_protected(port: u16) -> Result<(), String> {
    if is_protected_proxy_port(port) {
        Err(format!(
            "refusing to modify protected warm-standby proxy port {} (40004-40005 are protected)",
            port
        ))
    } else {
        Ok(())
    }
}

fn configured_ports(urls: Option<&Vec<String>>) -> Vec<u16> {
    let mut ports = Vec::new();
    for url in urls.into_iter().flatten() {
        let port = extract_port(url);
        if port != 0 && !ports.contains(&port) {
            ports.push(port);
        }
    }
    ports
}

pub fn configured_primary_ports(config: &crate::config::BridgeConfig) -> Vec<u16> {
    configured_ports(config.primary_proxies.as_ref())
}

pub fn configured_warm_standby_ports(config: &crate::config::BridgeConfig) -> Vec<u16> {
    configured_ports(config.warm_standby_proxies.as_ref())
}

#[cfg(test)]
mod configured_topology_tests {
    use super::{configured_primary_ports, configured_warm_standby_ports};
    use crate::config::BridgeConfig;

    #[test]
    fn configured_ports_follow_resolved_one_plus_one_topology() {
        let config = BridgeConfig {
            primary_proxies: Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
            warm_standby_proxies: Some(vec!["socks5h://127.0.0.1:40004".to_string()]),
            ..Default::default()
        };

        assert_eq!(configured_primary_ports(&config), vec![40001]);
        assert_eq!(configured_warm_standby_ports(&config), vec![40004]);
    }

    #[test]
    fn configured_ports_ignore_invalid_zero_ports_and_deduplicate() {
        let config = BridgeConfig {
            primary_proxies: Some(vec![
                "socks5h://127.0.0.1:40001".to_string(),
                "socks5h://127.0.0.1:40001".to_string(),
                "not-a-proxy".to_string(),
            ]),
            warm_standby_proxies: Some(vec![
                "socks5h://127.0.0.1:40004".to_string(),
                "invalid".to_string(),
            ]),
            ..Default::default()
        };

        assert_eq!(configured_primary_ports(&config), vec![40001]);
        assert_eq!(configured_warm_standby_ports(&config), vec![40004]);
    }
}
