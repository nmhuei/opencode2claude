# Phase 6: Health/Status/Log Polish

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-6-health-status-log` |
| **Status** | Planned |
| **Dependencies** | Phase 2 (Runtime + PID), Phase 4 (Proxy CLI manager) |
| **Scope** | Add `src/health.rs` — port check + `/health` poll. Create `src/supervisor.rs` `status()` reads PID file + bridge health. `logs` subcommand tails stdout/stderr journal. `/health` endpoint returns structured JSON (no config leak). |
| **Files to create** | `src/health.rs` |
| **Files to modify** | `src/supervisor.rs` (status, logs), `src/main.rs` (wire in health module) |
| **Expected behavior contract** | `opencode2claude status` shows bridge running/stopped, proxy pool health, uptime. `opencode2claude logs` tails recent bridge output. `/health` returns status without exposing config secrets. Health endpoint returns `{ proxy_pool: { primary: { managed: true, ports: [40001,40002,40003] }, warm_standby: { protected: true, ports: [40004,40005] } } }`. |
| **Acceptance gates** | cargo gates pass, `status` shows bridge state, `logs` returns output, `/health` is clean |
| **Verification command** | `./scripts/verify.sh phase-6 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Docker container management (Phase 4), proxy pool fixes (Phase 5), documentation (Phase 7) |
| **Definition of Done** | 1. All gates pass 2. `status` shows correct bridge state 3. `logs` returns recent output 4. `/health` doesn't leak config 5. No CRITICAL/HIGH findings |
