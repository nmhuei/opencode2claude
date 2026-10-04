# Proxy Pool Architecture

## Two-Tier Model

opencode2api uses a two-tier proxy pool:

1. **Primary Managed Pool** (default: port 40001)
   - Managed by opencode2api CLI
   - Can be started, restarted, recovered, rotated, purged
   - Used as default routing targets for normal traffic
   - Docker containers managed via CLI (`proxy restart`, `proxy purge`)

2. **Warm-Standby Protected Pool** (default: port 40004)
   - Protected anchor proxies
   - **Never** stopped, purged, or recreated by destructive CLI operations
   - Kept warm and health-checked for failover readiness
   - Used as temporary failover target when the selected primary is unhealthy/cooldown/dead
   - WarmStandby does not receive normal traffic while a primary is eligible

## Configuration

```
BRIDGE_PRIMARY_PROXIES=socks5h://127.0.0.1:40001
BRIDGE_WARM_STANDBY_PROXIES=socks5h://127.0.0.1:40004
BRIDGE_ACTIVE_PROXY_COUNT=1
```

The release default is **1 primary + 1 protected warm standby**, but the URL lists remain configurable for larger pools. CLI status, Docker bootstrap, dashboard proxy controls, doctor checks, and bulk lifecycle operations all derive their ports from the same resolved configuration instead of a fixed 3+2 list.

## Routing Policy

### Primary-First, Rendezvous Hashing

1. Each API key (routing key) is hashed via Rendezvous (highest random weight) to a deterministic primary proxy index
2. Normal traffic always uses the assigned primary proxy while it is healthy
3. If the selected primary is unhealthy/cooldown/dead, traffic fails over to WarmStandby
4. **Affected-agent-only remap**: failure of one primary does NOT remap agents assigned to healthy primaries

### Selection Flow

```
request → hash(key) → rendezvous primary
  ├─ primary healthy? → return primary
  └─ primary unhealthy? → rendezvous warm-standby
     ├─ warm-standby healthy? → return warm-standby
     └─ warm-standby unhealthy? → degraded (any available proxy)
```

### Sticky Determinism

- Same API key → same primary proxy every time (deterministic via Rendezvous hashing)
- Proxy URLs are stable — changing the pool requires configuration change (which re-hashes)

## Cooldown & Recovery Policy

### Transport Failure Threshold

| Constant | Default | Description |
|----------|---------|-------------|
| `FAILURE_THRESHOLD` | 2 | Consecutive transport failures before cooldown |
| `COOLDOWN_SECS` | 120 | Default cooldown duration (seconds) |
| `RECOVERY_SUCCESS_COUNT` | 2 | Consecutive successes required to auto-recover |

### Telemetry Distinction

| Event | Proxy Transport Failure? | Action |
|-------|-------------------------|--------|
| Network/proxy connection error | ✅ Yes | `record_failure()` → cooldown at threshold |
| HTTP request timeout | ✅ Yes | `record_failure()` → cooldown at threshold |
| HTTP 200–299 response | ❌ No | `record_success()` — resets failure count |
| HTTP 4xx (400/401/403/404/422) | ❌ No | `record_success()` — transport succeeded |
| HTTP 429 / 5xx | ❌ No | `record_success()` — may also `mark_rate_limited()` |
| Any HTTP response received | ❌ No | `record_success()` — proxy delivered the request |

**Upstream HTTP errors are NOT proxy transport failures.** The proxy successfully
connected, sent the request, and received a response. Only raw transport/network
errors (DNS, TCP, TLS, timeout) indicate proxy failure.

### Recovery Mechanism

After cooldown, a proxy recovers via:
1. **Auto-recovery via successes** — after `RECOVERY_SUCCESS_COUNT` consecutive
   `record_success()` calls, the proxy transitions from `Cooldown` → `Active`
2. **Cooldown timeout** — when the cooldown duration expires, the proxy becomes
   eligible for selection again (but status only reverts on next success)

## Safety

- Canonical standby ports `40004-40005` remain protected infrastructure; the default topology uses `40004`.
- `ensure_not_protected(port)` rejects destructive lifecycle operations on protected standby ports.
- WarmStandby proxies are excluded from normal routing: `select_proxy_for_key()` never returns a WarmStandby
  proxy unless the rendezvous-assigned primary is unhealthy
- Deprecated static port 40010 is removed

## Docker Proxy Setup

When Docker is available, `start.sh` automatically provisions WARP SOCKS5 proxy containers:

```bash
# Shell wrapper auto-provisions the proxies
source start.sh
```

- Uses `ghcr.io/mon-ius/docker-warp-socks` images
- Named volumes cache WARP registration config across restarts
- Verified in parallel after startup (15 attempts × 2s each)
- Failed proxies are retried automatically (restart container, re-verify)

## Health Check Integration

The `/health` endpoint exposes proxy pool telemetry:

```json
{
  "proxy_pool": {
    "policy": "primary-with-warm-standby",
    "primary": { "ports": [40001], "total": 1, "healthy": 1, ... },
    "warm_standby": { "ports": [40004], "total": 1, "healthy": 1, "protected": true },
    "nodes": [...]
  }
}
```

See [health-status.md](health-status.md) for full schema and telemetry policy.
