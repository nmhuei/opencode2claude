use reqwest::Client;
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProxyRole {
    /// Primary managed proxy (40001-40003) — CLI may restart/stop/recover
    Primary,
    /// Warm-standby protected proxy (40004-40005) — CLI may only health-check
    WarmStandby,
}

/// Proxy lifecycle management policy.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    #[allow(dead_code)]
    pub role: ProxyRole,
    /// Proxy lifecycle management policy (Managed/Protected).
    #[allow(dead_code)]
    pub lifecycle: ProxyLifecycle,
}

/// Proxy pool với hot-spare model.
///
/// - indices `[0..active_count)` là active slots (có thể Active, Cooldown, Dead, Starting)
/// - indices `[active_count..]` là spare slots (thường là Spare)
/// - Khi 1 active chết → swap status với 1 spare, push dead index vào restart_queue
#[derive(Debug, Default)]
pub struct ProxyPool {
    pub proxies: Vec<ProxyEntry>,
    pub active_count: usize,
    pub restart_queue: Vec<usize>,
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

        // Set indices >= active_count as Spare
        for proxy in proxies.iter_mut().take(total).skip(active_count) {
            proxy.status = ProxyStatus::Spare;
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

    fn is_on_cooldown(status: &ProxyStatus) -> bool {
        match status {
            ProxyStatus::Cooldown(until) => Instant::now() < *until,
            _ => false,
        }
    }

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

    fn clear_expired_cooldowns(proxies: &mut [ProxyEntry]) {
        let now = Instant::now();
        for p in proxies.iter_mut() {
            if let ProxyStatus::Cooldown(until) = p.status {
                if now >= until {
                    p.status = ProxyStatus::Active;
                }
            }
        }
    }

    // ── Main API ──

    /// Get the best available client for a given API key.
    ///
    /// 1. Clear expired cooldowns
    /// 2. Hash api_key → preferred index within active Primary set
    /// 3. Linear probe for non-cooldown active Primary
    /// 4. If all active Primaries are on cooldown, try spare swap
    /// 5. Degraded: pick closest to cooldown-end; fallback to default client if all dead
    ///
    /// IMPORTANT: WarmStandby proxies are NEVER selected for normal traffic.
    /// They are only reached via failover (all primaries dead/cooldown → spare swap)
    /// or degraded mode (everything unavailable).
    pub fn get_client(&mut self, api_key: &str) -> Option<(Client, String, usize)> {
        if self.proxies.is_empty() {
            return None;
        }

        Self::clear_expired_cooldowns(&mut self.proxies);

        let mut hasher = DefaultHasher::new();
        api_key.hash(&mut hasher);
        let hash_val = hasher.finish() as usize;

        // Build active indices (index < active_count, status usable, Primary role)
        let mut active_indices: Vec<usize> = (0..self.active_count)
            .filter(|&i| {
                Self::is_usable(&self.proxies[i].status)
                    && self.proxies[i].role == ProxyRole::Primary
            })
            .collect();

        // If no actives available, try swapping in a spare
        if active_indices.is_empty() {
            let spare_idx = (self.active_count..self.proxies.len())
                .find(|&i| matches!(self.proxies[i].status, ProxyStatus::Spare));

            if let Some(spare) = spare_idx {
                let dead_idx =
                    (0..self.active_count).find(|&i| !Self::is_usable(&self.proxies[i].status));

                if let Some(dead) = dead_idx {
                    self.proxies[spare].status = ProxyStatus::Active;
                    self.proxies[dead].status = ProxyStatus::Dead {
                        restart_attempts: 0,
                    };
                    self.restart_queue.push(dead);
                    active_indices = vec![spare]; // spare now lives at its original index but is Active
                    info!(
                        "Spare proxy #{} ({}) swapped into active slot #{}",
                        spare, self.proxies[spare].url, dead
                    );
                }
            }
        }

        // Still empty → try degraded (pick any usable proxy)
        if active_indices.is_empty() {
            let degraded = self.select_degraded();
            if let Some(idx) = degraded {
                warn!(
                    "CRITICAL: All proxies unavailable. Degraded mode, using proxy #{} ({})",
                    idx, self.proxies[idx].url
                );
                return Some((
                    self.proxies[idx].client.clone(),
                    self.proxies[idx].url.clone(),
                    idx,
                ));
            }
            return None;
        }

        // Hash to preferred index within active set
        let prefer_idx = hash_val % active_indices.len();

        // Linear probe for first non-cooldown proxy
        for i in 0..active_indices.len() {
            let idx = active_indices[(prefer_idx + i) % active_indices.len()];
            if !Self::is_on_cooldown(&self.proxies[idx].status) {
                return Some((
                    self.proxies[idx].client.clone(),
                    self.proxies[idx].url.clone(),
                    idx,
                ));
            }
        }

        // All active are on cooldown → try spare swap
        let spare_idx = (self.active_count..self.proxies.len()).find(|&i| {
            !Self::is_on_cooldown(&self.proxies[i].status)
                && matches!(self.proxies[i].status, ProxyStatus::Spare)
        });

        if let Some(spare) = spare_idx {
            self.proxies[spare].status = ProxyStatus::Active;
            return Some((
                self.proxies[spare].client.clone(),
                self.proxies[spare].url.clone(),
                spare,
            ));
        }

        // Everything on cooldown → picked preferred active anyway (degraded)
        let preferred = active_indices[prefer_idx];
        warn!(
            "All proxies in pool are currently rate-limited. Falling back to proxy #{} ({}).",
            preferred, self.proxies[preferred].url
        );
        Some((
            self.proxies[preferred].client.clone(),
            self.proxies[preferred].url.clone(),
            preferred,
        ))
    }

    /// Get a client excluding a specific index (for retry failover).
    pub fn get_client_excluding(
        &mut self,
        api_key: &str,
        exclude_idx: usize,
    ) -> Option<(Client, String, usize)> {
        if self.proxies.is_empty() {
            return None;
        }

        Self::clear_expired_cooldowns(&mut self.proxies);

        let mut hasher = DefaultHasher::new();
        api_key.hash(&mut hasher);
        let hash_val = hasher.finish() as usize;

        let active_indices: Vec<usize> = (0..self.active_count)
            .filter(|&i| {
                Self::is_usable(&self.proxies[i].status)
                    && i != exclude_idx
                    && self.proxies[i].role == ProxyRole::Primary
            })
            .collect();

        if active_indices.is_empty() {
            // Try spare excluding exclude_idx
            let spare = (self.active_count..self.proxies.len()).find(|&i| {
                matches!(self.proxies[i].status, ProxyStatus::Spare) && i != exclude_idx
            });
            if let Some(spare) = spare {
                return Some((
                    self.proxies[spare].client.clone(),
                    self.proxies[spare].url.clone(),
                    spare,
                ));
            }
            // Degraded: any usable excluding exclude_idx
            for i in 0..self.proxies.len() {
                if i != exclude_idx && Self::is_usable(&self.proxies[i].status) {
                    return Some((
                        self.proxies[i].client.clone(),
                        self.proxies[i].url.clone(),
                        i,
                    ));
                }
            }
            return None;
        }

        let prefer_idx = hash_val % active_indices.len();
        for i in 0..active_indices.len() {
            let idx = active_indices[(prefer_idx + i) % active_indices.len()];
            if !Self::is_on_cooldown(&self.proxies[idx].status) {
                return Some((
                    self.proxies[idx].client.clone(),
                    self.proxies[idx].url.clone(),
                    idx,
                ));
            }
        }

        let preferred = active_indices[prefer_idx];
        Some((
            self.proxies[preferred].client.clone(),
            self.proxies[preferred].url.clone(),
            preferred,
        ))
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
    let client = match reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::all(format!("socks5h://127.0.0.1:{}", port))
                .expect("Invalid proxy URL"),
        )
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
    fn test_proxy_pool_failover() {
        let urls = make_test_urls(4);
        let mut pool = ProxyPool::new(&urls);
        // 4 proxies → 3 active + 1 spare

        // Get preferred proxy for "agent-test"
        let preferred = pool.get_client("agent-test").unwrap().2;
        assert!(
            preferred < pool.active_count,
            "preferred should be in active set"
        );

        // Mark preferred proxy as rate-limited
        pool.mark_rate_limited(preferred, Duration::from_secs(60));

        // Should failover to a different active index
        let after_failover = pool.get_client("agent-test").unwrap();
        assert_ne!(after_failover.2, preferred);

        // Mark all actives as rate-limited
        for idx in 0..pool.active_count {
            pool.mark_rate_limited(idx, Duration::from_secs(60));
        }

        // Should swap in the spare
        let with_spare = pool.get_client("agent-test").unwrap();
        assert!(
            with_spare.2 >= pool.active_count,
            "should use spare when all actives are on cooldown"
        );
    }

    #[test]
    fn test_get_client_excluding() {
        let urls = make_test_urls(3);
        let mut pool = ProxyPool::new(&urls);

        let preferred = pool.get_client("agent-excl").unwrap().2;

        // Excluding the preferred proxy should return a different one
        let result = pool.get_client_excluding("agent-excl", preferred).unwrap();
        assert_ne!(result.2, preferred);

        // Excluding a non-preferred proxy should still return the preferred one
        let other_idx = (preferred + 1) % pool.active_count;
        let result2 = pool.get_client_excluding("agent-excl", other_idx).unwrap();
        assert_eq!(result2.2, preferred);
    }

    #[test]
    fn test_spare_swap_on_full_cooldown() {
        let urls = make_test_urls(3);
        let mut pool = ProxyPool::new(&urls);
        // 3 proxies → 2 active + 1 spare

        // Mark ALL actives as rate-limited
        for idx in 0..pool.active_count {
            pool.mark_rate_limited(idx, Duration::from_secs(60));
        }

        // When all actives are on cooldown, get_client swaps in the spare
        let result = pool.get_client("test-key").unwrap();
        assert_eq!(result.2, 2, "should return spare at index 2");

        // No restart queue entry because cooldown proxies recover naturally
        assert_eq!(
            pool.restart_queue.len(),
            0,
            "cooldown should not trigger restart"
        );
    }

    #[test]
    fn test_degraded_mode_picks_closest_to_cooldown_end() {
        let urls = make_test_urls(2);
        let mut pool = ProxyPool::new(&urls);
        // 2 proxies → 1 active + 1 spare

        // Mark active as rate-limited
        pool.mark_rate_limited(0, Duration::from_secs(120));

        // Active is on cooldown → spare should be swapped in
        let result = pool.get_client("test").unwrap();
        assert_eq!(result.2, 1, "should use spare when active is on cooldown");
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
}
