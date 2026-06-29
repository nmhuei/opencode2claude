# Phase 7: Docs + Migration

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-7-docs-migration` |
| **Status** | Implementation Complete |
| **Dependencies** | Phase 4 (2-tier proxy), Phase 5 (routing policy), Phase 6 (health telemetry) |
| **Scope** | Write `docs/cli.md` with all subcommands. Write `docs/migration-from-start-sh.md`. Write `docs/health-status.md` with /health schema. Update `docs/proxy-pool.md` with routing policy, telemetry, failover. Update `README.md` with CLI-first quick start. Update `CLAUDE.md` to match current architecture. Enable phase-7 verification gates. |
| **Files created** | `docs/cli.md`, `docs/migration-from-start-sh.md`, `docs/health-status.md` |
| **Files updated** | `README.md`, `CLAUDE.md`, `docs/proxy-pool.md`, `scripts/phases/phase-7-docs-migration.sh` |
| **Expected behavior contract** | `docs/cli.md` documents all subcommands (start/status/stop/restart/logs/env/proxy). `docs/migration-from-start-sh.md` explains migration from start.sh to CLI. `docs/proxy-pool.md` documents 2-tier architecture, routing policy, cooldown/recovery, telemetry, WarmStandby protection. `docs/health-status.md` documents /health schema and telemetry distinction. `stop.sh` does not stop WarmStandby 40004–40005. No active 40010 references. |
| **Acceptance gates** | cargo gates pass + docs mention CLI commands + docs mention primary/warm-standby pools + docs mention port ranges (40001–40003, 40004–40005) + no 40010 active ref + docs contain /health schema + docs contain telemetry distinction + stop.sh does not stop warm-standby |
| **Verification command** | `./scripts/verify.sh phase-7 --profile ci` |
| **Review requirements** | architecture-consistency (MEDIUM+) |
| **Out of scope** | CI workflow changes (Phase 8), release automation (Phase 8), proxy routing logic changes |
| **Definition of Done** | 1. All gates pass 2. `docs/cli.md` complete 3. `docs/migration-from-start-sh.md` complete 4. `docs/health-status.md` written 5. `docs/proxy-pool.md` updated 6. `README.md` CLI-first 7. `CLAUDE.md` architecture current 8. No CRITICAL/HIGH findings |
