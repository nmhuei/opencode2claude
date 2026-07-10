//! Proxy-pool construction, snapshots, and degraded selection.

use super::types::*;
use reqwest::Client;
use std::time::{Duration, Instant};
use tracing::{info, warn};

impl ProxyPool {
    pub fn new(proxy_urls: &[String]) -> Self {
        let mut proxies: Vec<ProxyEntry> = proxy_urls.iter().filter_map(build_entry).collect();
        let active_count = configured_active_count(proxies.len());

        for proxy in proxies.iter_mut().skip(active_count) {
            if proxy.role != ProxyRole::WarmStandby {
                proxy.status = ProxyStatus::Spare;
            }
        }

        info!(
            total = proxies.len(),
            active = active_count,
            spare = proxies.len().saturating_sub(active_count),
            "proxy pool initialized"
        );

        Self {
            proxies,
            active_count,
            restart_queue: Vec::new(),
        }
    }

    pub fn drain_restart_queue(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.restart_queue)
    }

    pub fn snapshot(&self) -> ProxyPoolStats {
        let mut primary = TierAccumulator::new(false);
        let mut standby = TierAccumulator::new(true);
        let mut nodes = Vec::with_capacity(self.proxies.len());

        for proxy in &self.proxies {
            nodes.push(node_stats(proxy));
            match proxy.role {
                ProxyRole::Primary => primary.record(proxy),
                ProxyRole::WarmStandby => standby.record(proxy),
            }
        }

        ProxyPoolStats {
            policy: "primary-with-warm-standby".to_string(),
            primary: primary.finish(),
            warm_standby: standby.finish(),
            nodes,
        }
    }
}

fn build_entry(url: &String) -> Option<ProxyEntry> {
    let proxy = match reqwest::Proxy::all(url) {
        Ok(proxy) => proxy,
        Err(error) => {
            warn!(%url, %error, "invalid proxy URL");
            return None;
        }
    };
    let client = match Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(600))
        .pool_max_idle_per_host(10)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!(%url, %error, "failed to build proxy HTTP client");
            return None;
        }
    };

    let port = extract_port(url);
    Some(ProxyEntry {
        url: url.clone(),
        client,
        status: ProxyStatus::Active,
        port,
        container_name: container_name(url),
        role: if is_protected_proxy_port(port) {
            ProxyRole::WarmStandby
        } else {
            ProxyRole::Primary
        },
        lifecycle: if is_managed_proxy_port(port) {
            ProxyLifecycle::Managed
        } else {
            ProxyLifecycle::Protected
        },
        consecutive_failures: 0,
        consecutive_successes: 0,
    })
}

fn configured_active_count(total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    std::env::var("BRIDGE_ACTIVE_PROXY_COUNT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| total.saturating_sub(1))
        .clamp(1, total)
}

fn node_stats(proxy: &ProxyEntry) -> ProxyNodeStats {
    ProxyNodeStats {
        port: proxy.port,
        role: proxy.role,
        lifecycle: proxy.lifecycle,
        status: proxy.status.description().to_string(),
        failure_count: proxy.consecutive_failures,
        success_count: proxy.consecutive_successes,
        cooldown_remaining_secs: match proxy.status {
            ProxyStatus::Cooldown(until) => Some(
                until
                    .checked_duration_since(Instant::now())
                    .unwrap_or_default()
                    .as_secs(),
            ),
            _ => None,
        },
    }
}

#[derive(Debug)]
struct TierAccumulator {
    ports: Vec<u16>,
    healthy: usize,
    degraded: usize,
    cooldown: usize,
    recovering: usize,
    dead: usize,
    protected: bool,
}

impl TierAccumulator {
    fn new(protected: bool) -> Self {
        Self {
            ports: Vec::new(),
            healthy: 0,
            degraded: 0,
            cooldown: 0,
            recovering: 0,
            dead: 0,
            protected,
        }
    }

    fn record(&mut self, proxy: &ProxyEntry) {
        self.ports.push(proxy.port);
        match proxy.status {
            ProxyStatus::Active => self.healthy += 1,
            ProxyStatus::Spare => self.degraded += 1,
            ProxyStatus::Cooldown(_) => self.cooldown += 1,
            ProxyStatus::Starting => self.recovering += 1,
            ProxyStatus::Dead { .. } => self.dead += 1,
        }
    }

    fn finish(self) -> ProxyTierStats {
        ProxyTierStats {
            total: self.ports.len(),
            ports: self.ports,
            healthy: self.healthy,
            degraded: self.degraded,
            cooldown: self.cooldown,
            recovering: self.recovering,
            dead: self.dead,
            protected: self.protected,
        }
    }
}
