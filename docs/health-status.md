# Health Status & Proxy Telemetry

The `/health` endpoint exposes bridge and proxy pool status as JSON.

## Health Endpoint

```
GET /health
```

No authentication required (always public for monitoring tools).

### Response Schema

```json
{
  "status": "healthy",
  "version": "0.2.1",
  "daemon": {
    "running": true,
    "port": 4096
  },
  "config": {
    "model": "opencode/deepseek-v4-flash-free",
    "shell_policy": "unrestricted",
    "auth_enabled": false,
    "bridge_port": 4000
  },
  "proxy_pool": {
    "policy": "primary-with-warm-standby",
    "primary": {
      "ports": [40001, 40002, 40003],
      "total": 3,
      "healthy": 3,
      "degraded": 0,
      "cooldown": 0,
      "recovering": 0,
      "dead": 0,
      "protected": false
    },
    "warm_standby": {
      "ports": [40004, 40005],
      "total": 2,
      "healthy": 2,
      "degraded": 0,
      "cooldown": 0,
      "recovering": 0,
      "dead": 0,
      "protected": true
    },
    "nodes": [
      {
        "port": 40001,
        "role": "Primary",
        "lifecycle": "Managed",
        "status": "healthy",
        "failure_count": 0,
        "success_count": 0,
        "cooldown_remaining_secs": null
      }
    ]
  }
}
```

### Field Reference

| Field | Type | Description |
|-------|------|-------------|
| `policy` | `string` | Always `primary-with-warm-standby` |
| `primary` | `ProxyTierStats` | Primary managed proxy pool (40001–40003) |
| `warm_standby` | `ProxyTierStats` | Warm-Standby protected pool (40004–40005) |
| `nodes` | `ProxyNodeStats[]` | Per-proxy status array |

#### ProxyTierStats

| Field | Description |
|-------|-------------|
| `ports` | Ports in this tier |
| `total` | Total proxy count |
| `healthy` | Proxies with `Active` status |
| `degraded` | Proxies with `Spare` status |
| `cooldown` | Proxies with `Cooldown` status |
| `recovering` | Proxies with `Starting` status |
| `dead` | Proxies with `Dead` status |
| `protected` | `true` if these proxies are protected from CLI modification |

#### ProxyNodeStats

| Field | Description |
|-------|-------------|
| `port` | Proxy port |
| `role` | `Primary` or `WarmStandby` |
| `lifecycle` | `Managed` or `Protected` |
| `status` | `healthy`, `spare`, `cooldown`, `dead`, `starting` |
| `failure_count` | Consecutive transport failures |
| `success_count` | Consecutive successful transports since cooldown |
| `cooldown_remaining_secs` | Seconds until cooldown expires (`null` if not in cooldown) |

## Telemetry Policy

### Proxy Transport Failure

A proxy is marked as having a **transport failure** only when:

| Event | Action | Reason |
|-------|--------|--------|
| Network/proxy connection error (DNS, TCP, TLS) | `record_failure(idx)` | Proxy is unreachable |
| HTTP request times out at transport layer | `record_failure(idx)` | Proxy not responding |

After `FAILURE_THRESHOLD` (default: **2**) consecutive failures, the proxy enters **cooldown** for `COOLDOWN_SECS` (default: **120s**).

### Proxy Transport Success

Any HTTP response received from the upstream (regardless of status code) counts as a **transport success**:

| Status Code | Transport Success | Proxy Health Impact | Notes |
|-------------|-------------------|---------------------|-------|
| 200–299 | ✅ `record_success` | Success — failures reset, recovery counter advances | Normal operation |
| 400 | ✅ `record_success` | **No proxy failure** | Upstream rejected request (bad input, auth) |
| 401/403/404 | ✅ `record_success` | **No proxy failure** | Upstream-level access denied |
| 422 | ✅ `record_success` | **No proxy failure** | Validation error in request |
| 429 | ✅ `record_success` | **No proxy failure** — but proxy may be `mark_rate_limited` | Rate-limit pressure |
| 500–599 | ✅ `record_success` | **No proxy failure** — but proxy may be `mark_rate_limited` | Upstream/server error |

**Key rule:** Upstream HTTP errors (4xx, 429, 5xx) do NOT mark the proxy as failed.
The proxy successfully transported the request to the upstream and back — the error is the upstream's response, not a proxy failure.

Rate-limit pressure (429, 5xx, some 400s) may trigger `mark_rate_limited()` to put the proxy
on a **cooldown timer**, but this is a **routing preference**, not a health failure.

### Recovery

After a proxy enters cooldown from transport failures, it can recover:

1. Each successful transport (`record_success`) resets the failure count and increments a success counter
2. After `RECOVERY_SUCCESS_COUNT` (default: **2**) consecutive successes, the proxy transitions from `Cooldown` → `Active`
3. Recovery is automatic — no manual intervention needed

### Cooldown Timeout

If the proxy entered cooldown with a fixed duration (e.g., `COOLDOWN_SECS` = 120s),
the cooldown expires naturally. Upon expiry, the proxy becomes eligible for selection
again even without `record_success` calls.

### Example Scenarios

| Scenario | Proxy Health | Routing Impact |
|----------|-------------|----------------|
| All proxies healthy | All Active | Normal primary-first routing |
| Primary A gets 2 transport errors | Primary A → `Cooldown` (120s) | Primary A's agents fail over to WarmStandby |
| Primary A gets a successful transport | Primary A → `Active` (recovery counter resets) | Selective recovery |
| Primary A gets transport timeout | `record_failure` (failure counter incremented) | After 2nd consecutive, enters cooldown |
| Primary A gets HTTP 400 from upstream | `record_success` (no failure) | No health impact |
| All primaries healthy, WarmStandby idle | WarmStandby stays `Active` | No routing to WarmStandby |
