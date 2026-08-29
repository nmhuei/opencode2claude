# Proxy Pool Architecture

## Two-Tier Model

opencode2api uses a two-tier proxy pool:

1. **Primary Managed Pool** (ports 40001–40003)
   - Managed by opencode2api CLI
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

Primary and warm-standby pools are resolved from configuration rather than a fixed node count:

```text
BRIDGE_PRIMARY_PROXIES=socks5h://127.0.0.1:40001,socks5h://127.0.0.1:40002
BRIDGE_WARM_STANDBY_PROXIES=socks5h://127.0.0.1:40004
BRIDGE_ACTIVE_PROXY_COUNT=2
```

`BRIDGE_ACTIVE_PROXY_COUNT` limits how many configured primaries are in the normal serving set; extra configured primaries remain present but routing-disabled. Protected role is still identified by the reserved standby ports (40004–40005) for lifecycle safety.

## Routing Policy

### Primary-first round robin with burst coalescing

1. Fresh normal traffic is selected only from routing-enabled, healthy, closed-circuit, non-duplicate, non-rate-limited, non-draining primaries with fresh identity when identity verification is required.
2. The pool advances across eligible primaries with a round-robin counter while concurrent bursts may reuse the same active primary to avoid unnecessary churn.
3. Warm standbys are considered only after there is no eligible primary; they remain protected from destructive lifecycle operations.
4. A draining node receives no new normal or half-open probe route, but existing request leases are allowed to finish.
5. Hybrid mode may fall back to direct only for startup/transport availability according to the hybrid egress policy; strict proxy mode remains fail closed.

### Operator drain

Authenticated management/dashboard controls can drain a managed primary before maintenance. Drain is orthogonal to health: it does not mark the node unhealthy and does not destroy or restart the container. Once active leases reach zero, the operator may safely restart/rotate it. Undrain only removes the routing gate; all health/circuit/identity checks still apply.

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
- WarmStandby proxies are excluded from normal routing while any eligible primary remains; they are failover-only and protected from destructive mutation.
- Draining managed primaries are excluded from fresh normal and probe routing until explicitly undrained or a successful managed recovery clears the drain.
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

## Health and management telemetry

Public `/health` remains intentionally minimal. Detailed proxy topology, drain state, active leases, identity freshness, duplicate ownership and recovery state are available from authenticated dashboard/management surfaces (`/api/dashboard/proxies`, `/api/v1/proxies`) and readiness/status endpoints expose only the bounded egress evidence needed by operators.

See [health-status.md](health-status.md) and [management-api.md](management-api.md) for the current contracts.
