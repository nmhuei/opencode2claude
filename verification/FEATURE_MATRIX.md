# Repository Feature Matrix

This matrix is the authoritative inventory of public contracts. `status` is one of `implemented`, `partial`, `blocked`, or `verified`. A release candidate requires every mandatory row to be `verified`.

| id | feature | public contract | implementation module | unit test | integration/system test | documentation | mandatory | status |
|---|---|---|---|---|---|---|---|---|
| CLI-001 | Default foreground server | `opencode2api` starts the bridge in foreground | `src/app/mod.rs`, `src/server/runtime.rs` | server/config tests | process smoke | README, CLI guide | yes | verified |
| CLI-002 | Server start | `server start` daemonizes; `-f` stays foreground | `src/app/server.rs`, `src/supervisor.rs` | supervisor tests | `tests/cli_e2e.sh` | `docs/cli.md` | yes | verified |
| CLI-003 | Server stop | `server stop` stops only the managed bridge | `src/app/server.rs`, `src/supervisor.rs` | supervisor tests | CLI E2E | CLI guide | yes | verified |
| CLI-004 | Server status | `server status` reports managed/unmanaged/stopped | `src/app/server.rs`, `src/supervisor.rs` | supervisor tests | CLI E2E | CLI guide | yes | verified |
| CLI-005 | Server restart | `server restart` performs bounded stop/start | `src/app/server.rs` | command tests | CLI E2E | CLI guide | yes | verified |
| CLI-006 | Server logs | `server logs` reads daemon logs | `src/app/server.rs` | output tests | CLI E2E | CLI guide | yes | verified |
| CLI-007 | Server config | `server config` prints safe resolved config | `src/app/server.rs`, `src/management/service.rs` | redaction tests | CLI E2E | config reference | yes | verified |
| CLI-008 | Proxy list | `proxy ps` reports node roles and state | `src/app/proxy.rs` | proxy snapshot tests | fake/real runtime tests | proxy guide | yes | verified |
| CLI-009 | Proxy restart | `proxy restart` restarts managed primaries only | `src/app/proxy.rs`, `src/docker/` | protection tests | fake/real runtime tests | proxy guide | yes | verified |
| CLI-010 | Proxy purge | `proxy purge --yes` recreates managed primaries only | `src/app/proxy.rs`, `src/docker/` | protection tests | fake/real runtime tests | proxy guide | yes | verified |
| CLI-011 | Proxy logs | `proxy logs` reads known container logs | `src/app/proxy.rs`, `src/docker/` | validation tests | fake runtime tests | proxy guide | yes | verified |
| CLI-012 | Dashboard start | `dashboard start` prints/opens service URL | `src/app/dashboard.rs` | output tests | CLI E2E | dashboard docs | yes | verified |
| CLI-013 | Dashboard status | `dashboard status` reports availability/auth | `src/app/dashboard.rs` | output tests | CLI E2E | dashboard docs | yes | verified |
| CLI-014 | Environment output | `env` emits Claude Code variables | `src/app/utility.rs` | output tests | CLI E2E | CLI guide | yes | verified |
| CLI-015 | Doctor | `doctor` diagnoses config/runtime/dependencies | `src/doctor.rs` | doctor tests | CLI E2E | troubleshooting | yes | verified |
| CLI-016 | Completion | `completion <shell>` emits shell completion | `src/app/utility.rs` | clap tests | CLI E2E | CLI guide | yes | verified |
| CLI-017 | Init | `init` creates default config safely | `src/init.rs` | init tests | disposable FS test | config reference | yes | verified |
| CLI-018 | Update check/apply | `update --check/--force` verifies and atomically updates | `src/update.rs` | update tests | update/rollback fixture | release guide | yes | verified |
| CLI-019 | Legacy aliases | hidden serve/start/status/stop/restart/logs remain compatible | `src/app/mod.rs` | clap tests | CLI E2E | migration guide | yes | verified |
| API-001 | Anthropic messages sync | `POST /v1/messages` returns Anthropic-compatible JSON | `src/handlers/messages.rs`, `src/opencode/forward/sync.rs` | mapper/forward tests | fake upstream HTTP | compatibility docs | yes | verified |
| API-002 | Anthropic messages stream | `POST /v1/messages` with `stream=true` emits valid Anthropic SSE | `src/opencode/forward/stream/` | stream state tests | fake upstream SSE | compatibility docs | yes | verified |
| API-003 | Token count | `POST /v1/messages/count_tokens` returns explicit estimate | `src/handlers/metadata.rs` | token tests | router test | compatibility docs | yes | verified |
| API-004 | Models | `GET /v1/models` reports supported/default models | `src/handlers/metadata.rs` | handler tests | router test | model docs | yes | verified |
| API-005 | Client authentication | configured bearer tokens protect `/v1/*` except health | `src/middleware.rs` | middleware tests | router tests | security guide | yes | verified |
| API-006 | Body/content limits | request bodies and stream buffers are bounded | server/stream modules | limit tests | router/SSE fixtures | security guide | yes | verified |
| API-007 | Shell delegation | `!command` becomes client tool-use; disabled by default | `src/handlers/shell.rs`, `src/shell.rs` | shell policy tests | protocol E2E | security/compatibility docs | yes | verified |
| API-008 | Native tools | Anthropic tools map to native upstream tool calls and back | mapper/forward modules | mapper/stream tests | fake upstream tool fixture | compatibility docs | yes | verified |
| API-009 | DSML tools | DSML tool-call text is parsed with bounded buffers | sanitize/stream modules | parser tests | fragmented SSE fixture | compatibility docs | yes | verified |
| API-010 | Reasoning stream | reasoning deltas preserve Anthropic block ordering | stream context/tracker | stream tests | fake upstream SSE | compatibility docs | yes | verified |
| API-011 | Search interception | configured providers are tried in documented order | `src/opencode/search/` | provider/policy tests | local provider fixtures | search docs | yes | verified |
| API-012 | Model fallback | configured fallback preserves request capabilities | `src/opencode/retry/` | retry policy tests | fake upstream fixture | model/retry docs | yes | verified |
| API-013 | Retry/failover | transport/rate/provider failures use bounded typed policy | retry/egress modules | policy tests | fake upstream/SOCKS | retry docs | yes | verified |
| API-014 | Cancellation/backpressure | client disconnect cancels upstream work; sends are bounded | stream transport/execution | stream tests | disconnect test | operations docs | yes | verified |
| HLT-001 | Compatibility health | `GET /health` remains minimal and public | `src/handlers/metadata.rs` | handler tests | router test | health guide | yes | verified |
| HLT-002 | Liveness | `GET /health/live` reflects process/event-loop life only | health module | health tests | router test | health guide | yes | verified |
| HLT-003 | Readiness | `GET /health/ready` reflects workers and permitted egress | health/egress modules | readiness tests | fault fixtures | health guide | yes | verified |
| HLT-004 | Authenticated diagnostics | diagnostics explain readiness without leaking secrets | management/dashboard/REST | redaction tests | router tests | health/security guide | yes | verified |
| OBS-001 | Structured request logs | requests have correlation IDs and secret-safe fields | `src/observability.rs` | request-ID and strict field-whitelist tests | middleware echo/counter test | `docs/observability.md` | yes | verified |
| OBS-002 | Metrics | authenticated or configurable metrics expose bounded counters | `src/observability.rs` | counter terminal-semantics tests | REST/protocol/provider fixtures | `docs/observability.md` | yes | verified |
| MGT-001 | REST status | `GET /api/v1/status` requires Bearer auth | `src/rest_api.rs` | REST tests | router test | OpenAPI | yes | verified |
| MGT-002 | REST proxies | `GET /api/v1/proxies` returns typed redacted snapshot | REST/management | snapshot tests | router test | OpenAPI | yes | verified |
| MGT-003 | REST config | `GET /api/v1/config` returns safe resolved config | REST/management | redaction tests | router test | OpenAPI | yes | verified |
| MGT-004 | REST restart | `POST /api/v1/proxies/:port/restart` protects standby | REST/management/runtime | protection tests | fake runtime test | OpenAPI | yes | verified |
| MGT-005 | OpenAPI | `/api/v1/openapi.json` is generated from shared schemas | REST DTO/schema | schema tests | runtime schema/router test | `docs/management-api.md` | yes | verified |
| MGT-006 | Typed config apply | validate/preview/atomic apply/rollback | `src/management/config_apply.rs` | merge/validation/rollback tests | disposable filesystem fixture | `docs/management-api.md` | yes | verified |
| MGT-007 | Browser CSRF | cookie-authenticated mutations require CSRF token | `src/dashboard/auth.rs` | double-submit/header tests | dashboard router tests | `docs/security.md` | yes | verified |
| MGT-008 | Audit events | management mutations emit secret-free audit events | `src/audit.rs`, REST/dashboard mutations | bounded/redaction tests | authenticated router test | `docs/management-api.md` | yes | verified |
| DASH-001 | Landing/dashboard assets | `/`, `/dashboard*` serve embedded UI safely | dashboard assets | asset tests | router tests | dashboard docs | yes | verified |
| DASH-002 | Login/logout/auth status | dashboard cookie auth works and fails closed | dashboard auth | auth tests | router tests | security guide | yes | verified |
| DASH-003 | Status/proxy/config views | dashboard uses shared management policy | dashboard/management | service tests | router tests | dashboard docs | yes | verified |
| DASH-004 | Event stream/test stream | dashboard SSE is bounded and authenticated | dashboard events | event tests | router tests | dashboard docs | yes | verified |
| CFG-001 | Precedence | defaults < TOML < env < CLI | config loader | precedence tests | CLI smoke | config reference | yes | verified |
| CFG-002 | Semantic validation | invalid auth/bind/egress combinations fail before bind | config security | security tests | process smoke | config reference | yes | verified |
| CFG-003 | Secret redaction | secrets redact in Debug/display/snapshots/logs | config/management | redaction tests | log/REST tests | security guide | yes | verified |
| CFG-004 | Legacy aliases/migration | old env/config names receive compatible migration | config loader | migration tests | CLI test | migration guide | yes | verified |
| EGR-001 | Typed egress nodes | role/health/circuit/lifecycle are independent | egress domain | transition tests | fake runtime | proxy guide | yes | verified |
| EGR-002 | Sticky routing | stable explicit rendezvous hash assigns healthy primaries | egress routing | routing tests | fixture | proxy guide | yes | verified |
| EGR-003 | Standby policy | standby is used only after eligible primary exhaustion | egress routing | routing tests | failover fixture | proxy guide | yes | verified |
| EGR-004 | Retry exclusion | failed/circuit-open nodes are excluded | egress/retry | regression tests | failover fixture | retry guide | yes | verified |
| EGR-005 | Leases | active requests prevent destructive node operations | egress domain | lease tests | fake runtime | proxy guide | yes | verified |
| EGR-006 | Exit identity | nodes are verified through configurable identity probes | egress identity | probe tests | real WARP system | proxy guide | yes | verified |
| EGR-007 | Duplicate suppression | duplicate exits do not count as independent capacity | egress identity/routing | identity tests | real WARP system | proxy guide | yes | verified |
| EGR-008 | No direct leak | configured proxy mode fails closed | retry/egress | regression test | SOCKS fixture | security/proxy docs | yes | verified |
| EGR-009 | Circuit breaker | open/half-open/closed transitions are bounded | egress circuit | table tests | fault fixture | retry guide | yes | verified |
| EGR-010 | Worker lifecycle | health/restart workers cancel, join and report health | `src/workers.rs`, proxy workers | cancel/join/panic/readiness tests | shutdown and Tier C soak | `docs/observability.md` | yes | verified |
| INF-001 | Container runtime adapter | one canonical Docker/WARP spec and injectable runtime | infrastructure/docker | adapter tests | fake/real runtime | deployment guide | yes | verified |
| INF-002 | Process manager | spawn/probe/terminate validates process identity | infrastructure/supervisor | process tests | Linux lifecycle fixture PASS | `docs/deployment.md` | yes | verified |
| INF-003 | Atomic file store | config/runtime/update writes are atomic and permissioned | infrastructure/files | file tests | disposable FS | security/upgrade docs | yes | verified |
| INS-001 | Install/uninstall | scripts work in disposable prefix with no leftovers | install scripts | ShellCheck plus transaction assertions | `tests/install_e2e.sh` | README, upgrade guide | yes | verified |
| INS-002 | Update/rollback | checksum-verified atomic update restores prior binary on failure | update module | update tests | candidate/checksum/rollback fixtures | `docs/upgrade-rollback.md` | yes | verified |
| INS-003 | Config migrations | schema version migrations preserve user settings | `src/config/migration.rs` | future/conflict/idempotence tests | config apply/CLI fixtures | `docs/upgrade-rollback.md` | yes | verified |
| SEC-001 | Public-bind fail-closed | strong auth is required for non-loopback binding | config security | security tests | process smoke | security guide | yes | verified |
| SEC-002 | Secret scanning | repository and generated evidence pass secret scan | `scripts/check_secrets.py` | scanner synthetic self-test | tracked-file scan | `docs/security.md` | yes | verified |
| SEC-003 | Dependency/license policy | audit and deny pass | `deny.toml`, Tier B | n/a | local Tier B; protected CI pending only for tier row | security/release docs | yes | verified |
| SEC-004 | Parser fuzzing | JSON/SSE/DSML/search/config parsers have fuzz smoke | `tests/parser_fuzz_smoke.rs` | 2,256 deterministic corpus cases | debug and release test runs | `docs/testing.md` | yes | verified |
| REL-001 | Tier A tests | deterministic per-commit test tier is mandatory | `scripts/tier-a.sh`, Linux CI | n/a | local Tier A PASS; Linux workflow configured | `docs/testing.md` | yes | verified |
| REL-002 | Tier B tests | protected Docker/install/OS/schema/security tier | `scripts/tier-b.sh`, Linux CI | n/a | local `scripts/tier-b.sh` PASS; Linux workflow configured | `docs/testing.md` | yes | verified |
| REL-003 | Tier C tests | real WARP, external canary, soak and release smoke | `scripts/tier-c.sh`, `system.yml` | n/a | real WARP, duplicate suppression, soak and external canary PASS | testing/release docs | yes | verified |
| REL-004 | Release artifacts | checksums, SBOM, provenance and clean install smoke | `.github/workflows/release.yml` | n/a | actionlint + local checksum/SPDX/install bundle PASS; publish workflow configured | `docs/release-checklist.md` | yes | verified |
| DOC-001 | Documentation set | required architecture/config/CLI/API/security/ops guides match code | `docs/`, `scripts/check_docs.py` | required-doc/env coverage checker | docs-check PASS | README index | yes | verified |
