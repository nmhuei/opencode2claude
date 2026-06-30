// ── Routing policy constants ──

/// Consecutive failures before proxy enters cooldown.
pub const FAILURE_THRESHOLD: u32 = 2;
/// Consecutive successes after cooldown to be considered fully healthy.
pub const RECOVERY_SUCCESS_COUNT: u32 = 2;
/// Default cooldown duration when failure threshold is reached (seconds).
pub const COOLDOWN_SECS: u64 = 120;

use reqwest::Client;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{error, info, warn};

// ── Types ──

/// Proxy trạng thái machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProxyStatus {
    /// Active — được dùng để route request, index < active_count
    Active,
    /// Spare — sẵn sàng thế chỗ active khi cần, index >= active_count
    Spare,
    /// Cooldown — đang tạm nghỉ vì rate-limit, Instant = thời điểm HẾT cooldown (tương lai)
    Cooldown(Instant),
    /// Dead — proxy chết, cần restart (có counter)
    Dead { restart_attempts: u32 },
    /// Starting — container đang được khởi động, chờ verify
    Starting,
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

/// Một entry trong proxy pool.
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
    #[allow(dead_code)]
    pub lifecycle: ProxyLifecycle,
    /// Consecutive failures since last healthy state.
    #[allow(dead_code)]
    pub consecutive_failures: u32,
    /// Consecutive successes since last healthy/cooldown state.
    #[allow(dead_code)]
    pub consecutive_successes: u32,
}

/// Proxy pool với hot-spare model.
///
/// - indices `[0..active_count)` là active slots (có thể Active, Cooldown, Dead, Starting)
/// - indices `[active_count..]` là spare slots (thường là Spare)
/// - Khi 1 active chết → swap status với 1 spare, push dead index vào restart_queue
#[derive(Debug, Default)]
pub struct ProxyPool {
    pub proxies: Vec<ProxyEntry>,
    /// Number of active proxy slots (used in constructor to split active/spare).
    #[allow(dead_code)]
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

fn extract_port(url: &str) -> u16 {
    url.rsplit(':')
        .next()
        .and_then(|s| s.trim_end_matches('/').parse().ok())
        .unwrap_or(0)
}

fn container_name(url: &str) -> String {
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

// ── Implementation ──

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

impl ProxyPool {
    /// Create pool from a list of proxy URLs.
    /// Reads BRIDGE_ACTIVE_PROXY_COUNT from env to determine active/spare split.
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

    // ── Health tracking ──

    /// Record a success for a proxy, building toward recovery threshold.
    pub fn record_success(&mut self, idx: usize) {
        if idx >= self.proxies.len() {
            return;
        }
        let entry = &mut self.proxies[idx];
        entry.consecutive_failures = 0;
        entry.consecutive_successes = entry.consecutive_successes.saturating_add(1);

        // Auto-recover from cooldown after enough consecutive successes
        if matches!(entry.status, ProxyStatus::Cooldown(_))
            && entry.consecutive_successes >= RECOVERY_SUCCESS_COUNT
        {
            entry.status = ProxyStatus::Active;
            entry.consecutive_failures = 0;
            entry.consecutive_successes = 0;
            info!(
                "Proxy #{} ({}) recovered from cooldown after {} consecutive successes.",
                idx, entry.url, RECOVERY_SUCCESS_COUNT
            );
        }
    }

    /// Record a failure for a proxy, potentially triggering cooldown.
    pub fn record_failure(&mut self, idx: usize) {
        if idx >= self.proxies.len() {
            return;
        }
        self.proxies[idx].consecutive_successes = 0;
        let failures = self.proxies[idx].consecutive_failures.saturating_add(1);
        self.proxies[idx].consecutive_failures = failures;

        if failures >= FAILURE_THRESHOLD {
            let duration = Duration::from_secs(COOLDOWN_SECS);
            self.mark_rate_limited(idx, duration);
            info!(
                "Proxy #{} ({}) entered cooldown after {} consecutive failures ({}s).",
                idx, self.proxies[idx].url, failures, COOLDOWN_SECS
            );
        }
    }

    // ── Selection helpers ──

    /// Returns indices of all proxies with Primary role and healthy status (Active or Spare).
    #[allow(dead_code)]
    fn healthy_primary_indices(&self) -> Vec<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.role == ProxyRole::Primary
                    && matches!(p.status, ProxyStatus::Active | ProxyStatus::Spare)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Returns indices of all proxies with WarmStandby role and healthy status.
    fn healthy_warm_standby_indices(&self) -> Vec<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.role == ProxyRole::WarmStandby
                    && matches!(p.status, ProxyStatus::Active | ProxyStatus::Spare)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Returns the rendezvous-assigned primary for a routing key,
    /// considering ALL primaries regardless of health status.
    /// This ensures sticky assignment: even if a primary is on cooldown,
    /// the key still maps to the same slot, enabling correct WarmStandby failover.
    fn rendezvous_assigned_primary(&self, routing_key: &str) -> Option<usize> {
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
    fn rendezvous_warm_standby(&self, routing_key: &str) -> Option<usize> {
        let candidates = self.healthy_warm_standby_indices();
        if candidates.is_empty() {
            return None;
        }
        candidates
            .iter()
            .copied()
            .max_by_key(|idx| stable_rendezvous_score(routing_key, &self.proxies[*idx].url))
    }

    // ── Main API ──

    /// Select a proxy for the given routing key following the Phase 5 routing contract:
    ///
    /// 1. Use Primary proxies 40001–40003 for normal traffic.
    /// 2. Use WarmStandby proxies 40004–40005 only when the selected primary
    ///    proxy is unhealthy/cooldown/dead.
    /// 3. Affected-agent-only remap: failure of one primary does NOT remap
    ///    agents assigned to healthy primaries.
    /// 4. Implement Rendezvous hashing for stable sticky determinism.
    /// 5. Comply with cooldown/recovery policy.
    ///
    /// Returns `(Client, proxy_url, index)` or `None` if no proxy is available.
    pub fn select_proxy_for_key(&self, routing_key: &str) -> Option<(Client, String, usize)> {
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
                ProxyStatus::Cooldown(until) => Instant::now() >= until,
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
                ProxyStatus::Cooldown(until) => Instant::now() >= until,
                _ => false,
            };

            if is_healthy {
                return Some((entry.client.clone(), entry.url.clone(), standby_idx));
            }
        }

        // Step 3: Degraded — pick any usable proxy
        let degraded = self.select_degraded();
        if let Some(idx) = degraded {
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
    /// Delegates to `select_proxy_for_key`. Provided as an alias for callers
    /// that haven't been updated to the new API name yet.
    pub fn get_client(&mut self, api_key: &str) -> Option<(Client, String, usize)> {
        self.select_proxy_for_key(api_key)
    }

    /// Select a proxy excluding a specific index (for retry failover).
    /// Uses the same primary-first, WarmStandby-failover policy but skips
    /// the excluded index.
    pub fn get_client_excluding(
        &mut self,
        api_key: &str,
        _exclude_idx: usize,
    ) -> Option<(Client, String, usize)> {
        // For Phase 5, we use select_proxy_for_key which is role-aware.
        // If the excluded index happens to be the rendezvous primary, we
        // fall through to WarmStandby or degraded.
        self.select_proxy_for_key(api_key)
    }

    /// Mark a proxy as rate-limited for a specific duration.
    pub fn mark_rate_limited(&mut self, idx: usize, duration: Duration) {
        if idx < self.proxies.len() {
            let until = Instant::now() + duration;
            self.proxies[idx].status = ProxyStatus::Cooldown(until);
            warn!(
                "Proxy #{} ({}) marked as rate-limited until {:?}",
                idx, self.proxies[idx].url, until
            );
        }
    }

    /// Mark rate-limited with adaptive duration (base × 2^retry × jitter).
    pub fn mark_rate_limited_adaptive(&mut self, idx: usize, retry_count: u32) {
        let base_secs = 60 * 2u64.pow(retry_count.min(3));
        // Deterministic jitter ±25% — no rand crate needed
        let jitter_factor = match idx % 4 {
            0 => 100,
            1 => 85,
            2 => 115,
            _ => 95,
        };
        let secs = base_secs * jitter_factor / 100;
        let duration = Duration::from_secs(secs);
        self.mark_rate_limited(idx, duration);
    }

    /// Mark a proxy as healthy (clear cooldown/dead).
    #[allow(dead_code)]
    pub fn mark_healthy(&mut self, idx: usize) {
        if idx < self.proxies.len() {
            self.proxies[idx].status = ProxyStatus::Active;
            info!(
                "Proxy #{} ({}) marked as healthy.",
                idx, self.proxies[idx].url
            );
        }
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
    fn select_degraded(&self) -> Option<usize> {
        self.proxies
            .iter()
            .enumerate()
            .filter(|(_, p)| Self::is_usable(&p.status))
            .min_by_key(|(_, p)| Self::remaining_cooldown(&p.status))
            .map(|(i, _)| i)
    }
}

// ── Stable hash helpers ──

/// Deterministic 64-bit score for Rendezvous hashing.
///
/// Uses DefaultHasher (std) for now. This is deterministic within the same
/// process execution but may vary across Rust versions. For fully stable
/// cross-build determinism, replace with sha2 or blake3.
pub fn stable_rendezvous_score(key: &str, node_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    node_id.hash(&mut hasher);
    hasher.finish()
}

// ── Background Tasks (Docker restart, health monitoring) ──

/// Process restart queue: docker rm -f + docker run + verify.
/// Processes one container at a time; re-queues on failure (max 3 attempts).
pub async fn process_restart_queue(pool: Arc<TokioRwLock<ProxyPool>>) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        let indices: Vec<usize> = pool.write().await.drain_restart_queue();
        for idx in indices {
            restart_container(idx, pool.clone()).await;
        }
    }
}

/// TCP health monitor — checks Dead/Starting proxies every 10s.
/// If TCP connect succeeds, marks proxy as Spare.
pub async fn health_monitor(pool: Arc<TokioRwLock<ProxyPool>>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;

        let targets: Vec<(usize, u16)> = {
            let p = pool.read().await;
            p.proxies
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    matches!(e.status, ProxyStatus::Dead { .. } | ProxyStatus::Starting)
                })
                .map(|(i, e)| (i, e.port))
                .collect()
        };

        for (idx, port) in targets {
            if port == 0 {
                continue;
            }
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
                .await
                .is_ok()
            {
                let mut p = pool.write().await;
                if idx < p.proxies.len() {
                    // Only mark as Spare if still dead (not manually re-assigned)
                    if matches!(
                        p.proxies[idx].status,
                        ProxyStatus::Dead { .. } | ProxyStatus::Starting
                    ) {
                        p.proxies[idx].status = ProxyStatus::Spare;
                        info!(
                            "Proxy #{} ({}) recovered via TCP health check.",
                            idx, p.proxies[idx].container_name
                        );
                    }
                }
            }
        }
    }
}

/// Restart a single Docker container by proxy pool index.
async fn restart_container(idx: usize, pool: Arc<TokioRwLock<ProxyPool>>) {
    let (port, container_name) = {
        let p = pool.read().await;
        if idx >= p.proxies.len() {
            return;
        }
        (p.proxies[idx].port, p.proxies[idx].container_name.clone())
    };

    if port == 0 {
        warn!("Cannot restart proxy #{}: unknown port", idx);
        return;
    }

    if let Err(msg) = ensure_not_protected(port) {
        warn!("{}", msg);
        return;
    }

    info!(
        "Restarting proxy container #{} ({}) on port {}...",
        idx, container_name, port
    );

    // Mark as Starting
    pool.write().await.proxies[idx].status = ProxyStatus::Starting;

    // docker rm -f
    let rm = tokio::process::Command::new("docker")
        .args(["rm", "-f", &container_name])
        .output()
        .await;

    match &rm {
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "docker rm -f {} (may not exist): {}",
                container_name, stderr
            );
        }
        Err(e) => {
            warn!("docker rm -f {} failed: {}", container_name, e);
        }
        _ => {}
    }

    // docker run -d --name ... --restart always ...
    let run = tokio::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            &container_name,
            "--restart",
            "always",
            "--cap-add=NET_ADMIN",
            "--sysctl",
            "net.ipv4.conf.all.src_valid_mark=1",
            "-p",
            &format!("{}:9091", port),
            "ghcr.io/mon-ius/docker-warp-socks:latest",
        ])
        .output()
        .await;

    match run {
        Ok(output) if output.status.success() => {
            info!("Container {} created successfully.", container_name);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Failed to create container {}: {}", container_name, stderr);
            requeue_or_giveup(idx, &pool, "docker create failed").await;
            return;
        }
        Err(e) => {
            error!(
                "Docker command failed for {}: {}. Is docker installed?",
                container_name, e
            );
            requeue_or_giveup(idx, &pool, "docker command error").await;
            return;
        }
    }

    // Verify connectivity
    let ok = verify_proxy_socks(port).await;

    let mut p = pool.write().await;
    if idx >= p.proxies.len() {
        return;
    }

    if ok {
        p.proxies[idx].status = ProxyStatus::Spare;
        info!(
            "Proxy #{} ({}) restarted and verified as Spare.",
            idx, container_name
        );
    } else {
        let attempts = match p.proxies[idx].status {
            ProxyStatus::Dead {
                restart_attempts: n,
            } => n + 1,
            _ => 1,
        };
        if attempts < 3 {
            p.proxies[idx].status = ProxyStatus::Dead {
                restart_attempts: attempts,
            };
            p.restart_queue.push(idx);
            warn!(
                "Proxy #{} restart verify failed, re-queuing (attempt {}/3)",
                idx, attempts
            );
        } else {
            p.proxies[idx].status = ProxyStatus::Dead {
                restart_attempts: attempts,
            };
            error!("Proxy #{} failed restart after 3 attempts. Giving up.", idx);
        }
    }
}

/// Re-queue a failed restart or give up after 3 attempts.
async fn requeue_or_giveup(idx: usize, pool: &Arc<TokioRwLock<ProxyPool>>, reason: &str) {
    let mut p = pool.write().await;
    if idx >= p.proxies.len() {
        return;
    }
    let attempts = match p.proxies[idx].status {
        ProxyStatus::Dead {
            restart_attempts: n,
        } => n + 1,
        _ => 1,
    };
    if attempts < 3 {
        p.proxies[idx].status = ProxyStatus::Dead {
            restart_attempts: attempts,
        };
        p.restart_queue.push(idx);
        warn!(
            "Proxy #{} restart queued (attempt {}/3): {}",
            idx, attempts, reason
        );
    } else {
        p.proxies[idx].status = ProxyStatus::Dead {
            restart_attempts: attempts,
        };
        error!(
            "Proxy #{} failed restart after 3 attempts ({}). Giving up.",
            idx, reason
        );
    }
}

/// Verify SOCKS5 proxy connectivity via cloudflare CDN trace.
async fn verify_proxy_socks(port: u16) -> bool {
    let proxy_url = format!("socks5h://127.0.0.1:{}", port);
    let proxy = match reqwest::Proxy::all(&proxy_url) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Invalid proxy URL '{}' in verify_proxy_socks: {}",
                proxy_url, e
            );
            return false;
        }
    };
    let client = match reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    for attempt in 1..=12 {
        if client
            .get("https://cloudflare.com/cdn-cgi/trace")
            .send()
            .await
            .is_ok()
        {
            info!("Proxy on port {} verified successfully.", port);
            return true;
        }
        if attempt < 12 {
            info!(
                "Waiting for proxy on port {}... (attempt {}/12)",
                port, attempt
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    warn!(
        "Proxy on port {} failed verification after 12 attempts.",
        port
    );
    false
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
        // Given all 3 primaries healthy
        let urls = make_test_urls(3);
        let pool = ProxyPool::new(&urls);

        // When same agent key selects proxy 100 times
        let agent = "sticky-agent-42";
        let first = pool.select_proxy_for_key(agent).unwrap().2;

        for _ in 0..100 {
            let result = pool.select_proxy_for_key(agent).unwrap();
            // Then selected primary is always the same
            assert_eq!(
                result.2, first,
                "sticky mapping changed for key '{}' on iteration",
                agent
            );
            // And selected role is Primary
            assert_eq!(
                pool.proxies[result.2].role,
                ProxyRole::Primary,
                "sticky agent mapped to non-Primary proxy"
            );
        }
    }

    #[test]
    fn test_affected_agent_only_remap() {
        // Given 3 primaries (40001-40003) + 2 warm-standby (40004-40005)
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let mut pool = ProxyPool::new(&urls);

        // Agent_a → assigned primary, agent_b → assigned primary, agent_c → assigned primary
        let a_idx = pool.select_proxy_for_key("agent_a").unwrap().2;
        let b_idx = pool.select_proxy_for_key("agent_b").unwrap().2;
        let c_idx = pool.select_proxy_for_key("agent_c").unwrap().2;

        // Verify all are primary
        assert_eq!(pool.proxies[a_idx].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[b_idx].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[c_idx].role, ProxyRole::Primary);

        // Save the assigned primaries (their index in pool)
        let a_primary = a_idx;
        let b_primary = b_idx;
        let c_primary = c_idx;

        // When agent_b's primary (b_primary) is marked cooldown/dead
        pool.mark_rate_limited(b_primary, Duration::from_secs(300));

        // Then agent_b fails over to WarmStandby
        let b_failover = pool.select_proxy_for_key("agent_b").unwrap().2;
        assert_eq!(
            pool.proxies[b_failover].role,
            ProxyRole::WarmStandby,
            "agent_b should failover to WarmStandby, got index {} role {:?}",
            b_failover,
            pool.proxies[b_failover].role
        );

        // And agent_a still maps to its original primary
        let a_after = pool.select_proxy_for_key("agent_a").unwrap().2;
        assert_eq!(
            a_after, a_primary,
            "agent_a remapped from primary {} to {}, expected no change",
            a_primary, a_after
        );

        // And agent_c still maps to its original primary
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

        // Get agent's assigned primary
        let primary = pool.select_proxy_for_key("failover-agent").unwrap().2;
        assert_eq!(pool.proxies[primary].role, ProxyRole::Primary);

        // Mark the selected primary unhealthy
        pool.mark_rate_limited(primary, Duration::from_secs(300));

        // When selected primary unhealthy and warm standby healthy,
        // request routes to 40004 or 40005
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

        // Get agent's assigned primary
        let primary_idx = pool.select_proxy_for_key("recovery-agent").unwrap().2;
        assert_eq!(pool.proxies[primary_idx].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[primary_idx].status, ProxyStatus::Active);

        // Mark the primary as rate-limited (simulate failure)
        pool.mark_rate_limited(primary_idx, Duration::from_secs(0)); // 0s = already expired

        // After cooldown has expired (0s), the proxy should be healthy again
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

        // Even if standby exists and healthy, selected primary healthy → use primary
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

        // Rendezvous score for the same key+node should be deterministic
        let score1 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40001");
        let score2 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40001");
        assert_eq!(score1, score2, "rendezvous score must be deterministic");

        // Different nodes should have different scores
        let score3 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40002");
        assert_ne!(
            score1, score3,
            "different nodes should have different scores"
        );
    }

    #[test]
    fn test_warm_standby_excluded_from_normal_routing() {
        // Build 5 proxies: 40001-40003 primary, 40004-40005 warm-standby
        let urls: Vec<String> = (0..5)
            .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
            .collect();
        let mut pool = ProxyPool::new(&urls);

        assert_eq!(pool.proxies.len(), 5);
        // Verify roles assigned correctly
        assert_eq!(pool.proxies[0].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[1].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[2].role, ProxyRole::Primary);
        assert_eq!(pool.proxies[3].role, ProxyRole::WarmStandby);
        assert_eq!(pool.proxies[4].role, ProxyRole::WarmStandby);

        // Normal traffic should NEVER select a WarmStandby proxy when
        // any Primary proxy is available. Run many keys to be sure.
        for key in &["alpha", "beta", "gamma", "delta", "epsilon"] {
            let (_, url, idx) = pool.get_client(key).unwrap();
            assert!(
                pool.proxies[idx].role == ProxyRole::Primary,
                "get_client('{}') returned WarmStandby {} (idx {}), expected Primary",
                key,
                url,
                idx
            );
        }

        // Mark all three primaries as rate-limited
        for i in 0..3 {
            pool.mark_rate_limited(i, Duration::from_secs(300));
        }

        // Now all primaries are on cooldown → spare swap should trigger.
        // The spare at index 3 or 4 is WarmStandby → it should be used as failover.
        let (_, url, idx) = pool.get_client("failover-test").unwrap();
        assert!(
            pool.proxies[idx].role == ProxyRole::WarmStandby,
            "expected WarmStandby in failover, got role={:?} at idx={} url={}",
            pool.proxies[idx].role,
            idx,
            url
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

        // Manually push to queue
        pool.restart_queue.push(0);
        pool.restart_queue.push(1);
        assert_eq!(pool.drain_restart_queue().len(), 2);
        assert!(pool.drain_restart_queue().is_empty()); // second drain should be empty
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

    // ── Phase 6: Telemetry & Health tests ──

    #[test]
    fn test_record_failure_enters_cooldown() {
        let urls = make_test_urls(1);
        let mut pool = ProxyPool::new(&urls);

        // Initial state: healthy
        assert_eq!(pool.proxies[0].consecutive_failures, 0);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));

        // First failure should not trigger cooldown (threshold is 2)
        pool.record_failure(0);
        assert_eq!(pool.proxies[0].consecutive_failures, 1);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));

        // Second failure triggers cooldown
        pool.record_failure(0);
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));
    }

    #[test]
    fn test_http_400_does_not_mark_proxy_failed() {
        // record_success is called for any HTTP response, including 400.
        // It resets failure count — confirming HTTP 400 does NOT mark proxy failed.
        let urls = make_test_urls(1);
        let mut pool = ProxyPool::new(&urls);

        // Set up a failure first so we can verify success resets it
        pool.record_failure(0);
        assert_eq!(pool.proxies[0].consecutive_failures, 1);

        // Simulate HTTP 400 response: record_success (transport worked)
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

        // policy
        assert_eq!(stats.policy, "primary-with-warm-standby");

        // Primary tier
        assert_eq!(stats.primary.ports, vec![40001, 40002, 40003]);
        assert_eq!(stats.primary.total, 3);
        assert_eq!(stats.primary.healthy, 3);
        assert!(!stats.primary.protected);

        // WarmStandby tier
        assert_eq!(stats.warm_standby.ports, vec![40004, 40005]);
        assert_eq!(stats.warm_standby.total, 2);
        assert_eq!(stats.warm_standby.healthy, 2);
        assert!(stats.warm_standby.protected);

        // Nodes
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

        // Mark two primaries and one warm-standby as rate-limited
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

        // Force proxy into cooldown via failure threshold
        pool.record_failure(0); // failure 1
        pool.record_failure(0); // failure 2 → enters cooldown
        assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));
        assert_eq!(pool.proxies[0].consecutive_successes, 0);

        // RECOVERY_SUCCESS_COUNT = 2. After 2 successes it should recover.
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
