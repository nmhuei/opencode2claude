//! Proxy health tracking, cooldown/recovery, and Docker container lifecycle.
//!
//! Tracks consecutive successes/failures per proxy, manages adaptive cooldown,
//! auto-recovery thresholds, and Docker container restart/monitor tasks.

use super::types::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{error, info, warn};

impl ProxyPool {
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
            "Proxy #{} restart queued (attempt {}/{}): {}",
            idx, attempts, 3, reason
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
