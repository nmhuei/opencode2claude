//! Exit-identity probing, normalization, and duplicate suppression.

use super::types::*;
use crate::workers::WorkerContext;
use futures_util::{future::join_all, StreamExt};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::warn;

const MAX_IDENTITY_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeObservation {
    ip: IpAddr,
    provider: Option<String>,
    colo: Option<String>,
    warp: Option<bool>,
}

pub async fn identity_monitor(
    pool: Arc<RwLock<ProxyPool>>,
    endpoints: Vec<String>,
    interval: Duration,
    context: WorkerContext,
) -> Result<(), String> {
    let mut ticker = tokio::time::interval(interval.max(Duration::from_secs(5)));
    loop {
        tokio::select! {
            _ = context.cancellation().cancelled() => return Ok(()),
            _ = ticker.tick() => {
                context.heartbeat();
                refresh_exit_identities(pool.clone(), &endpoints).await;
            }
        }
    }
}

pub async fn refresh_exit_identities(pool: Arc<RwLock<ProxyPool>>, endpoints: &[String]) {
    let probes = {
        let pool = pool.read().await;
        pool.proxies
            .iter()
            .enumerate()
            .map(|(index, node)| (index, node.id.clone(), node.client.clone()))
            .collect::<Vec<_>>()
    };

    let results = join_all(
        probes
            .into_iter()
            .map(|(index, node_id, client)| async move {
                let result = probe_exit_identity(&client, endpoints).await;
                (index, node_id, result)
            }),
    )
    .await;

    let mut pool = pool.write().await;
    pool.apply_identity_results(results);
}

pub async fn probe_exit_identity(
    client: &reqwest::Client,
    endpoints: &[String],
) -> Result<ExitIdentity, String> {
    if endpoints.is_empty() {
        return Err("no identity endpoints configured".to_string());
    }

    let observations = join_all(endpoints.iter().map(|endpoint| async move {
        let response = client
            .get(endpoint)
            .send()
            .await
            .map_err(|error| format!("identity request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "identity endpoint returned HTTP {}",
                response.status()
            ));
        }
        let body = read_limited(response).await?;
        parse_observation(&body).ok_or_else(|| "identity response was not recognized".to_string())
    }))
    .await;

    let valid = observations
        .into_iter()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    if valid.is_empty() {
        return Err("all identity probes failed".to_string());
    }

    let mut counts: HashMap<IpAddr, usize> = HashMap::new();
    for observation in &valid {
        *counts.entry(observation.ip).or_default() += 1;
    }
    let (winner, count) = counts
        .into_iter()
        .max_by(|(left_ip, left_count), (right_ip, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_ip.to_string().cmp(&left_ip.to_string()))
        })
        .ok_or_else(|| "identity consensus unavailable".to_string())?;

    let required = endpoints.len().min(2);
    if count < required {
        return Err(format!(
            "identity probes did not reach consensus: {count}/{required}"
        ));
    }

    let matching = valid
        .iter()
        .filter(|observation| observation.ip == winner)
        .collect::<Vec<_>>();
    let warp_signals_present = matching
        .iter()
        .any(|observation| observation.warp.is_some());
    if warp_signals_present
        && !matching
            .iter()
            .any(|observation| observation.warp == Some(true))
    {
        return Err("Cloudflare trace did not report warp=on or warp=plus".to_string());
    }

    Ok(ExitIdentity {
        public_ip: winner.to_string(),
        provider: matching
            .iter()
            .find_map(|observation| observation.provider.clone()),
        colo: matching
            .iter()
            .find_map(|observation| observation.colo.clone()),
        verified_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
}

async fn read_limited(response: reqwest::Response) -> Result<String, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("identity body read failed: {error}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_IDENTITY_BODY_BYTES {
            return Err("identity response exceeded size limit".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "identity response was not UTF-8".to_string())
}

fn parse_observation(body: &str) -> Option<ProbeObservation> {
    let trimmed = body.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["ip", "query", "origin"] {
            if let Some(raw) = value.get(key).and_then(serde_json::Value::as_str) {
                if let Some(ip) = first_public_ip(raw) {
                    return Some(ProbeObservation {
                        ip,
                        provider: value
                            .get("provider")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                        colo: value
                            .get("colo")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                        warp: None,
                    });
                }
            }
        }
    }

    let fields = trimmed
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect::<HashMap<_, _>>();
    if let Some(ip) = fields.get("ip").and_then(|value| first_public_ip(value)) {
        let warp = fields
            .get("warp")
            .map(|value| value.eq_ignore_ascii_case("on") || value.eq_ignore_ascii_case("plus"));
        return Some(ProbeObservation {
            ip,
            provider: warp
                .filter(|enabled| *enabled)
                .map(|_| "cloudflare-warp".to_string()),
            colo: fields.get("colo").map(|value| (*value).to_string()),
            warp,
        });
    }

    first_public_ip(trimmed).map(|ip| ProbeObservation {
        ip,
        provider: None,
        colo: None,
        warp: None,
    })
}

fn first_public_ip(value: &str) -> Option<IpAddr> {
    value
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter_map(|candidate| candidate.trim().parse::<IpAddr>().ok())
        .find(|ip| is_public_ip(*ip))
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 240)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

impl ProxyPool {
    fn apply_identity_results(
        &mut self,
        results: Vec<(usize, String, Result<ExitIdentity, String>)>,
    ) {
        for node in &mut self.proxies {
            node.duplicate_of = None;
        }

        for (index, node_id, result) in results {
            let Some(node) = self.proxies.get_mut(index) else {
                continue;
            };
            if node.id != node_id {
                continue;
            }
            match result {
                Ok(identity) => {
                    node.exit_identity = Some(identity);
                    if matches!(node.health, HealthState::Unknown | HealthState::Recovering)
                        && !node.circuit.is_open(std::time::Instant::now())
                    {
                        node.health = HealthState::Healthy;
                        node.circuit = CircuitState::Closed;
                    }
                }
                Err(error) => {
                    node.exit_identity = None;
                    if self.require_verified_exit_ip && node.health != HealthState::Unhealthy {
                        node.health = HealthState::Unknown;
                    }
                    warn!(node_id = %node.id, %error, "exit identity verification failed");
                }
            }
        }

        self.suppress_duplicate_exits();
    }

    pub fn suppress_duplicate_exits(&mut self) {
        for node in &mut self.proxies {
            node.duplicate_of = None;
        }
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, node) in self.proxies.iter().enumerate() {
            if let Some(identity) = &node.exit_identity {
                groups
                    .entry(identity.public_ip.clone())
                    .or_default()
                    .push(index);
            }
        }

        for indices in groups.values_mut() {
            indices.sort_by_key(|index| {
                let node = &self.proxies[*index];
                (
                    match node.role {
                        EgressRole::Primary => 0_u8,
                        EgressRole::WarmStandby => 1_u8,
                    },
                    !node.routing_enabled,
                    node.id.clone(),
                )
            });
            if let Some((&owner, duplicates)) = indices.split_first() {
                let owner_id = self.proxies[owner].id.clone();
                for duplicate in duplicates {
                    self.proxies[*duplicate].duplicate_of = Some(owner_id.clone());
                }
            }
        }
    }

    pub fn verified_unique_exit_count(&self) -> usize {
        self.proxies
            .iter()
            .filter(|node| node.exit_identity.is_some() && node.duplicate_of.is_none())
            .filter_map(|node| {
                node.exit_identity
                    .as_ref()
                    .map(|identity| &identity.public_ip)
            })
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    pub fn verified_unique_exit_count_fresh(&self, ttl: Duration) -> usize {
        self.proxies
            .iter()
            .filter(|node| node.duplicate_of.is_none())
            .filter_map(|node| node.exit_identity.as_ref())
            .filter(|identity| identity.is_fresh(ttl))
            .map(|identity| &identity.public_ip)
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    pub fn egress_ready(&self, minimum_unique_exit_ips: usize, identity_ttl: Duration) -> bool {
        let routable = self.proxies.iter().any(|node| {
            node.routing_enabled
                && node.health == HealthState::Healthy
                && node.circuit == CircuitState::Closed
                && node.duplicate_of.is_none()
                && (!self.require_verified_exit_ip
                    || node
                        .exit_identity
                        .as_ref()
                        .is_some_and(|identity| identity.is_fresh(identity_ttl)))
        });
        routable
            && (!self.require_verified_exit_ip
                || self.verified_unique_exit_count_fresh(identity_ttl) >= minimum_unique_exit_ips)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(ip: &str) -> ExitIdentity {
        ExitIdentity {
            public_ip: ip.to_string(),
            provider: Some("cloudflare-warp".to_string()),
            colo: Some("SIN".to_string()),
            verified_at_unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    #[test]
    fn parses_cloudflare_trace_json_and_plain_responses() {
        let trace = parse_observation("ip=1.1.1.1\ncolo=SIN\nwarp=on\n").expect("trace");
        assert_eq!(trace.ip, "1.1.1.1".parse::<IpAddr>().unwrap());
        assert_eq!(trace.warp, Some(true));
        assert_eq!(trace.colo.as_deref(), Some("SIN"));

        let json = parse_observation(r#"{"ip":"1.1.1.1"}"#).expect("json");
        assert_eq!(json.ip, trace.ip);
        assert_eq!(parse_observation("1.1.1.1").expect("plain").ip, trace.ip);
    }

    #[test]
    fn rejects_private_reserved_and_documentation_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "203.0.113.1",
            "::1",
            "2001:db8::1",
        ] {
            assert!(parse_observation(value).is_none(), "accepted {value}");
        }
    }

    #[test]
    fn duplicate_owner_is_deterministic_and_primary_first() {
        let mut pool = ProxyPool::new(&[
            "socks5://127.0.0.1:40001".to_string(),
            "socks5://127.0.0.1:40002".to_string(),
            "socks5://127.0.0.1:40004".to_string(),
        ]);
        for node in &mut pool.proxies {
            node.exit_identity = Some(identity("1.1.1.1"));
        }
        pool.suppress_duplicate_exits();
        assert_eq!(pool.proxies[0].duplicate_of, None);
        assert_eq!(
            pool.proxies[1].duplicate_of.as_deref(),
            Some("opencode-warp-1")
        );
        assert_eq!(
            pool.proxies[2].duplicate_of.as_deref(),
            Some("opencode-warp-1")
        );
        assert_eq!(pool.verified_unique_exit_count(), 1);
    }

    #[test]
    fn strict_pool_is_not_ready_until_unique_identity_requirement_is_met() {
        let mut pool = ProxyPool::new_with_policy(
            &[
                "socks5://127.0.0.1:40001".to_string(),
                "socks5://127.0.0.1:40002".to_string(),
            ],
            2,
            true,
        );
        assert!(!pool.egress_ready(2, Duration::from_secs(300)));
        pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
        pool.proxies[1].exit_identity = Some(identity("8.8.8.8"));
        pool.proxies[0].health = HealthState::Healthy;
        pool.proxies[1].health = HealthState::Healthy;
        pool.suppress_duplicate_exits();
        assert!(pool.egress_ready(2, Duration::from_secs(300)));
    }

    #[tokio::test]
    async fn probe_requires_consensus_and_warp_signal() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        async fn server(body: &'static str) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            format!("http://{address}")
        }

        let endpoints = vec![
            server("ip=1.1.1.1\ncolo=SIN\nwarp=on\n").await,
            server(r#"{"ip":"1.1.1.1"}"#).await,
        ];
        let identity = probe_exit_identity(&reqwest::Client::new(), &endpoints)
            .await
            .expect("consensus");
        assert_eq!(identity.public_ip, "1.1.1.1");
        assert_eq!(identity.provider.as_deref(), Some("cloudflare-warp"));
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    #[test]
    fn stale_identity_is_not_ready_or_routable_in_strict_mode() {
        let mut pool = ProxyPool::new_with_egress_policy(
            &["socks5h://127.0.0.1:40001".to_string()],
            1,
            true,
            Duration::from_secs(30),
        );
        pool.proxies[0].exit_identity = Some(ExitIdentity {
            public_ip: "1.1.1.1".to_string(),
            provider: Some("cloudflare-warp".to_string()),
            colo: Some("SIN".to_string()),
            verified_at_unix_secs: 1,
        });
        pool.proxies[0].health = HealthState::Healthy;
        pool.suppress_duplicate_exits();

        assert_eq!(pool.verified_unique_exit_count(), 1);
        assert_eq!(
            pool.verified_unique_exit_count_fresh(Duration::from_secs(30)),
            0
        );
        assert!(!pool.egress_ready(1, Duration::from_secs(30)));
        assert!(pool.select_proxy_for_key("stale-session").is_none());
    }
}
