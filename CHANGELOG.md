# Changelog

All notable changes to opencode2claude will be documented in this file.

## [0.3.2] — 2026-06-30

### Added
- Fast integration test coverage (health, models, shell-disabled-default, empty-messages, 404).
- Docker hygiene gates (`.dockerignore`, `apk add --no-cache`, `--locked` enforcement).
- `cargo audit` + `cargo deny` dependency security checks in CI.
- 17 Phase 8 CI + Release verification gates.

### Changed
- Proxy pool split into `proxy_pool/` module (routing, maintenance, types subtypes).
- Forwarding module split into upstream, retry, and sanitize submodules.
- CI hardened with strict ShellCheck (SC2034, SC2086, SC2059, SC2116 resolved).
- `start.sh` removed dead `BRIDGE_ALL_PROXY`/`BRIDGE_NO_PROXY` vars.

### Fixed
- `clippy::useless_format` in sanitizer attribute extraction.
- `cargo fmt` consistency across handlers, forward, sanitize.
- Test format: shell delegation returns `tool_use` responses (200) instead of `403`.
- Proxy failover integration test: proper accept loop, dual 429, `BRIDGE_PRIMARY_PROXIES` env.

## [0.3.1] — 2026-06-30

### Security (High Blocker Fixes)

- **Shell allowlist metacharacter bypass (C4)** — `has_shell_metacharacters()` rejects `; & | \` $ () > < \n` in `AllowList` mode. Commands like `git status; rm -rf /` now blocked with 15 new tests.
- **Unknown shell policy grants Unrestricted (C3)** — `_` wildcard match now logs a warning and falls back to `ShellPolicy::Disabled` instead of silently enabling `Unrestricted`.
- **Upstream error body leak (B7)** — All 5 client-facing paths sanitized to return status only. Full error body logged server-side.
- `SystemTime::now().unwrap()` panic — replaced with `unwrap_or_default()` in msg_id generation.
- `reqwest::Proxy::all().expect()` panic — replaced with match-based error handling.

### Build

- **ARM64 cross-compilation fix** — Switched reqwest from `native-tls` (OpenSSL) to `rustls-tls`. No OpenSSL sysroot needed for cross-compilation.

## [0.3.0] — 2026-06-30

### Added
- Phase 8: CI + Release pipeline finalization — 14 verification gates for CI/release workflow existence, `--locked` builds, CHANGELOG consistency, version alignment, no active 40010 references.
- Profile-aware test runner: `#[ignore]` live-network tests skip in `ci`/`local` profiles, run all under `heavy` (DDG test previously cost 60s+).
- CHANGELOG.md with full release history.

### Changed
- Version: 0.2.1 → 0.3.0 (all 8 phases complete).
- Release workflow: linux-arm64 target, `--locked` on all builds, `cargo publish --locked`.
- Dockerfile: `cargo build --release --locked` for reproducible builds.

### Fixed
- Docs/code drift: README shell policy default `unrestricted` → `disabled` (Phase 3 sync).
- Docs: README data flow diagram reflects direct API gateway (no OpenCode CLI intermediary).
- Docs: proxy-pool.md removes non-existent `BRIDGE_PROXY_POLICY`, `PRIMARY_POOL_SIZE`, `STANDBY_POOL_SIZE` env vars.
- Docs: health-status.md daemon schema `running: bool` → `status: string`.
- install.sh: `fetch_latest_version()` uses `dl()` helper instead of hardcoded `curl`.
- verification: removed `gate_all_enabled_phases_pass` recursion.

## [0.2.1] — 2026-06-30

### Added
- Phase 3: Security hardening — default `ShellPolicy::Disabled`, public bind guard (`validate_security()`) rejects `0.0.0.0` without auth, rejects `0.0.0.0` + unrestricted shell. 5 unit tests.
- Phase 6: Proxy pool health telemetry — `record_success`/`record_failure` wired in `forward.rs`, `/health` exposes `proxy_pool` schema with per-node stats, cooldown counts, `protected` flags. Auto-recovery from cooldown after `RECOVERY_SUCCESS_COUNT` (2) successes.
- Phase 7: Documentation rewrite — CLI-first `README.md`, `docs/cli.md`, `docs/health-status.md`, `docs/proxy-pool.md`, `docs/migration-from-start-sh.md`. 14 content verification gates.
- Phase 6: `stop.sh` cleanup for deprecated `warp-external` container (stale 40010).

### Changed
- Default shell policy: `unrestricted` → `disabled` (security).
- Dockerfile: `cargo build --release --locked` (reproducible builds).
- Release workflow: linux-arm64 target added, `--locked` on all build steps.

### Fixed
- `record_success` now transitions `Cooldown → Active` after threshold.
- System leakage tag stripping (`</think>`, `</parameter>`) from LLM outputs.

### Removed
- Deprecated static port 40010 container (`warp-external`).
- `#[allow(dead_code)]` from `record_success`, `record_failure`, `RECOVERY_SUCCESS_COUNT`.

## [0.2.0] — 2026-06-29

### Added
- Phase 4: 2-tier proxy architecture — Primary Managed Pool (40001–40003) + Warm-Standby Protected Pool (40004–40005). `is_protected_proxy_port()` guard.
- Phase 5: Primary-first Rendezvous routing policy. Sticky deterministic hash assignment, WarmStandby failover on selected-primary failure, affected-agent-only remap.
- Proxy CLI subcommands: `proxy status/ps`, `proxy restart`, `proxy purge`, `proxy logs`.
- Verification ecosystem: 8 phases, 60+ gates, `./scripts/verify.sh all --profile ci`.

## [0.1.0] — 2026-06-28

### Added
- Initial bridge implementation. Translates Anthropic Messages API → OpenAI Chat Completions format.
- CLI skeleton with subcommands (serve/start/status/stop/restart/env/proxy).
- Supervisor daemon with PID file tracking.
- Auth middleware with Bearer token validation.
- Web search interception with 5-provider fallback chain (Tavily → Exa → Serper → SearXNG → DuckDuckGo).
- Shell command interception (`!` prefix) with configurable policy.
- SSE streaming for both shell and upstream responses.
- Docker WARP SOCKS5 proxy automation.
- CI workflows (fmt → clippy → test → build).
- Release pipeline (cross-platform binaries, crates.io publish, ghcr.io Docker image).
