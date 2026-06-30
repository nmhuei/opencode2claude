//! Core types for the proxy pool routing and lifecycle management.
//!
//! Defines the proxy status machine, role/lifecycle enums, pool structure,
//! health snapshot types, and constants used across routing and maintenance.

use reqwest::Client;
use serde::Serialize;
use std::time::Instant;

// ── Routing policy constants ──

/// Consecutive failures before proxy enters cooldown.
pub const FAILURE_THRESHOLD: u32 = 2;
/// Consecutive successes after cooldown to be considered fully healthy.
pub const RECOVERY_SUCCESS_COUNT: u32 = 2;
/// Default cooldown duration when failure threshold is reached (seconds).
pub const COOLDOWN_SECS: u64 = 120;

// ── Types ──

/// Proxy state machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProxyStatus {
    /// Active — routing requests, index < active_count
    Active,
    /// Spare — ready to replace an active slot, index >= active_count
    Spare,
    /// Cooldown — resting due to rate-limit; Instant = when cooldown expires
    Cooldown(Instant),
    /// Dead — proxy is unusable, needs restart (tracks attempts)
    Dead { restart_attempts: u32 },
    /// Starting — container being initialized, awaiting health verification
    Starting,
}

impl ProxyStatus {
    /// Human-readable description of the current status.
    pub fn description(&self) -> &'static str {
        match self {
            ProxyStatus::Active => "healthy",
            ProxyStatus::Spare => "spare",
            ProxyStatus::Cooldown(_) => "cooldown",
            ProxyStatus::Dead { .. } => "dead",
            ProxyStatus::Starting => "starting",
        }
    }
}

/// Proxy role in the two-tier architecture.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum ProxyRole {
    /// Primary managed proxy (40001-40003) — CLI may restart/stop/recover
    Primary,
    /// Warm-standby protected proxy (40004-40005) — CLI may only health-check
    WarmStandby,
}

/// Proxy lifecycle management policy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum ProxyLifecycle {
    /// Fully managed by CLI — can be restarted, purged, recreated
    Managed,
    /// Protected — never stopped, restarted, purged, or recreated by CLI
    Protected,
}

/// One entry in the proxy pool.
#[derive(Debug)]
pub struct ProxyEntry {
    pub url: String,
    pub client: Client,
    pub status: ProxyStatus,
    pub port: u16,
    pub container_name: String,
    /// Proxy role in the two-tier architecture (Primary/WarmStandby).
    pub role: ProxyRole,
    /// Proxy lifecycle management policy (Managed/Protected).
    pub lifecycle: ProxyLifecycle,
    /// Consecutive failures since last healthy state.
    pub consecutive_failures: u32,
    /// Consecutive successes since last healthy/cooldown state.
    pub consecutive_successes: u32,
}

/// Proxy pool with hot-spare model.
///
/// - indices `[0..active_count)` are active slots (Active, Cooldown, Dead, Starting)
/// - indices `[active_count..]` are spare slots (usually Spare)
/// - When an active dies → swap status with a spare, push dead index into restart_queue
#[derive(Debug, Default)]
pub struct ProxyPool {
    pub proxies: Vec<ProxyEntry>,
    /// Number of active proxy slots (set in constructor).
    pub active_count: usize,
    pub restart_queue: Vec<usize>,
}

// ── Stats types (exposed via /health and status) ──

/// Snapshot of a single proxy node for health/status display.
#[derive(Debug, Clone, Serialize)]
pub struct ProxyNodeStats {
    pub port: u16,
    pub role: ProxyRole,
    pub lifecycle: ProxyLifecycle,
    pub status: String,
    pub failure_count: u32,
    pub success_count: u32,
    pub cooldown_remaining_secs: Option<u64>,
}

/// Aggregate stats for a tier (primary or warm-standby).
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
}

/// Full proxy pool snapshot for health/status endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct ProxyPoolStats {
    pub policy: String,
    pub primary: ProxyTierStats,
    pub warm_standby: ProxyTierStats,
    pub nodes: Vec<ProxyNodeStats>,
}

// ── Helpers ──

pub fn extract_port(url: &str) -> u16 {
    url.rsplit(':')
        .next()
        .and_then(|s| s.trim_end_matches('/').parse().ok())
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

/// Returns true if the port is a protected warm-standby proxy (40004-40005).
pub fn is_protected_proxy_port(port: u16) -> bool {
    matches!(port, 40004 | 40005)
}

/// Ensures a given port is NOT a protected warm-standby proxy.
/// Returns an error if it is, preventing destructive operations.
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

/// Returns the primary managed proxy ports (40001-40003).
pub fn get_primary_ports() -> [u16; 3] {
    [40001, 40002, 40003]
}

/// Returns the warm-standby protected proxy ports (40004-40005).
pub fn get_warm_standby_ports() -> [u16; 2] {
    [40004, 40005]
}
