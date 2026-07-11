//! Egress-pool construction and health snapshots.

use super::types::*;
use reqwest::Client;
use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

impl ProxyPool {
    pub fn new(proxy_urls: &[String]) -> Self {
        let primary_count = proxy_urls
            .iter()
            .filter(|url| !is_protected_proxy_port(extract_port(url)))
            .count();
        Self::new_with_active_count(proxy_urls, primary_count)
    }

    pub fn new_with_active_count(proxy_urls: &[String], requested_active_count: usize) -> Self {
        let mut proxies: Vec<ProxyEntry> = proxy_urls.iter().filter_map(build_entry).collect();
        let primary_count = proxies
            .iter()
            .filter(|proxy| proxy.role == EgressRole::Primary)
            .count();
        let active_count = requested_active_count.min(primary_count);

        let mut primary_ordinal = 0usize;
        for proxy in &mut proxies {
            if proxy.role == EgressRole::Primary {
                proxy.routing_enabled = primary_ordinal < active_count;
                primary_ordinal += 1;
            }
        }

        info!(
            total = proxies.len(),
            primary = primary_count,
            active_primary = active_count,
            standby = proxies.len().saturating_sub(primary_count),
            "egress pool initialized"
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

    pub fn begin_lease(&self, index: usize) -> Option<EgressLease> {
        self.proxies
            .get(index)
            .map(|node| node.acquire_lease(index))
    }

    pub fn can_modify_node(&self, index: usize) -> Result<(), String> {
        let node = self
            .proxies
            .get(index)
            .ok_or_else(|| format!("unknown proxy index {index}"))?;
        if node.lifecycle == LifecyclePolicy::Protected {
            return Err(format!("node {} is protected", node.id));
        }
        let active_requests = node.active_request_count();
        if active_requests > 0 {
            return Err(format!(
                "node {} has {} active request lease(s)",
                node.id, active_requests
            ));
        }
        Ok(())
    }

    pub fn snapshot(&self) -> ProxyPoolStats {
        let mut primary = TierAccumulator::new(false);
        let mut standby = TierAccumulator::new(true);
        let mut nodes = Vec::with_capacity(self.proxies.len());

        for proxy in &self.proxies {
            nodes.push(node_stats(proxy));
            match proxy.role {
                EgressRole::Primary => primary.record(proxy),
                EgressRole::WarmStandby => standby.record(proxy),
            }
        }

        ProxyPoolStats {
            policy: "primary-with-protected-warm-standby".to_string(),
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
    let name = container_name(url);
    let role = if is_protected_proxy_port(port) {
        EgressRole::WarmStandby
    } else {
        EgressRole::Primary
    };
    Some(ProxyEntry {
        id: name.clone(),
        url: url.clone(),
        client,
        port,
        container_name: name,
        role,
        lifecycle: if is_managed_proxy_port(port) {
            LifecyclePolicy::Managed
        } else {
            LifecyclePolicy::Protected
        },
        routing_enabled: role == EgressRole::Primary,
        health: HealthState::Healthy,
        circuit: CircuitState::Closed,
        consecutive_failures: 0,
        consecutive_successes: 0,
        restart_attempts: 0,
        cooldown_until: None,
        exit_identity: None,
        duplicate_of: None,
        active_requests: Arc::new(AtomicUsize::new(0)),
    })
}

fn node_stats(proxy: &ProxyEntry) -> ProxyNodeStats {
    let now = Instant::now();
    ProxyNodeStats {
        id: proxy.id.clone(),
        port: proxy.port,
        role: proxy.role,
        lifecycle: proxy.lifecycle,
        routing_enabled: proxy.routing_enabled,
        health: proxy.health,
        circuit: proxy.circuit.label().to_string(),
        status: compatibility_status(proxy).to_string(),
        failure_count: proxy.consecutive_failures,
        success_count: proxy.consecutive_successes,
        restart_attempts: proxy.restart_attempts,
        cooldown_remaining_secs: proxy.cooldown_until.and_then(|until| {
            until
                .checked_duration_since(now)
                .map(|duration| duration.as_secs())
        }),
        active_requests: proxy.active_request_count(),
        exit_identity: proxy.exit_identity.clone(),
        duplicate_of: proxy.duplicate_of.clone(),
    }
}

fn compatibility_status(proxy: &ProxyEntry) -> &'static str {
    if proxy.duplicate_of.is_some() {
        "duplicate_exit"
    } else if proxy.circuit.is_open(Instant::now()) {
        "cooldown"
    } else if proxy.health == HealthState::Recovering {
        "starting"
    } else if proxy.health == HealthState::Unhealthy {
        "dead"
    } else if proxy.role == EgressRole::Primary && !proxy.routing_enabled {
        "spare"
    } else {
        proxy.health_label()
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
    verified_exits: HashSet<String>,
    active_requests: usize,
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
            verified_exits: HashSet::new(),
            active_requests: 0,
        }
    }

    fn record(&mut self, proxy: &ProxyEntry) {
        self.ports.push(proxy.port);
        self.active_requests = self
            .active_requests
            .saturating_add(proxy.active_request_count());
        if proxy.duplicate_of.is_none() {
            if let Some(identity) = &proxy.exit_identity {
                self.verified_exits.insert(identity.public_ip.clone());
            }
        }

        if proxy.circuit.is_open(Instant::now()) {
            self.cooldown += 1;
            return;
        }
        match proxy.health {
            HealthState::Healthy => self.healthy += 1,
            HealthState::Unknown | HealthState::Degraded => self.degraded += 1,
            HealthState::Recovering => self.recovering += 1,
            HealthState::Unhealthy => self.dead += 1,
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
            unique_verified_exits: self.verified_exits.len(),
            active_requests: self.active_requests,
        }
    }
}
