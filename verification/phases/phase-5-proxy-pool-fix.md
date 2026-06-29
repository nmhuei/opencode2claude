# Phase 5: Proxy Pool Fix — 2-Tier Primary/Warm-Standby Routing

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-5-proxy-pool-fix` |
| **Status** | Planned |
| **Dependencies** | Phase 4 (Proxy CLI manager, Docker containment in `docker.rs`) |
| **Scope** | Implement primary-first deterministic hashing with warm-standby fallback. `get_client()` selects from the primary tier (ports 40001-40003, indices 0-2) first via `hash(api_key) mod PRIMARY_COUNT`. If the selected primary is unavailable (cooldown/dead/starting), fall back to a warm-standby proxy (ports 40004-40005, indices 3-4). Warm-standby nodes are **never marked as managed**, **never restarted by runtime recovery**. Sticky mapping to the same primary proxy persists while the primary remains healthy. |
| **Files to modify** | `src/proxy_pool.rs` (2-tier `get_client`, `get_client_excluding`, `health_monitor` skip standby, `process_restart_queue` skip standby), `src/opencode/forward.rs` (retry routing to attempt other primaries before warm-standby fallback) |
| **Expected behavior contract** | 1. `get_client(api_key)` hashes key, maps to a primary (0-2). 2. If primary is usable, return it (sticky). 3. If primary is on cooldown/dead, fall back to a warm-standby (3-4). 4. Warm-standby proxies are **never** assigned to `ProxyStatus::Active` or promoted into the primary active set. 5. `health_monitor` skips warm-standby indices entirely. 6. `process_restart_queue` ignores warm-standby indices. 7. `get_client_excluding` tries other primaries first, then falls back to warm-standby. 8. Sticky mapping is stable while primary is healthy — same `api_key` always hits the same primary. |
| **Acceptance gates** | G1: hash selects primary first. G2: failed primary falls back to warm-standby. G3: warm-standby never marked managed/active. G4: warm-standby not restarted by recovery/health monitor. G5: sticky mapping stable while primary healthy. |
| **Verification command** | `cargo test --lib proxy_pool -- --nocapture && cargo clippy -- -D warnings` |
| **Review requirements** | code-reviewer (MEDIUM+), architecture-consistency (MEDIUM+) |

---

## Architecture

### Tier Layout

| Tier | Ports | Indices | Managed | Health Monitoring | Auto-restart |
|------|-------|---------|---------|-------------------|-------------|
| **Primary** | 40001-40003 | 0-2 | Yes | Monitored by `health_monitor` | Restarted by `process_restart_queue` |
| **Warm-standby** | 40004-40005 | 3-4 | **No** | **Skipped** by `health_monitor` | **Never** restarted |

### Routing Algorithm (`get_client`)

```
hash(api_key) → primary_index = hash % PRIMARY_COUNT  // PRIMARY_COUNT = 3

if proxies[primary_index] is usable (Active, Spare, or cooldown-expired):
    return proxies[primary_index]
else:
    // Primary unavailable — fall back to warm-standby tier
    fallback_index = WARM_STANDBY_START + (hash % WARM_STANDBY_COUNT)
    // WARM_STANDBY_START = 3, WARM_STANDBY_COUNT = 2
    return proxies[fallback_index]
```

### Failover Retry Routing (`get_client_excluding`)

```
hash(api_key) → primary_index

if exclude_idx == primary_index:
    // Failed primary — try other primaries first, then warm-standby
    for each other_primary in (0..PRIMARY_COUNT) excluding exclude_idx:
        if proxies[other_primary] is usable:
            return proxies[other_primary]
    // All other primaries unavailable, try warm-standby
    return warm_standby_fallback(api_key, exclude_idx)
else:
    // Non-primary exclusion — return primary as normal
    return proxies[primary_index]
```

### Key Constraints

1. **Primary count is constant** — `PRIMARY_COUNT = 3` (hardcoded or derived from proxy config). The first 3 proxies in the pool are primary; the rest are warm-standby.

2. **Warm-standby status invariant** — Warm-standby proxies are initialized as `ProxyStatus::Spare` and **never** transition to `ProxyStatus::Active`. They remain Spare or Cooldown at most. The health monitor and restart queue explicitly filter them out.

3. **Sticky mapping** — `hash(api_key)` returns the same primary index every call. The mapping only changes when the primary is in a non-usable state (cooldown, dead, or starting). Once the primary recovers (cooldown expires or health monitor marks it Spare), the next call to `get_client()` routes back to the original primary.

4. **Proxy pool construction** — `ProxyPool::new()` accepts all proxy URLs but records the split point. Warm-standby proxies are marked Spare and never participate in hot-spare promotion to Active.

---

## Out of Scope

- Adding/removing proxies to/from the pool at runtime
- Changing PRIMARY_COUNT or WARM_STANDBY_COUNT via config (hardcoded for now, parameterization is future work)
- WARP CLI rotation logic (unchanged, in `forward.rs`)
- Docker container lifecycle beyond the restart queue (already in `docker.rs` from Phase 4)
- Capacity planning or autoscaling proxy count
- Metrics/observability beyond existing `info`/`warn`/`error` logs

## Definition of Done

1. All cargo gates pass (`cargo test --lib proxy_pool`, `cargo clippy -- -D warnings`, `cargo fmt --check`)
2. `get_client()` selects primary (0-2) first via hash, falls back to warm-standby (3-4) when primary is unavailable
3. `get_client_excluding()` routes to other primaries first, then warm-standby
4. Warm-standby proxies (3-4) are never promoted to Active status
5. `health_monitor` only checks indices < PRIMARY_COUNT
6. `process_restart_queue` / `restart_container` skips warm-standby indices
7. Sticky mapping: same `api_key` returns same primary across calls while health is stable
8. No CRITICAL or HIGH code review findings
9. All new unit tests pass
