# Phase 4: Proxy CLI Manager

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-4-proxy-cli` |
| **Status** | Planned |
| **Dependencies** | Phase 3 (Security hardening) |
| **Scope** | Add `proxy` subcommand group with `ps`, `restart`, `logs`, `status`. Implement Docker WARP lifecycle via CLI rather than `start.sh`. `src/docker.rs` manages container create/resume/verify. |
| **Files to create** | `src/docker.rs` |
| **Files to modify** | `src/cli.rs` (add proxy subcommand), `src/supervisor.rs` (proxy lifecycle) |
| **Expected behavior contract** | `opencode2claude proxy ps` lists WARP containers. `opencode2claude proxy restart <name>` restarts a proxy. `opencode2claude proxy logs <name>` shows container logs. `opencode2claude proxy status` reports health. |
| **Acceptance gates** | cargo gates pass, `proxy --help` works, `proxy ps` lists Docker containers |
| **Verification command** | `./scripts/verify.sh phase-4 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Proxy pool bug fixes (Phase 5), health endpoint polish (Phase 6) |
| **Definition of Done** | 1. All gates pass 2. `proxy ps` lists containers 3. `proxy restart` works 4. No Docker management in `proxy_pool.rs` 5. No CRITICAL/HIGH findings |
