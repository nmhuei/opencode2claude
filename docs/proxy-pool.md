# Proxy Pool Architecture

## Two-Tier Model

opencode2claude uses a two-tier proxy pool:

1. **Primary Managed Pool** (ports 40001–40003)
   - Managed by opencode2claude CLI
   - Can be started, restarted, recovered, rotated, purged
   - Used as default routing targets for normal traffic
   - Docker containers managed via CLI (`proxy restart`, `proxy purge`)

2. **Warm-Standby Protected Pool** (ports 40004–40005)
   - Protected anchor proxies
   - **Never** stopped, restarted, purged, or recreated by CLI
   - Health-checked (read-only) only
   - Used as temporary failover target when selected primary is unhealthy/cooldown/dead
   - WarmStandby does not receive normal traffic

## Configuration

```
BRIDGE_PRIMARY_PROXIES=socks5://127.0.0.1:40001,socks5://127.0.0.1:40002,socks5://127.0.0.1:40003
BRIDGE_WARM_STANDBY_PROXIES=socks5://127.0.0.1:40004,socks5://127.0.0.1:40005
BRIDGE_PROXY_POLICY=primary-with-warm-standby
```

- `PRIMARY_POOL_SIZE` env var (default: `3`) controls primary port count (starting at 40001)
- `STANDBY_POOL_SIZE` env var (default: `2`) controls standby port count (starting after primary)
- Total pool size must not exceed 5 ports (guard enforced in `start.sh`)

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

- Ports 40004–40005 are protected infrastructure — `is_protected_proxy_port()` guards all destructive Docker operations
- `ensure_not_protected(port)` returns an error for ports 40004–40005, preventing restart/purge/stop
- WarmStandby proxies are excluded from normal routing: `select_proxy_for_key()` never returns a WarmStandby
  proxy unless the rendezvous-assigned primary is unhealthy
- Deprecated static port 40010 is removed

## Docker Proxy Setup

When Docker is available, `start.sh` automatically provisions WARP SOCKS5 proxy containers:

```bash
# Standard configuration (3 primary + 2 warm-standby)
PRIMARY_POOL_SIZE=3 STANDBY_POOL_SIZE=2 source start.sh
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
    "primary": { "ports": [40001,40002,40003], "total": 3, "healthy": 3, ... },
    "warm_standby": { "ports": [40004,40005], "total": 2, "healthy": 2, "protected": true },
    "nodes": [...]
  }
}
```

See [health-status.md](health-status.md) for full schema and telemetry policy.
