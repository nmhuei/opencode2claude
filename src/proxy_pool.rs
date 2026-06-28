use std::time::{Instant, Duration};
use reqwest::Client;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct ProxyItem {
    pub url: String,
    pub client: Client,
    pub rate_limited_until: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct ProxyPool {
    pub proxies: Vec<ProxyItem>,
}

impl ProxyPool {
    pub fn new(proxies_urls: &[String]) -> Self {
        let mut proxies = Vec::new();
        for url in proxies_urls {
            // Build client with this proxy
            if let Ok(proxy) = reqwest::Proxy::all(url) {
                if let Ok(client) = Client::builder()
                    .proxy(proxy)
                    .timeout(Duration::from_secs(600))
                    .pool_max_idle_per_host(10)
                    .build()
                {
                    proxies.push(ProxyItem {
                        url: url.clone(),
                        client,
                        rate_limited_until: None,
                    });
                    info!("Added proxy to pool: {}", url);
                } else {
                    warn!("Failed to build reqwest Client for proxy: {}", url);
                }
            } else {
                warn!("Invalid proxy URL: {}", url);
            }
        }
        Self { proxies }
    }

    pub fn get_client(&mut self, api_key: &str) -> Option<(Client, String, usize)> {
        if self.proxies.is_empty() {
            return None;
        }

        // 1. Calculate preferred index based on API key hash
        let mut hasher = DefaultHasher::new();
        api_key.hash(&mut hasher);
        let hash_val = hasher.finish() as usize;
        let preferred_idx = hash_val % self.proxies.len();

        let now = Instant::now();

        // 2. Clear expired cooldowns
        for i in 0..self.proxies.len() {
            if let Some(until) = self.proxies[i].rate_limited_until {
                if now >= until {
                    info!("Proxy #{} ({}) cooldown expired, proxy is back online.", i, self.proxies[i].url);
                    self.proxies[i].rate_limited_until = None;
                }
            }
        }

        // 3. Try to find the first active proxy starting from preferred_idx
        for i in 0..self.proxies.len() {
            let idx = (preferred_idx + i) % self.proxies.len();
            let is_limited = self.proxies[idx].rate_limited_until
                .map(|until| now < until)
                .unwrap_or(false);

            if !is_limited {
                if i > 0 {
                    warn!(
                        "Preferred proxy #{} ({}) is rate-limited. Failing over to proxy #{} ({}).",
                        preferred_idx, self.proxies[preferred_idx].url, idx, self.proxies[idx].url
                    );
                } else {
                    info!("Using preferred proxy #{} ({}) for agent", idx, self.proxies[idx].url);
                }
                return Some((self.proxies[idx].client.clone(), self.proxies[idx].url.clone(), idx));
            }
        }

        // 3. Fallback: if all are rate-limited, use the preferred one
        let item = &self.proxies[preferred_idx];
        warn!(
            "All proxies in pool are currently rate-limited. Falling back to preferred proxy #{} ({}).",
            preferred_idx, item.url
        );
        Some((item.client.clone(), item.url.clone(), preferred_idx))
    }

    pub fn mark_rate_limited(&mut self, idx: usize, duration: Duration) {
        if idx < self.proxies.len() {
            let until = Instant::now() + duration;
            self.proxies[idx].rate_limited_until = Some(until);
            warn!(
                "Proxy #{} ({}) marked as rate-limited until {:?}",
                idx, self.proxies[idx].url, until
            );
        }
    }

    pub fn mark_rate_limited_adaptive(&mut self, idx: usize, retry_count: u32) {
        let secs = 60 * 2u64.pow(retry_count.min(3));
        let duration = Duration::from_secs(secs);
        self.mark_rate_limited(idx, duration);
    }

    #[allow(dead_code)]
    pub fn mark_healthy(&mut self, idx: usize) {
        if idx < self.proxies.len() {
            self.proxies[idx].rate_limited_until = None;
            info!(
                "Proxy #{} ({}) marked as healthy.",
                idx, self.proxies[idx].url
            );
        }
    }

    pub fn get_client_excluding(&mut self, api_key: &str, exclude_idx: usize) -> Option<(Client, String, usize)> {
        if self.proxies.is_empty() {
            return None;
        }

        let mut hasher = DefaultHasher::new();
        api_key.hash(&mut hasher);
        let hash_val = hasher.finish() as usize;
        let preferred_idx = hash_val % self.proxies.len();

        let now = Instant::now();

        // Clear expired cooldowns
        for i in 0..self.proxies.len() {
            if let Some(until) = self.proxies[i].rate_limited_until {
                if now >= until {
                    info!("Proxy #{} ({}) cooldown expired, proxy is back online.", i, self.proxies[i].url);
                    self.proxies[i].rate_limited_until = None;
                }
            }
        }

        for i in 0..self.proxies.len() {
            let idx = (preferred_idx + i) % self.proxies.len();
            if idx == exclude_idx {
                continue;
            }

            let is_limited = self.proxies[idx].rate_limited_until
                .map(|until| now < until)
                .unwrap_or(false);

            if !is_limited {
                info!("Using proxy #{} ({}) (excluding #{})", idx, self.proxies[idx].url, exclude_idx);
                return Some((self.proxies[idx].client.clone(), self.proxies[idx].url.clone(), idx));
            }
        }

        // Fallback: all non-excluded proxies are rate-limited, pick first non-excluded
        for i in 0..self.proxies.len() {
            let idx = (preferred_idx + i) % self.proxies.len();
            if idx != exclude_idx {
                let item = &self.proxies[idx];
                warn!(
                    "All non-excluded proxies are rate-limited. Falling back to proxy #{} ({}).",
                    idx, item.url
                );
                return Some((item.client.clone(), item.url.clone(), idx));
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_pool_mapping() {
        let urls = vec![
            "socks5://127.0.0.1:40001".to_string(),
            "socks5://127.0.0.1:40002".to_string(),
            "socks5://127.0.0.1:40003".to_string(),
        ];
        let mut pool = ProxyPool::new(&urls);
        assert_eq!(pool.proxies.len(), 3);

        // Same API key should always map to same proxy index
        let res1 = pool.get_client("agent-1").unwrap();
        let res2 = pool.get_client("agent-1").unwrap();
        assert_eq!(res1.2, res2.2);

        // Different API keys may map to different indexes
        let res3 = pool.get_client("agent-2").unwrap();
        // Since we only have 3 proxies, hash collision is possible, but they represent logical partitioning
        info!("agent-1 mapped to preferred proxy index {}", res1.2);
        info!("agent-2 mapped to preferred proxy index {}", res3.2);
    }

    #[test]
    fn test_proxy_pool_failover() {
        let urls = vec![
            "socks5://127.0.0.1:40001".to_string(),
            "socks5://127.0.0.1:40002".to_string(),
            "socks5://127.0.0.1:40003".to_string(),
        ];
        let mut pool = ProxyPool::new(&urls);

        // Get preferred proxy index for "agent-test"
        let preferred = pool.get_client("agent-test").unwrap().2;

        // Mark preferred proxy as rate-limited
        pool.mark_rate_limited(preferred, Duration::from_secs(60));

        // Get client again. It should failover to a different index
        let after_failover = pool.get_client("agent-test").unwrap();
        assert_ne!(after_failover.2, preferred);

        // Mark all proxies as rate-limited
        for idx in 0..3 {
            pool.mark_rate_limited(idx, Duration::from_secs(60));
        }

        // Get client. It should fallback to preferred index
        let fallback = pool.get_client("agent-test").unwrap();
        assert_eq!(fallback.2, preferred);
    }

    #[test]
    fn test_get_client_excluding() {
        let urls = vec![
            "socks5://127.0.0.1:40001".to_string(),
            "socks5://127.0.0.1:40002".to_string(),
            "socks5://127.0.0.1:40003".to_string(),
        ];
        let mut pool = ProxyPool::new(&urls);

        let preferred = pool.get_client("agent-excl").unwrap().2;

        // Excluding the preferred proxy should return a different one
        let result = pool.get_client_excluding("agent-excl", preferred).unwrap();
        assert_ne!(result.2, preferred);

        // Excluding a non-preferred proxy should still return the preferred one
        let other_idx = (preferred + 1) % 3;
        let result2 = pool.get_client_excluding("agent-excl", other_idx).unwrap();
        assert_eq!(result2.2, preferred);
    }
}
