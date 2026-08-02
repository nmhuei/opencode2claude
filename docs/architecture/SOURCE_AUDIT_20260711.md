# Source Audit and Architecture Overhaul — 2026-07-11

## Scope

This audit covers the Rust source tree, HTTP transports, CLI lifecycle, configuration loading, upstream protocol mapping, streaming, web search, proxy routing, Docker/WARP operations, supervisor behavior, and the fast/integration test harness.

Work was performed on:

```text
branch: refactor/architecture-overhaul-20260711
baseline: f94c2b8
```

The goal was not to reduce line count at any cost. The goal was to establish explicit responsibility boundaries, remove duplicated policy, preserve public behavior, and fix defects proven by code inspection and regression tests.

## Before and after

| Metric | Baseline | Current |
|---|---:|---:|
| Rust source files | 36 | 95 |
| Rust source lines | 14,053 | 14,458 |
| Largest Rust file | 1,586 lines | 539 lines |
| Files above 1,000 lines | 3 | 0 |
| Full test result | existing baseline | 188 passed, 1 ignored; 81 fast tests passed; 18 heavy tests ignored |
| Clippy all targets | previously blocked by async mutex misuse in tests | clean with `-D warnings` |

The slight increase in total lines comes from extracting tests, introducing domain boundaries, adding regression tests, and replacing implicit behavior with named policy functions.

## Runtime architecture after overhaul

```text
cmd/opencode2api
    |
    v
app/                       CLI command dispatch and presentation
    |
    v
server/                    HTTP router composition and foreground lifecycle
    |
    +--> handlers/         Anthropic-compatible transport
    +--> dashboard/        Browser dashboard transport
    +--> rest_api.rs       Versioned management REST transport
             |
             v
management/                Shared management authentication and operations

handlers/
    |
    v
opencode/
    +--> mapper/           Anthropic -> OpenAI-compatible request mapping
    +--> forward/          synchronous and streaming forwarding
    +--> retry/            retry, model fallback, and egress policy
    +--> search/           search fallback chain and provider adapters
    +--> sanitize.rs       DSML/system-tag sanitation
    +--> types.rs          upstream wire types
             |
             v
proxy_pool/                sticky routing, health state, cooldown, restart state
    |
    +--> docker/           Docker/WARP lifecycle adapter
    +--> supervisor.rs     bridge process lifecycle
```

## Audit by subsystem

### Binary and CLI application

**Previous condition**

`src/main.rs` contained 1,586 lines and was included from the binary with a path attribute. It combined argument dispatch, daemon lifecycle, proxy operations, dashboard commands, environment output, log rendering, tables, and presentation helpers.

**Changes**

- Replaced the path-included module with a normal library module.
- Reduced `src/bin/opencode2api.rs` to a minimal entry point.
- Split command responsibilities into:
  - `src/app/server.rs`
  - `src/app/proxy.rs`
  - `src/app/dashboard.rs`
  - `src/app/utility.rs`
  - `src/app/view.rs`
- Kept command names and CLI behavior intact.

**Remaining concern**

`app/server.rs`, `app/proxy.rs`, and `app/view.rs` are still relatively large. They are now separated by responsibility, so further extraction can occur without touching unrelated commands.

### HTTP server composition

**Previous condition**

Route registration, body limits, state construction, startup logging, bind errors, and signal handling were in one function.

**Changes**

- Added `server/args.rs` for CLI-to-config mapping.
- Added `server/routes.rs` for route composition.
- Added `server/runtime.rs` for process lifecycle.
- Exposed `server::build_router` so integration tests exercise the production route tree instead of maintaining a duplicate router.

**Remaining concern**

`run_server` still terminates the process directly on fatal configuration/bind errors. A future library-quality API should return a typed error and let the binary decide the exit code.

### Anthropic HTTP handlers

**Previous condition**

`handlers.rs` mixed wire structs, prompt extraction, shell command interception, SSE generation, request orchestration, models, token counting, and health endpoints.

**Changes**

- `handlers/types.rs`: request wire types.
- `handlers/prompt.rs`: prompt and local-shell-result extraction.
- `handlers/shell.rs`: shell delegation protocol.
- `handlers/messages.rs`: `/v1/messages` orchestration.
- `handlers/metadata.rs`: models, health, and token count.
- `handlers/tests.rs`: focused handler tests.

**Behavior preserved**

The bridge still delegates `!command` execution to the client through a tool-use block. It does not execute the command inside the HTTP handler.

### Management API and dashboard

**Previous condition**

Dashboard and REST handlers independently implemented authentication, proxy snapshots, safe configuration output, credential redaction, and proxy restart checks. The implementations could drift and return different security behavior.

**Changes**

- Added `management/auth.rs`:
  - dashboard token lookup;
  - REST token fallback;
  - cookie/header extraction;
  - constant-time token comparison.
- Added `management/service.rs`:
  - uptime;
  - proxy snapshot;
  - safe configuration snapshot;
  - proxy URL credential redaction;
  - managed-proxy restart validation.
- Split the browser dashboard into assets, auth, config-file operations, event streaming, overview handlers, and time helpers.
- Kept dashboard response contracts and versioned REST endpoints intact.

**Remaining concerns**

- The OpenAPI document is handwritten and can drift from handler schemas.
- The dashboard config editor still writes TOML directly. A typed update command would provide stronger validation and clearer reload semantics.

### Configuration

**Previous condition**

`config.rs` mixed TOML schema, file reading, environment parsing, CLI precedence, defaults, shell policy construction, proxy parsing, and public-bind security checks.

**Changes**

- `config/file.rs`: TOML schema and parsing.
- `config/loader.rs`: deterministic precedence resolution.
- `config/security.rs`: public-bind security policy.
- `config/types.rs`: public resolved configuration types.
- `config/tests.rs`: precedence and security regression tests.

**Confirmed defect fixed**

An empty `BRIDGE_AUTH_TOKEN` is no longer considered enabled authentication.

**Remaining concerns**

- Several operational settings are still read directly from environment variables outside `BridgeConfig`, including rate limits, model fallback policy, active proxy count, and runtime paths.
- The upstream API URL is currently a constant in retry execution instead of a resolved configuration value.

### Protocol mapping

**Previous condition**

`mapper.rs` mixed model aliases, reasoning policy, system extraction, search helpers, tool-result handling, request conversion, and tests.

**Changes**

- `mapper/policy.rs`: reasoning/stream token policy.
- `mapper/helpers.rs`: field conversion helpers.
- `mapper/request.rs`: construction of the upstream request.
- `mapper/tests.rs`: mapping contract tests.

**Remaining concern**

Model aliases and fallback behavior remain application policy encoded in Rust. A typed model registry would make supported aliases and capabilities easier to inspect and configure.

### Synchronous and streaming forwarding

**Previous condition**

`forward.rs` was 1,520 lines and combined daemon checks, token estimation, search-history injection, synchronous execution, streaming state, SSE parsing, search interception, cancellation, and tests.

**Changes**

- `forward/common.rs`: shared health, token, and history helpers.
- `forward/sync.rs`: non-streaming execution.
- `forward/stream/context.rs`: mutable stream state and delta translation.
- `forward/stream/execute.rs`: upstream execution/search loop.
- `forward/stream/transport.rs`: channel timeout and disconnect cancellation.
- `forward/stream/tests.rs`: SSE state-machine regression tests.

**Behavior preserved**

- Incremental SSE delivery.
- Thinking and text block ordering.
- Tool-use deltas.
- Search interception loop.
- Final partial line processing.
- Error-path stream finalization.

**Remaining concerns**

- `StreamContext` is still the largest runtime file. It now has one responsibility, but the DSML and native-tool-call paths could later become separate parsers.
- Token estimation remains heuristic.

### Retry and model fallback

**Previous condition**

One file combined host WARP commands, body classification, fallback model construction, route selection, HTTP retry behavior, proxy cooldown, network-failure tracking, and tests.

**Changes**

- `retry/policy.rs`: fallback and rate-limit classification.
- `retry/warp.rs`: host WARP reconnect adapter.
- `retry/execute.rs`: request execution and egress-aware retries.
- `retry/tests.rs`: policy tests.

**Confirmed defects fixed**

1. HTTP 5xx responses no longer penalize a healthy proxy. Receiving an HTTP response proves the proxy transport worked.
2. Non-rate-limit HTTP 400 responses no longer cooldown or rotate egress.
3. Error text now reports the actual provider retry count instead of saying ten retries when the constant was one.
4. Rate-limit body detection no longer classifies every occurrence of generic words such as `limit` as an IP rate limit.
5. When a proxy pool is configured but no eligible proxy remains, the bridge fails closed instead of silently sending the request through the direct host IP.
6. Host WARP reconnect logging no longer claims the public IP changed without verification.

### Search subsystem

**Previous condition**

One file contained the fallback orchestrator, five providers, HTML parsing, URL encoding/decoding, text sanitation, and tests.

**Changes**

- `search/client.rs`: provider fallback order.
- `search/providers/*.rs`: one adapter per provider.
- `search/util.rs`: encoding and text helpers.
- `search/types.rs`: domain types.
- `search/tests.rs`: helper and provider-client tests.

**Confirmed defects fixed**

1. Exa snippet truncation no longer slices UTF-8 strings at arbitrary byte offsets.
2. Percent-decoded Vietnamese text now reconstructs bytes before UTF-8 decoding instead of converting each escaped byte directly into a character.

Regression tests cover `Tiếng Việt` decoding and 300-character truncation with multibyte characters.

### Proxy pool

**Previous condition**

`proxy_pool/mod.rs` mixed pool construction, snapshots, degraded selection, exports, and nearly 400 lines of tests.

**Changes**

- `proxy_pool/pool.rs`: construction, snapshot, and tier aggregation.
- `proxy_pool/routing.rs`: sticky selection and retry exclusion.
- `proxy_pool/maintenance.rs`: health/cooldown/restart state.
- `proxy_pool/tests.rs`: routing and health tests.

**Confirmed defects fixed**

1. `get_client_excluding` previously accepted an excluded index but ignored it. Retry now cannot select the same failed proxy and tries another healthy primary before standby.
2. Restart attempts were lost when the state changed from `Dead { restart_attempts }` to `Starting`, potentially causing an endless sequence of “attempt 1.” The attempt number is now carried across the restart operation and stops after the third failure.

**Remaining concerns**

- `ProxyStatus` still mixes health state and serving role (`Active`, `Spare`, `Cooldown`, `Dead`, `Starting`). The target design should separate `role`, `health`, and circuit state.
- Exit IP identity is not verified or deduplicated.
- Routing uses Rust's default hasher. Mapping is deterministic for the running build but is not an explicit cross-version compatibility contract.
- Background tasks run forever without a shared cancellation token.

### Docker/WARP adapter

**Previous condition**

`docker.rs` mixed errors, port policy, command execution, container state, verification, bulk stop, and interactive bootstrap.

**Changes**

- `docker/types.rs`: errors and destructive-operation safety.
- `docker/lifecycle.rs`: Docker process calls and container state.
- `docker/health.rs`: proxy verification and bulk stop.
- `docker/bootstrap.rs`: interactive bootstrap.

**Remaining concerns**

- Container creation logic is still duplicated between `docker/lifecycle.rs` and `proxy_pool/maintenance.rs`.
- Docker commands are concrete process calls, which makes unit testing lifecycle behavior difficult.
- WARP image names, ports, and command arguments remain hardcoded.

### Supervisor and runtime

**Confirmed defect fixed**

The supervisor uses `/proc/<pid>` to detect process existence and validates the signed Linux `pid_t` range so very large unsigned values cannot be interpreted as process-group selectors. This matches the repository's Linux-only support scope.

Regression tests cover the current PID and an impossible PID.

**Remaining concerns**

- Stop signaling still assumes Unix `kill` semantics in the main implementation.
- The health probe is a minimal raw HTTP implementation and only checks HTTP 200.
- PID files cannot prove PID ownership if a PID is reused; process identity metadata would improve safety.

### Test architecture

**Previous condition**

`tests/fast.rs` copied the complete route tree and held a `std::sync::MutexGuard` across async waits. This produced Clippy failures and could block the Tokio executor.

**Changes**

- Fast tests now call the production `server::build_router`.
- Environment serialization uses `tokio::sync::Mutex` in async tests.
- `cargo test --all-targets` now passes with the default parallel harness.
- `cargo clippy --all-targets -- -D warnings` now passes.

**Remaining concerns**

- Eighteen heavy integration tests remain ignored by default.
- Several test modules mutate process-wide environment variables. They are serialized within each test binary, but an injected configuration source would be cleaner.
- Live search coverage is intentionally ignored because it depends on external network availability.

## Confirmed bug fixes and evidence

| Defect | Regression evidence |
|---|---|
| Retry selected excluded proxy | `test_retry_excludes_failed_proxy_and_prefers_other_primary` |
| Restart attempts reset in `Starting` state | `restart_failure_preserves_attempts_and_stops_after_third_try` |
| UTF-8 Exa snippet panic | `test_truncate_chars_is_utf8_safe` |
| Incorrect UTF-8 percent decode | `test_url_decode_utf8` |
| Direct-IP fallback when proxy pool unavailable | `configured_proxy_pool_never_silently_falls_back_to_direct` |
| Broad rate-limit body matching | `rate_limit_classifier_does_not_match_generic_bad_request_text` |
| Linux-only process probe | `process_probe_detects_current_process`, `process_probe_rejects_impossible_pid` |
| Async test mutex blocked executor/Clippy | full all-target Clippy and default-parallel test run |

## Remaining work by priority

### P1 — before treating proxy routing as production-grade

1. Separate proxy role, health, circuit state, and lifecycle policy into independent fields.
2. Add verified exit identity and duplicate-IP suppression.
3. Move all Docker lifecycle calls behind one adapter; remove duplicate container creation.
4. Add cancellation/shutdown to proxy health and restart workers.
5. Add an integration test with a real SOCKS server or controlled proxy fixture.

### P2 — reliability and operability

1. Move upstream URL, WARP image, active count, retry policy, and rate limits into typed configuration.
2. Replace handwritten OpenAPI with schemas generated from shared response/request types.
3. Add explicit `/health/live` and `/health/ready` semantics while preserving `/health` compatibility.
4. Return errors from server runtime instead of calling `process::exit` in library code.
5. Replace direct environment access in application/view modules with resolved config snapshots.

### P3 — maintainability

1. Split DSML parsing from native OpenAI tool-call handling inside `StreamContext`.
2. Extract command-runner traits for Docker, WARP, update, and supervisor tests.
3. Move presentation-only tests away from runtime modules.
4. Define a stable rendezvous hash implementation if cross-version session affinity matters.

## Validation commands

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --bins
```

The branch preserves existing public routes and CLI commands while replacing the major monolithic files with explicit modules and adding regression tests for every functional defect fixed during the audit.
