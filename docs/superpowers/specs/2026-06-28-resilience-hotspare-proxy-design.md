# OpenCode2Claude — Resilience & Hot-Spare Proxy Pool Design

> Date: 2026-06-28  
> Status: Draft  
> Supersedes: Previous implicit proxy behavior

## Motivation

Current bridge has 4 bug classes (hardcoded values, silent failures, missing error paths, thundering-herd cooldown) and no mechanism to recover from dead proxies. With multi-agent setups running 3–10 concurrent agents, any single proxy failure kills the pipeline until manual docker intervention.

This spec fixes the bugs and adds a hot-spare proxy model that keeps K active proxies online, transparently swapping in spares on failure and restarting dead containers in the background — same redundancy principle as RAID or rsync backup nodes.

---

## Section 1: Core Bug Fixes

### 1a. Hardcoded search loop limit → config-driven

**Files:** `src/opencode/forward.rs`, `src/handlers.rs`

`BridgeConfig::max_search_loops` is loaded from env `BRIDGE_MAX_SEARCH_LOOPS` but never passed to the forward functions.

**Changes:**

`forward_to_llm_sync` (line 205) — add param:
```rust
pub async fn forward_to_llm_sync(
    state: &AppState,
    api_key: String,
    mut payload: MessagesRequest,
    model: String,
    search_client: SearchClient,
    max_search_loops: u32,                    // NEW
) -> Result<serde_json::Value, BridgeError>
```
Line 215: `if loop_count > max_search_loops {` (was `5`)

`forward_to_llm_stream` (line 400) — add param:
```rust
pub async fn forward_to_llm_stream(
    ...
    max_search_loops: u32,                    // NEW
) -> Result<impl Stream<Item = Result<Event, Infallible>>, BridgeError>
```
Line 427: `if loop_count > max_search_loops {` (was `5`)

**Call sites in `handlers.rs`** (lines 209, 228):
```rust
// Line 209
opencode::forward_to_llm_stream(
    &state, api_key, payload, req_model,
    state.config.channel_capacity,
    state.search_client.clone(),
    state.config.max_search_loops,            // NEW
)
// Line 228
opencode::forward_to_llm_sync(
    &state, api_key, payload, req_model,
    state.search_client.clone(),
    state.config.max_search_loops,            // NEW
)
```

### 1b. 429 post-retry returns error, not Ok(response)

**File:** `src/opencode/forward.rs` line 122

```rust
// BEFORE (bug):
return Ok(response);

// AFTER:
return Err(BridgeError::UpstreamError(format!(
    "Rate limited after {} retries (status {})",
    retry_count, status
)));
```

This ensures Claude Code sees a descriptive "rate limited" message instead of a generic upstream error.

### 1c. rotate_warp_ip() error checking

**File:** `src/opencode/forward.rs` lines 32–45

Add output.status checking for both `disconnect` and `connect` commands. Log warning on failure. Return early if `warp-cli` is not in PATH.

Full replacement (see Appendix A for the complete function).

### 1d. Cooldown jitter

**File:** `src/proxy_pool.rs` line 110–113

Add hash-based jitter (±25%) to prevent all proxies from expiring cooldown simultaneously:

```rust
pub fn mark_rate_limited_adaptive(&mut self, idx: usize, retry_count: u32) {
    let base_secs = 60 * 2u64.pow(retry_count.min(3));
    // Deterministic jitter — no rand crate needed
    let jitter_factor = match idx % 4 {
        0 => 100,  // 1.00x
        1 => 85,   // 0.85x
        2 => 115,  // 1.15x
        _ => 95,   // 0.95x
    };
    let secs = base_secs * jitter_factor / 100;
    self.mark_rate_limited(idx, Duration::from_secs(secs));
}
```

---

## Section 2: Proxy Hot-Spare Pool

### 2.1 State Machine

```
                  ┌─────────────────────────────────┐
                  │                                 │
                  ▼                                 │
 ┌────────┐  rate-limit  ┌───────────┐  expired  ───┘
 │ Active │──────────────▶│ Cooldown  │──────────────┘
 └────────┘              └───────────┘
     │                        │
     │                    (timeout
     │                     too long)
     ▼                        ▼
 ┌────────┐            ┌───────────┐    retry ≤3   ┌──────────┐   verify   ┌───────┐
 │  Dead  │───────────▶│ Restarting │──────────────▶│ Starting │──────────▶│ Spare │
 └────────┘            └───────────┘    retry >3    └──────────┘            └───────┘
     │                                            verify fail
     │                                               │
     ▼                                               ▼
 ┌──────────┐                                  ┌──────────┐
 │ Dead     │                                  │ Dead     │
 │(permanent)│                                 │(permanent)│
 └──────────┘                                  └──────────┘
```

**Transitions:**
- **Active** → **Cooldown**: Upstream returned 429/503.
- **Cooldown** → **Active**: Cooldown expired, proxy healthy.
- **Active/Spare** → **Dead**: TCP connect failed or reqwest error on proxy.
- **Dead** → **Restarting**: Background health worker picks it up.
- **Restarting** → **Starting**: `docker create` + `docker start` succeeded.
- **Starting** → **Spare**: TCP connect + cloudflare trace verified.
- **Starting** → **Dead** (permanent): verify failed ≥ 3 times.
- **Spare** → **Active**: swap-in via status change when an Active proxy dies.

### 2.2 Data Structures

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProxyStatus {
    Active,
    Spare,
    Cooldown(Instant),      // until (not started-at)
    Dead { restart_attempts: u32 },
    Starting,                // docker container being verified
}

#[derive(Debug)]
pub struct ProxyEntry {
    pub url: String,
    pub client: Client,
    pub status: ProxyStatus,
    pub port: u16,
    pub container_name: String,  // "opencode-warp-{idx+1}"
}

pub struct ProxyPool {
    pub proxies: Vec<ProxyEntry>,
    pub active_count: usize,   // indices [0..active_count) are Active/Cooldown
    pub restart_queue: Vec<usize>,
    restart_in_progress: bool,
}
```

`active_count` is set at pool creation:
```rust
let active_count = env::var("BRIDGE_ACTIVE_PROXY_COUNT")
    .ok().and_then(|v| v.parse().ok())
    .unwrap_or(proxies.len().saturating_sub(1));
```

### 2.3 get_client Algorithm

```
fn get_client(api_key: &str) -> Option<(Client, String, usize)>
  1. Build active list: indices [0..active_count) where status == Active|Cooldown.
     If empty → swap first Spare into active slot.
       (status = Active, spare index becomes active, active_count unchanged)
     If still empty → degraded mode (see below).

  2. Hash api_key → prefer_idx within active list.

  3. If proxies[prefer_idx] is Cooldown and still cooling → linear probe
     remaining active indices for Active|Cooldown(expired).

  4. If all active are on cooldown → try spare slice [active_count..len]
     filter status == Spare. Pick first → swap into active slot.

  5. Return (client, url, idx). If all failed → run degraded.

Degraded: pick proxy closest to cooldown-end, log CRITICAL.
  If none → return None (caller falls back to default HTTP client).
```

### 2.4 Swap Mechanism

**In-place status change only.** No array element swap.

```rust
fn swap_spare_into_active(&mut self, active_idx: usize, spare_idx: usize) {
    self.proxies[spare_idx].status = ProxyStatus::Active;
    self.proxies[active_idx].status = ProxyStatus::Dead { restart_attempts: 0 };
    self.restart_queue.push(active_idx);
}
```

Container name (`opencode-warp-{idx+1}`) stays tied to index. Hash routing stays deterministic.

### 2.5 Restart Queue

```rust
impl ProxyPool {
    pub fn process_restart_queue(&mut self) {
        if self.restart_in_progress || self.restart_queue.is_empty() {
            return;
        }
        self.restart_in_progress = true;
        let idx = self.restart_queue.remove(0);
        self.proxies[idx].status = ProxyStatus::Restarting;
        // Actual docker work is delegated via channel to a background task
        // so we don't hold the RwLock while docker runs
    }
}
```

A tokio `watch` channel signals the background worker:

```rust
let (restart_tx, mut restart_rx) = tokio::sync::mpsc::channel::<usize>(32);

// Spawned at pool creation:
tokio::spawn(async move {
    while let Some(idx) = restart_rx.recv().await {
        restart_container(idx).await;
    }
});
```

### 2.6 Docker Restart (`restart_container`)

```rust
async fn restart_container(idx: usize, pool: Arc<RwLock<ProxyPool>>, restart_tx: mpsc::Sender<usize>) {
    let port = 40001 + idx;
    let name = format!("opencode-warp-{}", idx + 1);

    // 1. Remove old
    tokio::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output().await.ok();

    // 2. Create + start new
    let create = tokio::process::Command::new("docker")
        .args(["run", "-d", "--name", &name,
            "--restart", "always",
            "--cap-add=NET_ADMIN",
            "--sysctl", "net.ipv4.conf.all.src_valid_mark=1",
            "-p", &format!("{}:9091", port),
            "ghcr.io/mon-ius/docker-warp-socks:latest"])
        .output().await;

    if let Err(e) = create {
        // Push back to queue
        pool.write().await.proxies[idx].status = ProxyStatus::Dead { restart_attempts: 0 };
        restart_tx.send(idx).await.ok();
        return;
    }

    // 3. Mark Starting
    pool.write().await.proxies[idx].status = ProxyStatus::Starting;

    // 4. Verify connectivity (up to ~60s)
    let verified = verify_proxy_socks(port).await;

    let mut pool = pool.write().await;
    if verified {
        pool.proxies[idx].status = ProxyStatus::Spare;
        pool.proxies[idx].restart_attempts = 0;
    } else {
        let attempts = match pool.proxies[idx].status {
            ProxyStatus::Dead { restart_attempts: n } => n + 1,
            _ => 1,
        };
        if attempts < 3 {
            pool.proxies[idx].status = ProxyStatus::Dead { restart_attempts: attempts };
            pool.restart_queue.push(idx);
        } else {
            error!("Proxy {} failed restart after 3 attempts. Giving up.", name);
            pool.proxies[idx].status = ProxyStatus::Dead { restart_attempts: attempts };
        }
    }
    pool.restart_in_progress = false;
}
```

### 2.7 Health Monitor (TCP check)

Background task spawned at pool creation, runs every 10 seconds:

```rust
async fn health_monitor(pool: Arc<RwLock<ProxyPool>>, restart_tx: mpsc::Sender<usize>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let check_indices: Vec<usize> = {
            let pool = pool.read().await;
            pool.proxies.iter().enumerate()
                .filter(|(_, p)| matches!(p.status, ProxyStatus::Dead { .. } | ProxyStatus::Starting))
                .map(|(i, _)| i)
                .collect()
        };
        for idx in check_indices {
            let port = 40001 + idx;
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)).await.is_ok() {
                let mut pool = pool.write().await;
                if matches!(pool.proxies[idx].status, ProxyStatus::Dead { .. }) {
                    pool.proxies[idx].status = ProxyStatus::Spare;
                    info!("Proxy {} recovered by TCP health check.", pool.proxies[idx].container_name);
                }
            }
        }
    }
}
```

### 2.8 Verify Connectivity (after docker restart)

```rust
async fn verify_proxy_socks(port: u16) -> bool {
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{}", port)).unwrap())
        .timeout(Duration::from_secs(5))
        .build().unwrap();

    for attempt in 1..=12 {  // ~60s total
        if client.get("https://cloudflare.com/cdn-cgi/trace")
            .send().await.is_ok()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    false
}
```

### 2.9 get_client with Retry-After Support

In `get_client`, when checking cooldown status:

```rust
fn is_on_cooldown(&self, status: &ProxyStatus) -> bool {
    match status {
        ProxyStatus::Cooldown(until) => Instant::now() < *until,
        _ => false,
    }
}

fn cooldown_remaining(status: &ProxyStatus) -> Option<Duration> {
    match status {
        ProxyStatus::Cooldown(until) => {
            let remaining = until.checked_duration_since(Instant::now());
            remaining
        }
        _ => None,
    }
}
```

In `execute_with_warp_retry`, when receiving 429/503:

```rust
// Try to parse Retry-After header (HTTP/1.1 standard)
let retry_after = response.headers()
    .get("retry-after")
    .and_then(|v| v.to_str().ok())
    .and_then(|s| s.parse::<u64>().ok());

let cooldown = retry_after
    .map(Duration::from_secs)
    .unwrap_or_else(|| get_adaptive_cooldown(retry_count, idx));

pool.mark_rate_limited(idx, cooldown);
```

### 2.10 Degraded Mode (Fixed)

```rust
fn select_degraded(&self) -> Option<usize> {
    let now = Instant::now();
    self.proxies.iter().enumerate()
        .filter(|(_, p)| matches!(p.status, ProxyStatus::Active | ProxyStatus::Cooldown(_)))
        .min_by_key(|(_, p)| match p.status {
            ProxyStatus::Cooldown(until) => {
                // CÒN ÍT THỜI GIAN COOLDOWN NHẤT (sắp available)
                until.checked_duration_since(now).unwrap_or_default()
            }
            ProxyStatus::Active => Duration::ZERO,
            _ => Duration::MAX,
        })
        .map(|(i, _)| i)
}
```

Critical difference from earlier draft: uses `checked_duration_since` to measure **remaining** cooldown, not elapsed. Returns the proxy closest to becoming available.

---

## Section 3: Configuration — New Env Vars

| Variable | Default | Description |
|----------|---------|-------------|
| `BRIDGE_ACTIVE_PROXY_COUNT` | `PROXY_POOL_SIZE - 1` | Number of active proxies in the pool (remainder are spares) |
| `BRIDGE_PROXY_RESTART_MAX` | `3` | Max restart attempts before marking proxy dead permanently |
| `BRIDGE_PROXY_COOLDOWN_BASE` | `60` | Base cooldown in seconds (actual = base × 2^retry × jitter) |

Existing `BRIDGE_PROXY_POOL_SIZE` now also controls the `active_count` default.

---

## Section 4: File Change Summary

| File | Change Type | Lines Changed |
|------|-------------|---------------|
| `src/proxy_pool.rs` | Major rewrite | ~180 to ~350 |
| `src/opencode/forward.rs` | Bug fixes + Retry-After | ~30 |
| `src/handlers.rs` | Pass max_search_loops param | ~6 |
| `src/state.rs` | Wire new pool creation | ~5 |
| `src/config.rs` | No changes (max_search_loops already parsed) | 0 |

No new Cargo dependencies. All timer/interval primitives from `tokio` (already in `[dependencies]`).

---

## Appendices

### Appendix A: Full `rotate_warp_ip()` replacement

```rust
async fn rotate_warp_ip() {
    info!("Rotating WARP IP address...");

    let disconnect = tokio::process::Command::new("warp-cli")
        .arg("disconnect")
        .output()
        .await;

    match disconnect {
        Ok(output) if output.status.success() => {
            info!("warp-cli disconnect succeeded");
        }
        Ok(output) => {
            warn!(
                "warp-cli disconnect returned non-zero: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            warn!("warp-cli disconnect failed (maybe not installed?): {}", e);
            return;
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let connect = tokio::process::Command::new("warp-cli")
        .arg("connect")
        .output()
        .await;

    match connect {
        Ok(output) if output.status.success() => {
            info!("warp-cli connect succeeded");
        }
        Ok(output) => {
            warn!(
                "warp-cli connect returned non-zero: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
            return;
        }
        Err(e) => {
            warn!("warp-cli connect failed: {}", e);
            return;
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
    info!("WARP IP address rotated successfully.");
}
```

### Appendix B: Health Monitor Design for `state.rs`

```rust
// In AppState::new(), after creating proxy_pool:
if !config.proxies.as_ref().map_or(true, |p| p.is_empty()) {
    let pool_clone = state.proxy_pool.clone();
    let restart_tx = state.restart_tx.clone();
    tokio::spawn(async move {
        health_monitor(pool_clone, restart_tx).await;
    });
}
```

`AppState` gains `restart_tx: mpsc::Sender<usize>`.
