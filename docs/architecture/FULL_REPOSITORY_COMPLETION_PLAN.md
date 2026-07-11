# Full Repository Completion Plan

**Repository:** `opencode2api`  
**Execution branch:** `refactor/architecture-overhaul-20260711`  
**Plan created:** 2026-07-11  
**Completion policy:** The repository is **not complete** until every mandatory phase and every final acceptance gate in this document passes with recorded evidence.

---

## 1. Mission

Complete the repository as a production-grade, testable, secure, cross-platform bridge that:

- exposes a stable Anthropic-compatible API;
- forwards synchronous and streaming requests correctly;
- supports tool use, local shell delegation, web-search interception, model fallback, and reasoning streams;
- routes traffic through a verified, observable egress pool without silent direct-IP leakage;
- manages Docker/WARP lifecycle safely;
- provides CLI, daemon, dashboard, REST management API, health/readiness, diagnostics, update, installation, and release workflows;
- is fully covered by deterministic unit, integration, system, and release tests;
- has no undocumented or unverified feature path.

The architecture refactor already completed is the foundation. It is not the final completion state.

---

## 2. Non-negotiable definition of complete

The project may be declared complete only when all conditions below are true.

### 2.1 Functional completeness

Every public command, HTTP route, dashboard action, retry path, proxy transition, installer path, updater path, and shutdown path has:

1. a written contract;
2. an implementation;
3. at least one success test;
4. at least one relevant failure test;
5. smoke or integration evidence where external processes are involved.

### 2.2 Test completeness

- Zero unexplained `#[ignore]` tests.
- Network-dependent tests are replaced with deterministic local fixtures where possible.
- Real Docker/WARP tests remain separately tagged but run in mandatory protected CI or scheduled CI.
- Linux and macOS release behavior is exercised.
- Windows behavior is either fully supported and tested or explicitly removed from advertised support.

### 2.3 Security completeness

- No secrets appear in logs, health responses, diagnostics, dashboard payloads, panic messages, or config snapshots.
- Public binding fails closed without strong authentication.
- Proxy-configured mode never silently falls back to direct egress.
- Shell delegation remains disabled by default and policy constrained when enabled.
- Management operations enforce one shared authorization policy.
- Dependency, license, shell, and secret scans pass.

### 2.4 Operational completeness

- Liveness and readiness are distinct.
- Every background worker can be cancelled and joined.
- Shutdown is graceful and bounded.
- PID ownership is verified before termination.
- Docker/WARP resources are reconciled rather than blindly recreated.
- Upgrade and rollback paths are tested.

### 2.5 Documentation completeness

The README, CLI help, config reference, REST/OpenAPI contract, deployment guide, security guide, migration guide, troubleshooting guide, and release guide all match the running implementation.

---

## 3. Current verified baseline

The current branch already provides:

- monolith split into responsibility-focused modules;
- shared dashboard/REST management authentication and service logic;
- production router reused by integration tests;
- retry exclusion of failed proxy nodes;
- fail-closed behavior when configured proxy egress is unavailable;
- bounded SSE channel sends and disconnect cancellation;
- UTF-8-safe search parsing;
- restart-attempt preservation;
- Linux/macOS-compatible process existence probing;
- clean format, check, Clippy, unit/fast tests, binary build, and process smoke tests.

Current unresolved inventory found directly in source:

- 19 ignored tests: 18 integration tests plus one live search test;
- 28 direct environment reads outside the config loader boundary;
- direct process calls distributed across supervisor, Docker, WARP, and proxy maintenance;
- duplicate Docker container creation logic;
- proxy role, health, serving state, circuit state, and lifecycle still partially conflated;
- no verified exit-IP identity or duplicate-IP suppression;
- no shared cancellation/join lifecycle for proxy workers;
- handwritten OpenAPI contract;
- incomplete cross-platform process termination semantics;
- library runtime still exits the process on some failures;
- upstream, retry, Docker image, active proxy count, and several policies remain hardcoded or read globally.

---

# 4. Execution roadmap

The roadmap is ordered by dependency. Later phases must not bypass unfinished acceptance criteria from earlier phases.

---

## Phase 0 — Freeze contracts and build the completion matrix

### Goal

Define exactly what the repository claims to support before changing more runtime behavior.

### Work

1. Inventory every CLI command and option from `clap`.
2. Inventory every HTTP route and method from the production router.
3. Inventory every dashboard action.
4. Inventory every config field, environment variable, default, and precedence rule.
5. Inventory every external dependency:
   - upstream LLM endpoint;
   - Docker;
   - WARP container image;
   - `warp-cli`;
   - search providers;
   - filesystem/runtime directories;
   - update/release hosting.
6. Create `verification/FEATURE_MATRIX.md` with columns:
   - feature;
   - public contract;
   - implementation module;
   - unit test;
   - integration test;
   - system test;
   - documentation;
   - status.
7. Convert all remaining work into linked issue IDs or checklist IDs.

### Acceptance

- Every public feature appears exactly once in the matrix.
- No route or CLI command exists without an owner and test target.
- The matrix is checked in CI for missing mandatory columns.

---

## Phase 1 — Consolidate all runtime configuration

### Goal

Make one immutable resolved configuration tree the only runtime policy source.

### Target model

```rust
struct AppConfig {
    server: ServerConfig,
    upstream: UpstreamConfig,
    auth: AuthConfig,
    streaming: StreamingConfig,
    retry: RetryConfig,
    egress: EgressConfig,
    search: SearchConfig,
    runtime: RuntimeConfig,
    observability: ObservabilityConfig,
}
```

### Work

1. Move all direct environment reads into config/bootstrap code, including:
   - active proxy count;
   - rate limit;
   - minimum reasoning stream tokens;
   - model fallback list and enable flag;
   - dashboard/config path;
   - runtime path;
   - CLI view/status values;
   - upstream base URL;
   - Docker binary and WARP image;
   - retry counts and intervals;
   - worker intervals;
   - exit identity requirements.
2. Replace environment reads in `app/*`, `state.rs`, mapper, retry, proxy pool, dashboard, doctor, and management auth with injected config.
3. Define deterministic precedence:
   `defaults < config file < environment < CLI`.
4. Add semantic validation, not only parsing validation.
5. Add secret wrapper types whose `Debug` and `Display` redact values.
6. Add config dump commands for safe and explicit-secret modes.
7. Add migration aliases for old environment variable names.

### Acceptance

- Zero operational `std::env::var` calls outside bootstrap/config/runtime-path code.
- Full precedence test matrix passes.
- Invalid combinations fail before binding the HTTP socket.
- Safe config serialization proves secrets are redacted.
- Existing config files remain backward compatible or receive an automated migration error with exact guidance.

---

## Phase 2 — Redesign the egress domain model

### Goal

Replace the overloaded proxy state model with independently testable dimensions.

### Target types

```rust
struct EgressNode {
    id: NodeId,
    endpoint: ProxyEndpoint,
    role: EgressRole,
    health: HealthState,
    circuit: CircuitState,
    lifecycle: LifecyclePolicy,
    exit_identity: Option<ExitIdentity>,
    active_requests: usize,
    consecutive_failures: u32,
    consecutive_successes: u32,
    cooldown_until: Option<Instant>,
    restart_attempts: u32,
}
```

### Work

1. Introduce typed node IDs and validated proxy endpoints.
2. Separate:
   - role: primary/standby;
   - health: unknown/healthy/degraded/unhealthy/recovering;
   - circuit: closed/open/half-open;
   - lifecycle: managed/protected/external;
   - serving eligibility.
3. Define a written state transition table.
4. Add active-request leases so a node is not destroyed while in use.
5. Add stable rendezvous hashing using an explicit algorithm and seed.
6. Preserve sticky routing across versions where node set is unchanged.
7. Define failover ordering and recovery probing.
8. Remove the legacy `ProxyStatus` only after compatibility tests pass.

### Acceptance

- Every state transition is covered by a table-driven test.
- Retry never selects an excluded or circuit-open node.
- Primary routing remains sticky.
- Standby is not used during healthy primary operation.
- Recovery does not cause a thundering herd.
- No lifecycle operation runs against protected nodes.

---

## Phase 3 — Verify exit identity and suppress duplicate egress

### Goal

Prove that configured proxy nodes provide the intended independent public egress identities.

### Work

1. Add `ExitIdentityProbe` abstraction.
2. Probe through each proxy using at least two configurable identity endpoints.
3. Record:
   - public IPv4/IPv6;
   - provider/ASN when available;
   - probe timestamp;
   - confidence/source agreement.
4. Detect duplicate public identities.
5. Mark duplicates as non-independent and exclude extras from normal routing.
6. Add TTL and re-verification after restart or reconnect.
7. Expose redacted identity state in management diagnostics.
8. Add strict mode: readiness fails when unique verified exits are below the configured minimum.
9. Add controlled fixture tests and real WARP system tests.

### Acceptance

- No node becomes production-ready before identity policy is satisfied.
- Duplicate exits cannot be counted as independent capacity.
- Restarted nodes are re-probed before serving.
- Readiness accurately reports insufficient unique egress.

---

## Phase 4 — Create one infrastructure adapter layer

### Goal

Remove direct OS/Docker/WARP calls from domain and transport code.

### Interfaces

```rust
trait CommandRunner { /* bounded command execution */ }
trait ContainerRuntime { /* create, remove, start, stop, inspect, logs */ }
trait WarpController { /* connect, disconnect, status */ }
trait ProcessManager { /* spawn, probe, terminate, identity */ }
trait FileStore { /* atomic runtime/config writes */ }
```

### Work

1. Consolidate all Docker container specification generation in one module.
2. Remove duplicate Docker creation from proxy maintenance.
3. Add typed command output and timeout handling.
4. Add idempotent reconcile operations:
   - desired spec vs actual container;
   - create/start/migrate/recreate only when required.
5. Make WARP host rotation optional and explicit; do not mix it with container egress policy.
6. Inject fake adapters into unit/integration tests.
7. Add platform-specific process manager implementations.
8. Eliminate production `unsafe` where possible; otherwise isolate and document invariants.

### Acceptance

- Proxy domain contains no `Command::new`.
- HTTP handlers contain no OS/container calls.
- One canonical container spec is used by bootstrap and restart.
- Adapter unit tests cover command failures, timeouts, malformed output, and partial state.
- Fake runtime tests cover all lifecycle branches without Docker.

---

## Phase 5 — Finish worker lifecycle and graceful shutdown

### Goal

Ensure every spawned task has ownership, cancellation, health reporting, and bounded shutdown.

### Work

1. Introduce application-level `CancellationToken`.
2. Register every worker:
   - proxy health monitor;
   - restart queue processor;
   - dashboard heartbeat;
   - SSE request tasks;
   - any update/maintenance jobs.
3. Keep join handles in a worker registry.
4. Report worker health and last failure.
5. On shutdown:
   - stop accepting new requests;
   - cancel workers;
   - drain or cancel active streams according to policy;
   - wait with timeout;
   - persist final state;
   - return an error if cleanup is incomplete.
6. Make server runtime return typed errors rather than calling `process::exit`.
7. Let only binary entry points choose exit codes.

### Acceptance

- No untracked infinite task remains.
- Shutdown integration test completes within configured timeout.
- Client disconnect cancels upstream stream work.
- Worker panic/failure changes readiness.
- Library modules do not terminate the process.

---

## Phase 6 — Complete upstream protocol correctness

### Goal

Make Anthropic-to-upstream mapping and streaming behavior fully contractual.

### Work

1. Define supported Anthropic request fields and rejection behavior for unsupported fields.
2. Add typed model capability registry:
   - tools;
   - reasoning;
   - streaming;
   - images/content types;
   - token behavior;
   - aliases.
3. Split native tool-call parser and DSML parser.
4. Bound every streaming buffer and define overflow behavior.
5. Replace heuristic token counting where a reliable tokenizer is available; otherwise expose estimates explicitly.
6. Cover fragmented UTF-8, fragmented JSON, multiple tool calls, interleaved reasoning/text, malformed SSE, premature EOF, duplicate `[DONE]`, search loops, cancellation, and backpressure.
7. Add fake upstream servers for sync and SSE.
8. Verify response headers, status mapping, and Anthropic-compatible error bodies.

### Acceptance

- Protocol conformance test suite passes for sync and stream.
- No malformed upstream input causes panic or unbounded allocation.
- Every emitted SSE sequence satisfies Anthropic block ordering.
- Search interception cannot exceed configured loop or memory bounds.
- Tool-use and tool-result round trips are deterministic.

---

## Phase 7 — Complete retry, fallback, and rate-control policy

### Goal

Make retry behavior predictable, observable, and free from cross-layer side effects.

### Work

1. Define typed failure classes:
   - transport;
   - timeout;
   - rate limit;
   - provider 4xx;
   - provider 5xx;
   - malformed response;
   - cancellation.
2. Define retry budgets per request and per node.
3. Parse numeric and HTTP-date `Retry-After`.
4. Add bounded jittered backoff.
5. Add circuit breaker thresholds and half-open probes.
6. Separate model fallback from egress retry budgets.
7. Preserve reasoning/tool capability when selecting fallback models.
8. Add global and per-key concurrency/rate limits.
9. Export retry counters and final failure reason.

### Acceptance

- Table-driven tests cover every failure class.
- Provider errors do not penalize healthy transport.
- Network errors do not consume model fallback unnecessarily.
- Retry budgets cannot loop indefinitely.
- Cancellation interrupts backoff immediately.

---

## Phase 8 — Complete search subsystem

### Goal

Make search providers typed, deterministic, secure, and testable without public internet.

### Work

1. Define typed `SearchQuery`, `SearchResult`, and `SearchError`.
2. Provider adapters return structured data instead of preformatted strings.
3. Central formatter converts structured results into model context.
4. Validate provider URLs and prevent unsafe internal-network access for configurable SearXNG unless explicitly allowed.
5. Add timeouts, result limits, content limits, and HTML sanitization.
6. Add local mock servers for Tavily, Exa, Serper, SearXNG, and DuckDuckGo HTML.
7. Remove the ignored live search test from the mandatory suite; retain a separate scheduled external canary.
8. Add caching only if policy and privacy requirements are explicit.

### Acceptance

- All five providers have deterministic fixture tests.
- Fallback order and error propagation are proven.
- UTF-8, malformed JSON, malformed HTML, timeout, and oversized response cases pass.
- Search result injection contains source URLs and bounded content.

---

## Phase 9 — Complete management API, dashboard, and OpenAPI

### Goal

Use one typed management contract for REST and dashboard behavior.

### Work

1. Define shared request/response DTOs.
2. Generate OpenAPI from the same schema types used by handlers.
3. Version every management endpoint.
4. Make the dashboard consume the versioned management API where practical.
5. Replace raw TOML write endpoint with typed validate/apply workflow:
   - validate;
   - preview diff;
   - atomically save;
   - reload/restart requirement;
   - rollback on failure.
6. Add CSRF strategy for cookie-authenticated browser mutations.
7. Add audit events for config changes and proxy actions without recording secrets.
8. Add pagination/filtering for logs/events if unbounded data is possible.

### Acceptance

- Generated OpenAPI validates all management responses in tests.
- Dashboard and REST cannot drift on authorization or proxy policy.
- Config apply is atomic and rollback-tested.
- Browser mutation routes pass CSRF tests.
- No management response exposes credentials.

---

## Phase 10 — Health, readiness, diagnostics, and observability

### Goal

Provide truthful operational state without leaking sensitive topology.

### Work

1. Preserve compatibility `/health`.
2. Add `/health/live`.
3. Add `/health/ready` based on:
   - valid config;
   - worker health;
   - available permitted egress/direct policy;
   - verified exit minimum;
   - runtime dependency readiness.
4. Add detailed authenticated diagnostics.
5. Add structured tracing fields and request correlation IDs.
6. Add metrics endpoint or exporter:
   - requests;
   - latency;
   - streams;
   - retries;
   - node health;
   - restarts;
   - search provider outcomes;
   - worker state.
7. Add log rotation/retention or document external logging requirements.
8. Ensure topology and tokens never appear in public health endpoints.

### Acceptance

- Liveness remains green during upstream outage.
- Readiness turns red for no usable egress or failed critical worker.
- Diagnostics explain readiness failure to authenticated operators.
- Observability tests verify redaction.

---

## Phase 11 — Complete supervisor and cross-platform lifecycle

### Goal

Make start, stop, status, restart, logs, and process ownership safe on every supported OS.

### Work

1. Store process identity metadata beyond PID:
   - executable path/hash;
   - start time;
   - nonce or instance ID.
2. Refuse to terminate a reused/unrelated PID.
3. Implement Unix and Windows process termination independently.
4. Remove Unix-only assumptions from advertised cross-platform paths.
5. Test stale PID, corrupt PID file, reused PID simulation, bind conflict, crash-before-ready, shutdown timeout, and unmanaged process.
6. Make runtime file writes atomic with permissions.
7. Define behavior for service managers such as systemd/launchd where applicable.

### Acceptance

- Supervisor never kills a process it cannot identify as its own.
- Start/stop/status/restart tests pass on each supported OS CI runner.
- Stale state is recovered automatically and safely.

---

## Phase 12 — Complete CLI and user workflows

### Goal

Ensure every CLI path is coherent, scriptable, and stable.

### Work

1. Verify every command in JSON, human, and quiet output modes where supported.
2. Standardize exit codes.
3. Remove presentation logic that reads global environment directly.
4. Add `--dry-run` for destructive proxy/config/update operations.
5. Add non-interactive flags for automation.
6. Validate shell completion generation.
7. Add CLI snapshot/E2E tests.
8. Ensure `doctor` covers all critical dependencies and configuration conflicts.

### Acceptance

- `tests/cli_e2e.sh` is deterministic and mandatory.
- Every CLI command has success and failure E2E tests.
- Machine-readable output schemas are stable and documented.
- Destructive actions require explicit intent.

---

## Phase 13 — Installation, update, migration, and rollback

### Goal

Make deployment and upgrades safe and reversible.

### Work

1. Test install, local install, uninstall, and legacy migration scripts in disposable environments.
2. Verify checksums/signatures for downloaded binaries.
3. Make self-update atomic:
   - download temporary file;
   - verify;
   - preserve old binary;
   - replace;
   - health check;
   - rollback on failure.
4. Define config schema version and migrations.
5. Preserve user config/runtime state on upgrade.
6. Test upgrade from every supported prior release baseline.
7. Document rollback commands.

### Acceptance

- Install/uninstall leaves no undocumented files.
- Failed update restores the previous working binary.
- Config migration is test-covered.
- Release artifact verification is mandatory.

---

## Phase 14 — Security hardening and supply-chain gates

### Goal

Treat security as a release blocker.

### Work

1. Threat model:
   - public API;
   - management API;
   - shell delegation;
   - proxy credentials;
   - config editor;
   - updater;
   - Docker socket/process access;
   - SSRF/search provider configuration.
2. Add secret scanning.
3. Add dependency audit and deny policies.
4. Add SAST/Clippy security lints where practical.
5. Fuzz:
   - JSON request parsing;
   - SSE parser;
   - DSML parser;
   - URL/HTML parser;
   - config parser.
6. Add request body, header, line, event, and response limits.
7. Validate file permissions for config, tokens, PID, and logs.
8. Review all `unsafe` blocks and external commands.
9. Run a focused penetration checklist against public-bind mode.

### Acceptance

- `cargo audit`, `cargo deny`, shellcheck, secret scan, fuzz smoke, and security regression suite pass.
- No critical/high unresolved finding.
- Medium findings require explicit documented disposition.

---

## Phase 15 — Replace ignored tests with mandatory test tiers

### Goal

Turn the existing test inventory into reliable release evidence.

### Test tiers

#### Tier A — Per commit

- format;
- compile all targets;
- Clippy `-D warnings`;
- unit tests;
- fast router tests;
- fixture-based upstream/search/SOCKS tests;
- CLI E2E;
- docs/schema consistency.

#### Tier B — Protected CI

- Docker fake-runtime integration;
- real local SOCKS fixture;
- release build;
- install/uninstall tests;
- Linux/macOS supervisor tests;
- OpenAPI validation;
- security scans.

#### Tier C — Scheduled/system

- real Docker/WARP pool;
- exit-IP uniqueness;
- container failure/recovery;
- external search canary;
- release artifact smoke tests;
- long-running leak/soak test.

### Work

1. Classify all 19 ignored tests.
2. Replace public-network dependencies with local fixtures.
3. Remove `#[ignore]` from deterministic tests.
4. Tag true system tests through features/profile/scripts rather than silent ignores.
5. Make Tier C mandatory before release promotion.
6. Add flaky-test policy: no blind rerun acceptance.

### Acceptance

- Zero unexplained ignored tests.
- Every release has Tier A, B, and latest successful Tier C evidence.
- Test reports are archived as artifacts.

---

## Phase 16 — CI/CD and release engineering

### Goal

Prevent incomplete or unverified code from becoming a release.

### Work

1. Matrix CI for supported OS and Rust MSRV/current stable.
2. Cache dependencies safely.
3. Build signed/checksummed release artifacts.
4. Generate SBOM and provenance.
5. Verify release artifacts in clean containers/VMs.
6. Add branch protection required checks.
7. Add version consistency checks across Cargo, changelog, docs, and binary output.
8. Automate release notes from conventional commits plus reviewed changelog.
9. Require completion-matrix closure for release tag.

### Acceptance

- Release cannot publish unless all mandatory gates pass.
- Artifacts are reproducibly identified by checksums and provenance.
- Installation from released artifacts is smoke-tested.

---

## Phase 17 — Documentation and operator experience

### Goal

Make the implementation understandable and operable without reading source.

### Required documents

1. Architecture overview.
2. Full config reference with defaults and precedence.
3. CLI reference and exit codes.
4. Anthropic compatibility matrix.
5. REST/OpenAPI documentation.
6. Proxy/WARP architecture and limitations.
7. Security model.
8. Production deployment guide.
9. Health/readiness/metrics guide.
10. Troubleshooting and incident playbooks.
11. Upgrade, migration, and rollback guide.
12. Contributor/testing guide.
13. Release checklist.

### Acceptance

- All commands and config examples are executable in docs tests where possible.
- No README claim exceeds verified feature coverage.
- Generated references match code.

---

## Phase 18 — Final stabilization and release candidate

### Goal

Prove the complete system under realistic operation.

### Work

1. Run 24-hour soak with concurrent sync/stream requests.
2. Inject failures:
   - upstream timeout/429/400/500;
   - malformed SSE;
   - proxy process loss;
   - duplicate IP;
   - Docker unavailable;
   - disk write failure;
   - worker panic;
   - client disconnect;
   - shutdown during active stream.
3. Track memory, file descriptors, tasks, container count, and latency.
4. Run complete security review.
5. Freeze release candidate and fix only release blockers.
6. Produce final evidence bundle.

### Acceptance

- No unbounded memory/task/FD growth.
- Recovery matches documented policy.
- No critical/high defect remains.
- Completion matrix is 100% green.

---

# 5. Final release gates

The repository is complete only when all gates below pass on the same release candidate commit.

## Gate A — Source quality

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Requirements:

- zero warnings;
- zero unreviewed `unsafe`;
- zero `todo!()` or `unimplemented!()` in production code;
- no public feature missing from the feature matrix.

## Gate B — Deterministic tests

```bash
cargo test --all-targets --all-features
./tests/cli_e2e.sh
```

Requirements:

- zero failed tests;
- zero unexplained ignored tests;
- no test depends on uncontrolled public network.

## Gate C — Security

```bash
cargo audit
cargo deny check
shellcheck ...
secret-scan ...
fuzz-smoke ...
```

Requirements:

- no critical/high findings;
- no secret leakage tests failing.

## Gate D — System behavior

Mandatory evidence for:

- sync request;
- streaming request;
- reasoning stream;
- native tool call;
- DSML tool call;
- shell delegation allowed/blocked;
- web search fallback;
- proxy sticky routing;
- retry exclusion;
- failover/recovery;
- verified unique exit IP;
- no direct-IP leak;
- graceful shutdown;
- supervisor lifecycle;
- config apply/rollback;
- update/rollback.

## Gate E — Cross-platform

- Linux supported matrix passes.
- macOS supported matrix passes.
- Windows passes if still advertised; otherwise documentation and release targets remove Windows claims.

## Gate F — Release artifact

- release binaries build;
- checksums/signatures/SBOM generated;
- clean install succeeds;
- health/readiness smoke succeeds;
- uninstall succeeds;
- previous version upgrade and rollback succeed.

## Gate G — Documentation

- generated OpenAPI matches runtime;
- config reference matches schema;
- CLI docs match help output;
- all operational guides reviewed;
- completion matrix is fully green.

---

# 6. Evidence rules

A checklist item is not complete based only on code review or a successful compile.

Each completed item must include at least one of:

- named automated test and output;
- deterministic smoke script and captured result;
- CI job URL/artifact reference;
- generated contract diff;
- before/after runtime evidence;
- security scanner report;
- system test log.

Evidence must be stored under:

```text
verification/completion/
  phase-00/
  phase-01/
  ...
  phase-18/
  final/
```

Every phase receives:

- `SUMMARY.md`;
- exact commands;
- commit hash;
- pass/fail results;
- unresolved risks;
- links to tests and implementation.

---

# 7. Commit and review strategy

1. One branch per phase or tightly coupled sub-phase.
2. Keep behavior migrations separate from mechanical moves.
3. Add characterization tests before changing stateful behavior.
4. Every bug fix must include a regression test.
5. Every new state transition must include table-driven tests.
6. No large phase is merged without intermediate green commits.
7. Squash only when it does not destroy useful audit history.
8. Final integration branch remains release-blocked until all gates pass.

---

# 8. Recommended implementation order

```text
0. Contract matrix
1. Configuration consolidation
2. Egress state model
3. Exit identity verification
4. Infrastructure adapters
5. Worker lifecycle and shutdown
6. Protocol correctness
7. Retry/rate-control completion
8. Search completion
9. Management/OpenAPI/dashboard
10. Health/readiness/observability
11. Supervisor cross-platform completion
12. CLI completion
13. Install/update/migration/rollback
14. Security hardening
15. Mandatory test tiers
16. CI/CD and release
17. Documentation
18. Stabilization and release candidate
```

Phases 6–12 can be partially parallelized only after Phases 1–5 establish stable configuration, egress, infrastructure, and lifecycle interfaces.

---

# 9. Explicit stop condition

Do **not** declare the overhaul complete when:

- only architecture cleanup is finished;
- unit tests pass but system tests are ignored;
- Docker/WARP behavior is not verified;
- exit identities are unknown or duplicated;
- direct-IP leakage is still possible;
- only Linux behavior is proven while other platforms are advertised;
- OpenAPI/docs do not match runtime;
- updater/installer rollback is untested;
- background workers cannot be cancelled;
- completion matrix contains any mandatory red or unknown item.

The only valid completion state is:

```text
All phases accepted
+ all final gates passed
+ evidence bundle committed
+ feature matrix 100% green
+ release candidate smoke-tested from built artifacts
```
