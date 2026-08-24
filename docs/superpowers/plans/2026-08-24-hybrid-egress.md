# Hybrid Egress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `hybrid` egress so OpenCode2API becomes usable immediately through direct egress, brings the 1-primary + 1-standby WARP proxy subsystem online in the background, prefers strictly verified proxy routes when healthy, and falls back to direct only for startup/transport availability without using route switching to bypass provider limits.

**Architecture:** Keep `direct` and strict `proxy` semantics unchanged. Add an explicit hybrid proxy-subsystem state machine, a bounded background reconciler/verifier, and route metadata carried with each upstream response. Hybrid startup does not synchronously bootstrap Docker; request routing reads the subsystem state and either selects an eligible verified proxy immediately or chooses direct immediately. Existing per-node health/identity/restart workers remain responsible for ongoing node health and managed-primary restart; the new reconciler owns initial non-destructive topology bring-up and staged readiness publication.

**Tech Stack:** Rust 2021, Tokio, Axum, Reqwest SOCKS proxy support, async-trait, existing Docker CLI runtime abstraction, SQLite history, shell E2E tests.

**Spec:** `docs/superpowers/specs/2026-08-24-hybrid-egress-design.md`

## Global Constraints

- Main `127.0.0.1:4000` MUST NOT be restarted, stopped, or have its route changed during implementation and isolated testing.
- All live hybrid tests run on `127.0.0.1:4010` with a separate runtime directory and history DB.
- Canonical topology remains exactly one primary `socks5h://127.0.0.1:40001` plus one protected warm standby `socks5h://127.0.0.1:40004`; `active_proxy_count = 1`.
- `direct` keeps direct-only behavior; strict `proxy` keeps fail-closed behavior; `--no-proxy` keeps forcing direct and must not bootstrap/mutate proxy containers.
- Hybrid is direct-first at startup and proxy-preferred at runtime: proxy is preferred only after strict readiness; fallback is allowed only for startup/transport availability. Provider 429/quota/account/application errors MUST NOT trigger proxy/direct switching to circumvent provider policy.
- Host-wide WARP (`warp-cli`) MUST NOT be mutated.
- Protected standby MUST NOT be removed, purged, recreated, or otherwise destructively mutated automatically.
- No Docker, SOCKS, identity, route-probe, backoff, or shutdown operation may wait forever; cancellation must interrupt long waits.
- Proxy container `running` alone is never sufficient for routing. A hybrid proxy route becomes preferred only after transport, WARP identity, freshness/duplicate, and route verification pass.
- No user prompt, conversation content, API secret, proxy credential, or auth token may be sent in readiness probes or logged in state/error fields.
- Do not reset, clean, or overwrite unrelated existing dirty worktree changes. Current uncommitted repo changes are dependencies and must be preserved.

---

## File Structure / Responsibility Map

### New files

- `src/proxy_pool/subsystem.rs` — hybrid proxy-subsystem phase/state/snapshot types and transition API. No Docker/network I/O.
- `src/proxy_pool/reconcile.rs` — bounded hybrid bootstrap/reconcile worker, staged verifier abstraction, backoff, and cancellation-aware orchestration.

### Existing files to modify

- `src/config/types.rs` — `EgressMode::Hybrid` and hybrid timeout/backoff fields in `EgressConfig`.
- `src/config/file.rs` — optional TOML fields for hybrid timing.
- `src/config/loader.rs` — env/TOML/default resolution for hybrid.
- `src/config/security.rs` — validation rules that preserve strict proxy fail-closed behavior while allowing hybrid direct availability.
- `src/config/tests.rs` — parser/precedence/security regression tests.
- `src/config/mod.rs` — exports/default constants if needed.
- `src/proxy_pool/mod.rs` — export subsystem/reconcile modules.
- `src/proxy_pool/types.rs` — expose enough node role/identity information for route metadata and verification without changing node safety rules.
- `src/proxy_pool/identity.rs` — reuse public identity probe/duplicate logic; only narrowly extend helpers if reconciler needs a read-only verification primitive.
- `src/proxy_pool/maintenance.rs` — integrate subsystem re-evaluation after ongoing node recovery; keep managed-primary restart as the sole restart owner.
- `src/proxy_pool/routing.rs` — return node role/identity information needed to label routes; keep round-robin semantics.
- `src/proxy_pool/tests.rs` — hybrid readiness/routing/standby/duplicate tests.
- `src/docker/bootstrap.rs` — expose non-destructive ensure primitives for the reconciler; strict proxy startup compatibility remains.
- `src/docker/health.rs` — reusable bounded transport probe primitive if needed.
- `src/docker/types.rs` — no destructive-policy relaxation; only helper/interface additions required by testable reconciliation.
- `src/state.rs` — create proxy pool/workers for `Proxy | Hybrid`, store subsystem state, spawn hybrid reconciler only in Hybrid.
- `src/app/server.rs` — skip synchronous bootstrap only for Hybrid; Direct/Proxy compatibility remains explicit.
- `src/opencode/retry/response.rs` — carry `RouteMetadata` for the response lifetime along with the egress lease.
- `src/opencode/retry/execute.rs` — hybrid immediate route selection, transport-only direct fallback, route metadata, no 429 route switching.
- `src/opencode/retry/tests.rs` and/or inline tests in `execute.rs` — route selection and retry policy regressions.
- `src/history/types.rs` — `RouteKind` serialization in history attempt records.
- `src/history/store.rs` — schema migration and `HistoryCapture::attempt_route(...)`.
- `src/opencode/forward/sync.rs` — annotate final upstream attempt route before consuming response.
- `src/opencode/forward/stream/execute.rs` — annotate streaming attempt route before consuming stream.
- `src/handlers/openai.rs` — annotate OpenAI-compatible attempt route.
- `src/observability.rs` — hybrid egress/bootstrap/state metrics.
- `src/handlers/metadata.rs` — hybrid readiness response with separate direct/proxy state.
- `src/management/dto.rs`, `src/management/service.rs`, dashboard status code as needed — expose subsystem state/metrics without secrets.
- `src/init.rs`, `.env`, `docs/configuration.md`, `docs/proxy-pool.md`, `docs/health-status.md`, `README.md` — generated/default config and user docs.
- `tests/cli_e2e.sh`, relevant Rust integration tests — startup/compatibility checks.

### Ownership rule

- `proxy-reconcile` may call only `inspect`, `create_missing`, and `start_managed` for initial/non-destructive bring-up.
- Existing `proxy-restart` remains the sole worker allowed to call `restart_managed` for managed primary recovery.
- No worker may call destructive lifecycle methods on a protected standby.

---

### Task 1: Add the Hybrid Configuration Contract

**Files:**
- Modify: `src/config/types.rs`
- Modify: `src/config/file.rs`
- Modify: `src/config/loader.rs`
- Modify: `src/config/security.rs`
- Modify: `src/config/tests.rs`
- Modify: `src/config/mod.rs` if constants are needed

**Interfaces:**
- Consumes: existing `BridgeConfig`, `CliOverrides`, `EgressMode::parse`.
- Produces:
  ```rust
  pub enum EgressMode { Direct, Proxy, Hybrid }

  pub struct EgressConfig {
      // existing fields...
      pub bootstrap_timeout: Duration,
      pub verify_timeout: Duration,
      pub recovery_backoff_max: Duration,
  }
  ```
- Environment variables:
  - `BRIDGE_PROXY_BOOTSTRAP_TIMEOUT_SECS`, default `30`
  - `BRIDGE_PROXY_VERIFY_TIMEOUT_SECS`, default `10`
  - `BRIDGE_PROXY_RECOVERY_BACKOFF_MAX_SECS`, default `120`

- [ ] **Step 1: Write failing parser/default tests**

Add tests equivalent to:

```rust
#[test]
fn hybrid_egress_mode_parses_without_changing_direct_or_proxy() {
    assert_eq!(EgressMode::parse("hybrid"), Some(EgressMode::Hybrid));
    assert_eq!(EgressMode::parse("direct"), Some(EgressMode::Direct));
    assert_eq!(EgressMode::parse("proxy"), Some(EgressMode::Proxy));
    assert_eq!(EgressMode::parse("warp"), Some(EgressMode::Proxy));
}

#[test]
fn hybrid_timing_defaults_are_bounded() {
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(config.egress.bootstrap_timeout, Duration::from_secs(30));
    assert_eq!(config.egress.verify_timeout, Duration::from_secs(10));
    assert_eq!(config.egress.recovery_backoff_max, Duration::from_secs(120));
}
```

Also add an env precedence test that sets the three new env vars to `7`, `4`, `45` and asserts the resolved durations.

- [ ] **Step 2: Run targeted tests and verify RED**

Run:

```bash
cargo test --lib config::tests::hybrid_egress_mode_parses_without_changing_direct_or_proxy -- --exact
cargo test --lib config::tests::hybrid_timing_defaults_are_bounded -- --exact
```

Expected: compile/test failure because `Hybrid` and timing fields do not exist.

- [ ] **Step 3: Implement the minimal config types/loader parsing**

Implement:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMode {
    Direct,
    Proxy,
    Hybrid,
}

impl EgressMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "direct" => Some(Self::Direct),
            "proxy" | "warp" => Some(Self::Proxy),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }
}
```

Resolve the new duration fields with env > TOML > defaults and reject zero-second values in validation.

- [ ] **Step 4: Add security regression tests**

Add:

```rust
#[test]
fn strict_proxy_still_rejects_direct_fallback_flag() { /* existing rule remains */ }

#[test]
fn hybrid_does_not_require_legacy_allow_direct_fallback() {
    let mut config = BridgeConfig::default();
    config.egress.mode = EgressMode::Hybrid;
    config.egress.allow_direct_fallback = false;
    assert!(config.validate_security().is_ok());
}
```

- [ ] **Step 5: Run config tests**

Run:

```bash
cargo test --lib config::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/config/types.rs src/config/file.rs src/config/loader.rs src/config/security.rs src/config/tests.rs src/config/mod.rs
git commit -m "feat: add hybrid egress configuration"
```

---

### Task 2: Model Explicit Proxy-Subsystem State

**Files:**
- Create: `src/proxy_pool/subsystem.rs`
- Modify: `src/proxy_pool/mod.rs`
- Test: inline unit tests in `src/proxy_pool/subsystem.rs`

**Interfaces:**
- Consumes: no I/O; standard time types only.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxySubsystemPhase {
    Disabled,
    Starting,
    TransportVerifying,
    IdentityVerifying,
    RouteVerifying,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxySubsystemSnapshot {
    pub phase: ProxySubsystemPhase,
    pub ready: bool,
    pub last_transition_unix_secs: u64,
    pub last_success_unix_secs: Option<u64>,
    pub last_error: Option<String>,
    pub backoff_until_unix_secs: Option<u64>,
}

#[derive(Debug)]
pub struct ProxySubsystemStatus { /* internal fields */ }

impl ProxySubsystemStatus {
    pub fn disabled() -> Self;
    pub fn starting() -> Self;
    pub fn transition(&mut self, phase: ProxySubsystemPhase, error: Option<String>);
    pub fn mark_ready(&mut self);
    pub fn mark_degraded(&mut self, error: impl Into<String>, backoff_until: Option<u64>);
    pub fn is_ready(&self) -> bool;
    pub fn snapshot(&self) -> ProxySubsystemSnapshot;
}
```

`last_error` must be sanitized/truncated to a bounded length (e.g. 512 UTF-8 bytes) before storage.

- [ ] **Step 1: Write failing state-machine tests**

```rust
#[test]
fn subsystem_only_reports_ready_in_ready_phase() {
    let mut state = ProxySubsystemStatus::starting();
    assert!(!state.is_ready());
    state.mark_ready();
    assert!(state.is_ready());
    assert_eq!(state.snapshot().phase, ProxySubsystemPhase::Ready);
}

#[test]
fn degraded_state_records_bounded_secret_safe_error() {
    let mut state = ProxySubsystemStatus::starting();
    state.mark_degraded("x".repeat(2048), Some(123));
    let snap = state.snapshot();
    assert_eq!(snap.phase, ProxySubsystemPhase::Degraded);
    assert!(snap.last_error.unwrap().len() <= 512);
    assert_eq!(snap.backoff_until_unix_secs, Some(123));
}
```

- [ ] **Step 2: Run test and verify RED**

```bash
cargo test --lib proxy_pool::subsystem -- --nocapture
```

Expected: module/types missing.

- [ ] **Step 3: Implement state model only**

Do not add Docker/network behavior in this task.

- [ ] **Step 4: Run unit tests**

```bash
cargo test --lib proxy_pool::subsystem
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/proxy_pool/subsystem.rs src/proxy_pool/mod.rs
git commit -m "feat: model proxy subsystem lifecycle"
```

---

### Task 3: Add a Testable Strict Verification Boundary

**Files:**
- Create: `src/proxy_pool/reconcile.rs`
- Modify: `src/proxy_pool/mod.rs`
- Modify: `src/proxy_pool/identity.rs` only for narrowly reusable public helpers
- Modify: `src/docker/health.rs` only for a reusable bounded transport helper
- Test: inline tests in `src/proxy_pool/reconcile.rs`

**Interfaces:**
- Consumes: `ProxyEntry.client`, `probe_exit_identity`, configured identity endpoints and timeouts.
- Produces:

```rust
#[async_trait]
pub trait ProxyVerifier: Send + Sync + std::fmt::Debug {
    async fn verify_transport(
        &self,
        client: &reqwest::Client,
        timeout: Duration,
    ) -> Result<(), String>;

    async fn verify_identity(
        &self,
        client: &reqwest::Client,
        endpoints: &[String],
        timeout: Duration,
    ) -> Result<ExitIdentity, String>;

    async fn verify_route(
        &self,
        client: &reqwest::Client,
        upstream_base_url: &str,
        timeout: Duration,
    ) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct LiveProxyVerifier;
```

`verify_route` performs a credential-free request to the configured upstream base URL (prefer `/models` if URL joining is valid) and treats receipt of any syntactically valid HTTP response as transport-path success; it must not include user payload or API credentials. A provider HTTP 4xx/429 proves network routing and is not itself a proxy-verification failure. Network/TLS/DNS/timeout failures are failures.

- [ ] **Step 1: Write failing verifier orchestration tests with a fake verifier**

Create a fake that can independently fail transport, identity, or route. Assert stages stop at the first failure and never report Ready.

```rust
#[tokio::test]
async fn staged_verification_never_reaches_route_after_identity_failure() {
    let verifier = FakeVerifier::identity_error("warp=off");
    let result = verify_candidate(&verifier, /* fake client/config */).await;
    assert!(matches!(result, Err(VerificationFailure::Identity(_))));
    assert_eq!(verifier.route_calls(), 0);
}
```

- [ ] **Step 2: Run and verify RED**

```bash
cargo test --lib proxy_pool::reconcile::tests::staged_verification_never_reaches_route_after_identity_failure -- --exact
```

Expected: missing module/interfaces.

- [ ] **Step 3: Implement verification stages with timeout wrappers**

Use `tokio::time::timeout` around every stage. Map timeout errors to bounded strings like `"transport verification timed out after 10s"`.

- [ ] **Step 4: Add full-pass and timeout tests**

Cover transport fail, identity fail/warp off, route fail, and full pass.

- [ ] **Step 5: Run verifier tests**

```bash
cargo test --lib proxy_pool::reconcile
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/proxy_pool/reconcile.rs src/proxy_pool/mod.rs src/proxy_pool/identity.rs src/docker/health.rs
git commit -m "feat: add strict proxy verification stages"
```

---

### Task 4: Implement Bounded Hybrid Background Reconciliation

**Files:**
- Modify: `src/proxy_pool/reconcile.rs`
- Modify: `src/docker/bootstrap.rs`
- Modify: `src/docker/types.rs` only if an injectable helper is required
- Test: `src/proxy_pool/reconcile.rs`, `src/docker/bootstrap.rs`

**Interfaces:**
- Consumes: `ContainerRuntime`, `ProxyVerifier`, `ProxyPool`, `ProxySubsystemStatus`, `WorkerContext`, `BridgeConfig`.
- Produces:

```rust
pub async fn hybrid_proxy_reconciler(
    pool: Arc<RwLock<ProxyPool>>,
    subsystem: Arc<RwLock<ProxySubsystemStatus>>,
    runtime: Arc<dyn ContainerRuntime>,
    verifier: Arc<dyn ProxyVerifier>,
    config: Arc<BridgeConfig>,
    metrics: Arc<Metrics>,
    context: WorkerContext,
) -> Result<(), String>;
```

Ownership constraints in implementation:

```text
reconciler: inspect/create_missing/start_managed only
proxy-restart worker: restart_managed for managed primary only
protected standby: no recreate/remove/purge
```

Backoff sequence must be deterministic enough for tests and jittered in production. Implement a helper:

```rust
fn recovery_backoff(attempt: u32, max: Duration, jitter_seed: u64) -> Duration;
```

Base sequence target: 2s, 5s, 10s, 30s, 60s, capped at configured max.

- [ ] **Step 1: Write failing slow-Docker/nonblocking-cancellation tests**

Use a fake runtime whose `inspect` sleeps for 60s. Drive the worker under paused Tokio time and assert cancellation returns promptly.

Also test Docker unavailable produces `Degraded` and a backoff deadline rather than terminating the worker.

- [ ] **Step 2: Verify RED**

```bash
cargo test --lib proxy_pool::reconcile::tests -- --nocapture
```

Expected: reconciler missing.

- [ ] **Step 3: Implement one bounded reconcile cycle**

Add an internal function:

```rust
async fn reconcile_once(...) -> Result<(), String>;
```

Sequence:
1. `Starting`.
2. bounded runtime inspect/ensure for configured primary and standby.
3. `TransportVerifying`; verify candidates.
4. `IdentityVerifying`; apply verified identities through existing pool logic, then suppress duplicates.
5. require at least one eligible route.
6. `RouteVerifying`; probe selected route.
7. `Ready` on success.

On failure: `Degraded`, record metric/error, sleep cancellation-aware backoff, retry.

- [ ] **Step 4: Add no-thrash and backoff-reset tests**

Assert repeated identical failure does not schedule zero-delay retries, and a successful cycle resets the next failure to the first backoff step.

- [ ] **Step 5: Run reconcile/bootstrap tests**

```bash
cargo test --lib proxy_pool::reconcile
cargo test --lib docker::bootstrap
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/proxy_pool/reconcile.rs src/docker/bootstrap.rs src/docker/types.rs
git commit -m "feat: reconcile hybrid proxies in background"
```

---

### Task 5: Wire Hybrid State and Workers into AppState

**Files:**
- Modify: `src/state.rs`
- Modify: `src/proxy_pool/mod.rs`
- Test: inline tests in `src/state.rs`

**Interfaces:**
- Consumes: `ProxySubsystemStatus`, `hybrid_proxy_reconciler`, existing health/identity/restart workers.
- Produces on `AppState`:

```rust
pub proxy_subsystem: Arc<RwLock<ProxySubsystemStatus>>,
```

Pool construction rule:

```rust
matches!(config.egress.mode, EgressMode::Proxy | EgressMode::Hybrid)
```

Worker rule:

- `Proxy`: existing health + identity + restart workers; no hybrid reconciler.
- `Hybrid`: health + identity + restart + hybrid reconciler.
- `Direct`: empty proxy pool and `ProxySubsystemStatus::disabled()`; no proxy workers.

- [ ] **Step 1: Write failing AppState mode tests**

```rust
#[tokio::test]
async fn hybrid_constructs_pool_but_starts_not_ready() {
    let mut config = BridgeConfig::default();
    config.egress.mode = EgressMode::Hybrid;
    let state = AppState::new_with_container_runtime(config, Arc::new(FakeRuntime::default()));
    assert_eq!(state.proxy_pool.read().await.proxies.len(), 2);
    assert!(!state.proxy_subsystem.read().await.is_ready());
}
```

Retain/extend the existing `direct_mode_does_not_register_proxy_pool_or_workers` regression.

- [ ] **Step 2: Run and verify RED**

```bash
cargo test --lib state::tests -- --nocapture
```

- [ ] **Step 3: Implement AppState wiring**

Inject `Arc<LiveProxyVerifier>` in production. If deterministic tests require injection, add a test-facing constructor parameter rather than global state.

- [ ] **Step 4: Verify worker registry behavior**

Assert hybrid has named workers `proxy-health`, `proxy-identity`, `proxy-restart`, `proxy-reconcile`; direct has none of those.

- [ ] **Step 5: Run state tests**

```bash
cargo test --lib state::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/state.rs src/proxy_pool/mod.rs
git commit -m "feat: wire hybrid proxy workers into app state"
```

---

### Task 6: Decouple Hybrid Gateway Startup from Proxy Bootstrap

**Files:**
- Modify: `src/app/server.rs`
- Modify: `tests/cli_e2e.sh` if a CLI-level timing fake can be expressed safely
- Test: inline unit tests in `src/app/server.rs`

**Interfaces:**
- Consumes: resolved `BridgeConfig.egress.mode`.
- Produces helper:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupProxyPolicy {
    Skip,
    BlockingBootstrap,
    BackgroundReconcile,
}

fn startup_proxy_policy(config: &BridgeConfig, no_proxy: bool) -> StartupProxyPolicy;
```

Expected mapping:

```text
--no-proxy            -> Skip
Direct                -> Skip
Proxy                 -> BlockingBootstrap
Hybrid                -> BackgroundReconcile
```

- [ ] **Step 1: Write failing startup-policy tests**

Include all four rows above and ensure strict proxy compatibility remains blocking.

- [ ] **Step 2: Verify RED**

```bash
cargo test --lib app::server::tests::hybrid_startup_never_blocks_on_bootstrap -- --exact
```

- [ ] **Step 3: Implement policy and move hybrid bootstrap off the parent CLI path**

In `start_daemon`, call `maybe_bootstrap_proxies` only for `BlockingBootstrap`. Hybrid should call supervisor start immediately; the child `AppState` reconciler owns proxy bring-up.

Foreground `server start -f` must use the same policy semantics.

- [ ] **Step 4: Add a fake 60s bootstrap regression**

Use a test seam around bootstrap selection rather than sleeping in production tests. Assert hybrid reaches supervisor start without invoking the blocking bootstrap function.

- [ ] **Step 5: Run server/CLI targeted tests**

```bash
cargo test --lib app::server::tests
bash tests/cli_e2e.sh --help >/dev/null 2>&1 || true
```

Use the repository's existing CLI E2E entry points; do not restart live 4000.

- [ ] **Step 6: Commit**

```bash
git add src/app/server.rs tests/cli_e2e.sh
git commit -m "feat: start hybrid gateway before proxy bootstrap"
```

---

### Task 7: Add Route Metadata and Immediate Hybrid Route Selection

**Files:**
- Modify: `src/opencode/retry/response.rs`
- Modify: `src/opencode/retry/execute.rs`
- Modify: `src/proxy_pool/routing.rs`
- Modify: `src/proxy_pool/types.rs`
- Test: inline tests in `src/opencode/retry/execute.rs`, `src/opencode/retry/response.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteKind {
    Direct,
    Proxy,
    Standby,
    DirectHybridFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMetadata {
    pub kind: RouteKind,
    pub proxy_node: Option<String>,
}
```

`SelectedRoute` gains `metadata: RouteMetadata`.

`LeasedResponse` becomes:

```rust
pub(crate) struct LeasedResponse {
    response: reqwest::Response,
    lease: Option<EgressLease>,
    route: RouteMetadata,
}

impl LeasedResponse {
    pub fn route(&self) -> &RouteMetadata;
}
```

Route rules:

```text
Direct mode                  -> Direct
Hybrid + subsystem !Ready    -> DirectHybridFallback immediately
Hybrid + Ready + primary     -> Proxy
Hybrid + Ready + standby     -> Standby
Proxy + primary/standby      -> Proxy/Standby; fail closed if unavailable
```

- [ ] **Step 1: Write failing route-selection tests**

Add tests for Starting/Degraded/Ready and strict proxy unavailable. Use in-memory pool state; no network calls needed.

- [ ] **Step 2: Verify RED**

```bash
cargo test --lib opencode::retry::execute::tests::hybrid_starting_selects_direct_immediately -- --exact
```

- [ ] **Step 3: Implement immediate hybrid selection**

Refactor `select_route` so Hybrid never enters the current 30-second proxy wait loop when no eligible verified route exists. Strict Proxy retains the current wait/fail-closed loop.

- [ ] **Step 4: Implement route metadata lifetime in LeasedResponse**

Ensure route metadata remains readable before body consumption and the egress lease still lives until text/stream body consumption completes.

- [ ] **Step 5: Run retry/response tests**

```bash
cargo test --lib opencode::retry::execute::tests
cargo test --lib opencode::retry::response
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/opencode/retry/response.rs src/opencode/retry/execute.rs src/proxy_pool/routing.rs src/proxy_pool/types.rs
git commit -m "feat: route hybrid requests without proxy startup delay"
```

---

### Task 8: Preserve 429 Semantics and Add Transport-Only Hybrid Fallback

**Files:**
- Modify: `src/opencode/retry/execute.rs`
- Modify: `src/opencode/retry/tests.rs` if shared fixtures are useful
- Test: targeted retry tests

**Interfaces:**
- Consumes: `FailureClass`, `SelectedRoute.metadata`, existing retained-rate-limit route behavior.
- Produces helper policy:

```rust
fn may_change_egress_after_failure(class: FailureClass) -> bool {
    matches!(class, FailureClass::Transport | FailureClass::Timeout)
}
```

Provider 429/rate-limit must retain the same selected route for the in-flight retry path exactly as current retained-rate-limit logic requires.

- [ ] **Step 1: Write failing 429 and transport tests**

```rust
#[tokio::test]
async fn hybrid_429_never_switches_proxy_to_direct() { /* retained route identity unchanged */ }

#[tokio::test]
async fn hybrid_transport_failure_can_use_direct_when_no_proxy_remains() { /* kind == DirectHybridFallback */ }
```

- [ ] **Step 2: Verify RED**

Run both tests by exact name.

- [ ] **Step 3: Implement minimal failure-class route policy**

Do not alter model fallback budgets. Do not rotate on provider client/server/application classification merely because Hybrid exists.

- [ ] **Step 4: Run full retry tests**

```bash
cargo test --lib opencode::retry
```

Expected: PASS including existing `direct_rate_limit_never_reconnects_host_warp` and `configured_proxy_pool_never_silently_falls_back_to_direct`.

- [ ] **Step 5: Commit**

```bash
git add src/opencode/retry/execute.rs src/opencode/retry/tests.rs
git commit -m "fix: restrict hybrid fallback to transport failures"
```

---

### Task 9: Persist Route Kind in Request History

**Files:**
- Modify: `src/history/types.rs`
- Modify: `src/history/store.rs`
- Modify: `src/opencode/forward/sync.rs`
- Modify: `src/opencode/forward/stream/execute.rs`
- Modify: `src/handlers/openai.rs`
- Test: `src/history/store.rs` plus targeted forward tests

**Interfaces:**
- Consumes: `LeasedResponse::route()`.
- Produces history field:

```rust
pub struct HistoryAttempt {
    // existing fields...
    pub route_kind: Option<RouteKind>,
}

impl HistoryCapture {
    pub fn attempt_route(&self, route: &RouteMetadata);
}
```

SQLite migration:

```sql
ALTER TABLE history_attempts ADD COLUMN route_kind TEXT;
```

Migration must be idempotent using the repository's existing schema/version strategy; never blindly execute an ALTER that fails on every restart.

`attempt_route` updates the last started attempt with:
- `proxy_node = route.proxy_node.clone()`
- `route_kind = Some(route.kind)`

- [ ] **Step 1: Write failing history round-trip test**

Start a capture, call `effective_json`, then `attempt_route` with `Standby/opencode-warp-4`, finish the attempt, reopen/query the store, and assert both values survive SQLite round trip.

- [ ] **Step 2: Verify RED**

```bash
cargo test --lib history::store::tests::attempt_route_round_trips_route_kind_and_proxy_node -- --exact
```

- [ ] **Step 3: Implement schema/type/capture changes**

Update SELECT/INSERT column ordering consistently in all history read/write paths.

- [ ] **Step 4: Annotate response routes at all three protocol paths**

Immediately after upstream response acquisition and before consuming `.text()` or `.bytes_stream()`, call:

```rust
capture.attempt_route(response.route());
```

Apply this to Anthropic sync, Anthropic stream, and OpenAI-compatible paths.

- [ ] **Step 5: Run history + forward protocol tests**

```bash
cargo test --lib history
cargo test --lib opencode::forward
cargo test --test protocol_conformance
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/history/types.rs src/history/store.rs src/opencode/forward/sync.rs src/opencode/forward/stream/execute.rs src/handlers/openai.rs
git commit -m "feat: record upstream egress route in history"
```

---

### Task 10: Add Hybrid Metrics and Readiness Contract

**Files:**
- Modify: `src/observability.rs`
- Modify: `src/handlers/metadata.rs`
- Modify: `src/management/dto.rs`
- Modify: `src/management/service.rs`
- Modify: dashboard status rendering files only if current DTO rendering requires it
- Test: observability + metadata/management tests

**Interfaces:**
- Metrics snapshot gains:

```rust
pub egress_direct_requests: u64,
pub egress_proxy_requests: u64,
pub egress_hybrid_fallbacks: u64,
pub proxy_bootstrap_attempts: u64,
pub proxy_bootstrap_successes: u64,
pub proxy_bootstrap_failures: u64,
pub proxy_state_transitions: u64,
pub proxy_route_probe_failures: u64,
pub proxy_duplicate_exit_events: u64,
```

Add explicit recording methods; do not mutate counters directly outside `Metrics`.

Hybrid readiness JSON must have:

```json
{
  "checks": {"critical_workers": true, "egress": true, "proxy": false},
  "egress": {
    "mode": "hybrid",
    "active_route": "direct",
    "direct": {"ready": true},
    "proxy": {"state": "degraded", "ready": false}
  }
}
```

Strict proxy with unusable pool remains HTTP 503. Hybrid with unusable proxy remains HTTP 200 if critical gateway workers are ready.

- [ ] **Step 1: Write failing readiness tests**

Cover:
- Hybrid + Degraded => 200, active route direct.
- Hybrid + Ready => 200, active route proxy.
- Proxy + unavailable => 503.
- Direct => existing response compatibility preserved as much as possible.

- [ ] **Step 2: Verify RED**

```bash
cargo test --lib handlers::metadata -- --nocapture
```

- [ ] **Step 3: Implement metric counters and readiness response**

Never report proxy Ready from container status alone; read `state.proxy_subsystem`.

- [ ] **Step 4: Add management/status serialization tests**

Ensure `last_error` is bounded/sanitized and no credential-bearing proxy URL is emitted by new fields.

- [ ] **Step 5: Run observability/management tests**

```bash
cargo test --lib observability
cargo test --lib management
cargo test --lib handlers
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/observability.rs src/handlers/metadata.rs src/management/dto.rs src/management/service.rs
git diff --cached --name-only
git commit -m "feat: expose hybrid egress health and metrics"
```

If implementation proves a specific dashboard renderer file must change, add that exact file path explicitly after reviewing its diff; never stage an entire directory in this dirty worktree.

---

### Task 11: Update Defaults, Init Template, CLI/README Documentation

**Files:**
- Modify: `src/init.rs`
- Modify: `.env`
- Modify: `README.md`
- Modify: `docs/configuration.md`
- Modify: `docs/proxy-pool.md`
- Modify: `docs/health-status.md`
- Modify: `docs/deployment.md` if it describes startup blocking
- Modify: `tests/cli_e2e.sh`

**Interfaces:**
- User-facing mode string: `hybrid`.
- Recommended topology: 1 primary `40001`, 1 standby `40004`.
- Do not change live daemon 4000 as part of this task.
- Default promotion rule: only change loader/template default from current behavior to `hybrid` after Task 12 isolated verification proves startup/fallback/recovery. If Task 12 fails, document `hybrid` as opt-in and leave default unchanged.

- [ ] **Step 1: Write failing CLI/config-template expectations**

Assert generated config accepts/documents `direct | proxy | hybrid`, and `--no-proxy` still produces direct behavior.

- [ ] **Step 2: Verify RED**

Run the relevant Rust init/config tests plus safe CLI E2E parsing; do not issue a live `server restart`.

- [ ] **Step 3: Update docs and generated templates**

README workflow should remain simple:

```bash
opencode2api server start      # one-time daemon start
opencode2api set env           # each terminal
claude
```

Users must not need proxy-specific environment exports for the recommended hybrid setup.

- [ ] **Step 4: Run documentation/config tests and diff check**

```bash
cargo test --lib init
cargo test --lib config
bash -n tests/cli_e2e.sh
git diff --check
```

- [ ] **Step 5: Commit**

```bash
git add src/init.rs .env README.md docs/configuration.md docs/proxy-pool.md docs/health-status.md docs/deployment.md tests/cli_e2e.sh
git commit -m "docs: document hybrid egress startup"
```

---

### Task 12: Full Regression Verification Before Live 4010

**Files:**
- No production changes unless a regression is found; fixes require a new RED test first.

**Interfaces:**
- Gate for live testing.

- [ ] **Step 1: Format and compile**

```bash
cargo fmt --check
cargo check --all-targets
```

Expected: PASS.

- [ ] **Step 2: Run library and protocol suites**

```bash
cargo test --lib
cargo test --test protocol_conformance
```

Expected: PASS except repository tests explicitly marked `#[ignore]` for real external dependencies.

- [ ] **Step 3: Run relevant integration/E2E suites**

```bash
cargo test --test fast
bash tests/cli_e2e.sh
bash tests/install_e2e.sh
```

If the repository's CLI E2E has known unrelated failures, record exact pre-existing failures and run the hybrid-specific cases independently; do not claim a green full suite unless it is actually green.

- [ ] **Step 4: Clippy**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 5: Verify main 4000 was not touched**

Capture before/after:

```bash
opencode2api server status --json
curl -fsS http://127.0.0.1:4000/health/live
```

PID must be unchanged across implementation verification unless the user separately authorized a main restart.

- [ ] **Step 6: Commit only regression fixes, if any**

If no changes, do not create an empty commit.

---

### Task 13: Isolated Hybrid Live Test on Port 4010

**Files:**
- Runtime artifacts only under `~/Downloads/bqa/opencode2api-hybrid-test-4010/`; do not add to git.

**Interfaces:**
- Test instance:
  ```text
  host = 127.0.0.1
  port = 4010
  mode = hybrid
  primary = socks5h://127.0.0.1:40001
  standby = socks5h://127.0.0.1:40004
  runtime_dir = ~/Downloads/bqa/opencode2api-hybrid-test-4010/runtime
  history_path = ~/Downloads/bqa/opencode2api-hybrid-test-4010/history.sqlite3
  ```
- 4010 MUST NOT have destructive ownership of proxy containers concurrently with main. For failure injection use fake dead URLs/adapters, not `docker stop` on live proxy containers.

- [ ] **Step 1: Record main baseline**

Save main PID/status/health to the test artifact directory.

- [ ] **Step 2: Launch 4010 hybrid with isolated runtime/history**

Use the freshly built test binary by explicit path, not an older `~/.local/bin` binary. Configure hybrid through environment local to the process.

- [ ] **Step 3: Prove direct-first startup**

Immediately after 4010 binds, issue a benign model request while proxy subsystem is Starting/Degraded. Assert:
- HTTP succeeds through direct.
- history `route_kind = direct-hybrid-fallback`.
- `/health/ready` is 200.

- [ ] **Step 4: Prove automatic proxy promotion**

Wait for full strict verification. Assert:
- proxy subsystem `ready`.
- active route `proxy`.
- next model request returns expected marker.
- history `proxy_node = opencode-warp-1` and route kind `proxy`.

- [ ] **Step 5: Prove safe standby failover without touching live containers**

Use a separate 4010 configuration/fake primary URL `127.0.0.1:49999` with standby `40004`, as already proven safe in prior tests. Assert route kind `standby` and successful model response.

- [ ] **Step 6: Prove all-proxy failure falls back direct**

Use fake dead proxy URLs for both candidates in an isolated test configuration. Assert gateway remains ready and request route kind is `direct-hybrid-fallback`.

- [ ] **Step 7: Prove 429 does not change egress**

Use a deterministic local/fake upstream fixture for this policy check; do not intentionally trigger provider quota/rate limit against the real service. Assert selected route metadata remains unchanged across retained 429 retry handling.

- [ ] **Step 8: Run concurrency smoke**

At minimum repeat the previously successful shapes:
- SOCKS-only: 80 requests per real proxy at concurrency 24.
- model: 8 requests at concurrency 4 and 16 requests at concurrency 8.

Record success ratio and p50/p95/max. Do not declare a performance regression solely from model latency variance unless compared against a baseline from the same upstream period.

- [ ] **Step 9: Re-verify main 4000**

PID and health must remain unchanged; no `docker stop`, `server restart`, or host WARP mutation should appear in test logs.

---

### Task 14: Soak, Promote Defaults, Build Release, and Update Installed Artifacts

**Files:**
- Modify config defaults/templates only if soak passes.
- Modify release/install docs/scripts only if required by final user-facing default.
- Build artifacts outside repo target according to existing `CARGO_TARGET_DIR` policy.

**Interfaces:**
- Promotion requires evidence; no automatic main restart.

- [ ] **Step 1: Run 4010 soak**

Run several hours of synthetic sync/stream traffic with periodic safe fake probe failures. Track:
- RSS and FD count over time.
- worker count/state.
- proxy bootstrap attempts/failures.
- direct/proxy/fallback counters.
- log growth rate.
- restart attempts (should not thrash).

Success condition: no monotonic resource leak, no busy-loop, no stuck worker, and direct fallback remains low-latency when proxy is unavailable.

- [ ] **Step 2: Decide default promotion from evidence**

If soak passes, change the default resolved egress mode and generated user templates to `Hybrid`. If soak does not pass, keep `Hybrid` opt-in and document the failed criterion; do not force the default.

Add/adjust a test asserting the chosen default before changing code.

- [ ] **Step 3: Re-run complete verification after any default change**

Repeat Task 12 in full.

- [ ] **Step 4: Build release binaries**

Use the repo's current release build process and configured non-tmpfs cargo target directory. Verify:

```bash
<release-opencode2api> --version
<release-opencode2api-serve> --version
```

- [ ] **Step 5: Update installed CLI/server binaries without restarting main**

Backup the current installed executables following the existing installer/update convention, replace them atomically, and verify `opencode2api --version`. Do not stop or restart the running 4000 daemon.

- [ ] **Step 6: Verify release/install paths**

Run `tests/install_e2e.sh` and shell integration checks so new installs, updates, and `opencode2api set env` remain functional.

- [ ] **Step 7: Update README/release notes/worklog**

Document:
- Hybrid direct-first behavior.
- Strict verification states.
- 1+1 topology.
- How to inspect proxy readiness.
- Explicit rollback to `direct` or strict `proxy`.
- Main promotion command is intentionally not executed automatically.

- [ ] **Step 8: Commit promotion/release changes**

Stage only files changed for the promotion/release task and commit with a focused message such as:

```bash
git commit -m "feat: promote verified hybrid egress runtime"
```

---

## Final Acceptance Checklist

Before claiming the feature complete, verify every statement with fresh command output:

- [ ] Hybrid gateway starts and becomes direct-usable without waiting for Docker/WARP.
- [ ] Proxy route is never selected before staged verification reaches `Ready`.
- [ ] Healthy verified primary becomes preferred automatically.
- [ ] Verified standby is selected when primary is unavailable.
- [ ] All-proxy unavailability yields immediate direct hybrid fallback, not a 30-second wait.
- [ ] Proxy recovery automatically restores proxy preference without server restart.
- [ ] Provider 429/quota does not cause egress switching.
- [ ] Strict Proxy remains fail-closed; Direct remains direct-only; `--no-proxy` remains direct/no-bootstrap.
- [ ] Background Docker/verification operations are timeout-bounded, backoff-bounded, and cancellation-aware.
- [ ] Protected standby receives no destructive lifecycle operation.
- [ ] History records reliable `route_kind` and `proxy_node` for final upstream attempts.
- [ ] Metrics/readiness distinguish gateway availability from proxy availability.
- [ ] No credentials/user payloads appear in probe requests or proxy state errors/logs.
- [ ] Full regression suite results are recorded accurately.
- [ ] 4010 isolated live test and soak evidence are saved outside the repo.
- [ ] Main 4000 PID/health remained unchanged throughout isolated implementation/testing.
- [ ] Installed/release binaries and install/update flows are verified before any future user-authorized main promotion.
