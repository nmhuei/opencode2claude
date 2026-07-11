//! Egress-node domain types.
//!
//! Serving role, health, circuit state, lifecycle policy, identity, and load are
//! deliberately independent. A node can be a healthy protected standby, a
//! recovering managed primary, or a healthy duplicate exit without overloading
//! one enum with unrelated dimensions.

use reqwest::Client;
use serde::Serialize;
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

#[derive(Debug, Default)]
pub struct ProxyPool {
    pub proxies: Vec<ProxyEntry>,
    pub active_count: usize,
    pub restart_queue: Vec<usize>,
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

pub fn get_primary_ports() -> [u16; 3] {
    [40001, 40002, 40003]
}

pub fn get_warm_standby_ports() -> [u16; 2] {
    [40004, 40005]
}
