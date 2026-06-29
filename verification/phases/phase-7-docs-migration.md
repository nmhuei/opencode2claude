# Phase 7: Docs + Migration

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-7-docs-migration` |
| **Status** | Planned |
| **Dependencies** | Phase 5 (Proxy pool fix), Phase 6 (Health/Status/Log) |
| **Scope** | Write `docs/cli.md` with all subcommands and examples. Write `docs/migration-from-start-sh.md`. Update `start.sh` → wrapper calling `opencode2claude start`. Update `stop.sh` → wrapper calling `opencode2claude stop`. |
| **Files to create** | `docs/cli.md`, `docs/migration-from-start-sh.md` |
| **Files to modify** | `start.sh` (delegate to supervisor), `stop.sh` (delegate to supervisor) |
| **Expected behavior contract** | `docs/cli.md` documents all subcommands. `docs/migration-from-start-sh.md` explains migration path. `start.sh` is a thin wrapper around `opencode2claude start`. `stop.sh` is a thin wrapper around `opencode2claude stop`. |
| **Acceptance gates** | cargo gates pass, docs exist, cli.md documents all commands, migration guide covers start.sh users |
| **Verification command** | `./scripts/verify.sh phase-7 --profile ci` |
| **Review requirements** | architecture-consistency (MEDIUM+) |
| **Out of scope** | CI workflow changes (Phase 8), release automation (Phase 8) |
| **Definition of Done** | 1. All gates pass 2. `docs/cli.md` complete 3. Migration guide written 4. `start.sh`/`stop.sh` delegate to supervisor 5. No CRITICAL/HIGH findings |
