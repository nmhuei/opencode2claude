# Phase 1: CLI Skeleton

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-1-cli-skeleton` |
| **Status** | Implementation Complete |
| **Dependencies** | None |
| **Scope** | Refactor `main.rs` to use Clap subcommands: `serve`, `start`, `status`, `stop`, `restart`, `env`, `logs`. Extract `run_server()`. Create `src/cli.rs`. |
| **Files to create** | `src/cli.rs` |
| **Files to modify** | `src/main.rs` (refactor flat args → subcommands), `Cargo.toml` (update Clap dep) |
| **Expected behavior contract** | `opencode2claude --help` lists all subcommands. `opencode2claude serve` starts bridge on configured port. `opencode2claude start/status/stop/restart/env/logs` are accepted (may error until Phase 2). `opencode2claude` (no args) runs `serve`. |
| **Acceptance gates** | cargo fmt ✓, clippy ✓, check ✓, unit tests ✓, build ✓, `--help` lists subcommands ✓, `serve --port X` serves `/health` ✓, integration tests pass ✓ |
| **Verification command** | `./scripts/verify.sh phase-1 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+), architecture-consistency (MEDIUM+) |
| **Out of scope** | Runtime directory creation, PID file management, Docker control, health endpoint beyond basic check, security hardening |
| **Definition of Done** | 1. All 8 gates pass on `--profile local` 2. `opencode2claude --help` shows 7 subcommands 3. `opencode2claude serve --port X` serves `/health` 4. All existing unit tests still pass 5. No CRITICAL/HIGH code review findings |
