# Phase 4: Proxy CLI Manager

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-4-proxy-cli` |
| **Status** | Implementation Complete |
| **Dependencies** | Phase 3 (Security hardening) |
| **Scope** | Implement 2-tier proxy architecture with Primary Managed Pool (40001-40003) and Warm-Standby Protected Pool (40004-40005). Remove 40010. Add proxy CLI commands (ps, status, restart, logs). Add `is_protected_proxy_port` guard. |
| **Files to create** | `src/docker.rs` (Docker operations for primary only) |
| **Files to modify** | `src/proxy_pool.rs` (role/lifecycle types, protected guards), `src/cli.rs` (proxy subcommand), `src/config.rs` (`BRIDGE_PRIMARY_PROXIES`, `BRIDGE_WARM_STANDBY_PROXIES` env vars), `src/main.rs` (wire proxy) |
| **Expected behavior contract** | `proxy ps` lists proxies with roles. `proxy restart` only affects ports 40001-40003. `proxy purge` only affects primary. `is_protected_proxy_port(40004)` returns true. |
| **Acceptance gates** | Protected ports never stopped. Restart affects only primary. Purge affects only primary. Status shows protection status. No 40010 anywhere. |
| **Verification command** | `./scripts/verify.sh phase-4 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Runtime failover routing (Phase 5), health dashboard (Phase 6) |
| **Definition of Done** | 1. All gates pass 2. `proxy ps` lists containers with roles 3. `proxy restart` limited to 40001-40003 4. `is_protected_proxy_port(40004)` returns true 5. No 40010 references remain 6. No CRITICAL/HIGH findings |
