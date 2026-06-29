# Phase 5: Proxy Pool Fix

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-5-proxy-pool-fix` |
| **Status** | Planned |
| **Dependencies** | Phase 4 (Proxy CLI manager) |
| **Scope** | Fix hot-spare selection bug: `get_client()` uses status-based selection instead of index range. Remove Docker management from `proxy_pool.rs` (moved to `docker.rs` in Phase 4). Fix `health_monitor` to do real SOCKS5 HTTP check instead of TCP connect. |
| **Files to modify** | `src/proxy_pool.rs` (spare visibility, health_monitor SOCKS5 check) |
| **Expected behavior contract** | Hot-spare promoted to Active is immediately visible to `get_client()`. `health_monitor` sends real HTTP request through SOCKS5 proxy to verify connectivity. No `docker run`/`docker kill` in `proxy_pool.rs`. |
| **Acceptance gates** | cargo gates pass, spare active visible, no docker in proxy_pool |
| **Verification command** | `./scripts/verify.sh phase-5 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+), architecture-consistency (MEDIUM+) |
| **Out of scope** | Adding new proxies to pool, changing pool sizing algorithm |
| **Definition of Done** | 1. All gates pass 2. `get_client()` finds spare promoted to Active 3. `health_monitor` does SOCKS5 HTTP check 4. `proxy_pool.rs` has zero Docker commands 5. No CRITICAL/HIGH findings |
