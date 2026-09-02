# Provider Capacity Scheduler — Implementation Plan

> **Owner:** provider-management runtime
> **Status:** approved design, ready for inline implementation
> **Scope:** v1 process-local credential-pool capacity control; no Redis, no queue

## Outcome

Replace the current ordered-only provider target selection with an atomic
admission scheduler. An alias will still select provider/model candidates by
priority, but a candidate can bind to a credential pool. The scheduler chooses
one healthy credential fairly, prevents local concurrency/RPM/TPM overload,
applies cooldowns immediately after provider failures, and moves to the next
candidate when a pool cannot admit the request.

The public operator experience is deliberately small:

```text
opencode2api pool list|show|set|remove
opencode2api alias set ... provider:model:pool=POOL_ID
opencode2api route simulate ALIAS --concurrent N
opencode2api health
```

The v1 scheduler is held in a single process-local runtime snapshot. It
fails fast when capacity is unavailable: it does not create an unbounded wait
queue and it does not coordinate capacity between multiple gateway processes.

## Constraints carried into the work

- Do not spawn OpenCode, Kilo Code, or another agent CLI to serve requests.
- Preserve encrypted credential storage and never expose secret values in CLI,
  REST, logs, metrics, diagnostics, or tests.
- Preserve the existing fallback contract: retry only before downstream body
  bytes are committed; never replay a partially streamed response.
- Preserve current direct credential and anonymous candidates as backward
  compatible configuration.
- An alias such as `claude-code-1m` can contain same-context alternatives
  (for example DeepSeek 1M and GLM Flash 1M); the scheduler never silently
  mixes models that the alias has not declared.
- Context window and auto-compact policy remain model metadata. Capacity
  control does not pretend that 1M context makes an upstream request free.

## Design map

```text
alias
  └─ priority candidate group
       └─ RouteTarget(provider, model, direct credential | pool | anonymous)
            └─ CapacityScheduler::admit(demand, exclusions)
                 ├─ checks cooldown + shared quota scope
                 ├─ selects a pool member fairly
                 ├─ atomically reserves in-flight/RPM/TPM capacity
                 └─ returns DispatchLease(concrete AttemptTarget)
                        ├─ success: observe headers/usage
                        ├─ upstream failure: record cooldown/breaker state
                        └─ drop: release in-flight capacity exactly once
```

## Baseline and responsibility boundary

Existing files have these roles and must remain single-purpose:

| Area | Existing ownership | Result after this change |
| --- | --- | --- |
| `provider/config.rs` | TOML parse/migrate/render | parses explicit pools and bindings |
| `provider/store.rs` | durable mutations | validates and persists pool CRUD |
| `provider/registry.rs` | immutable resolved catalog | exposes semantic route targets and pool metadata |
| `provider/routing.rs` | priority/fallback planner | returns routes, not a prematurely selected key |
| `provider/resilience.rs` | cooldown policy | becomes scheduler-owned state machinery |
| `provider/capacity.rs` | new | atomic admission, reservations, fairness, live summary |
| `opencode/retry/execute` | HTTP attempt orchestration | acquires/completes a dispatch lease |
| `opencode/retry/response.rs` | downstream body ownership | keeps lease alive until stream/body ends |
| `cli.rs` / `command/provider.rs` | operator commands | pool CRUD and local concurrency simulation |
| `rest_api.rs` | live observability endpoint | exposes sanitized capacity summary |

Do **not** leave a second mutable `RouteState` on `AppState`. The cooldown and
capacity state used for an attempt must be in the same scheduler mutex as the
admission reservation, otherwise a parallel request can admit a credential
between an error and its cooldown update.

## Task 1 — Introduce explicit pool and route-binding domain types

**Files:**

- Modify `src/provider/types.rs`
- Modify `src/provider/mod.rs`
- Add `src/provider/capacity.rs` (types only in this task)
- Modify tests in `tests/provider_schema_v3_contract.rs`

**Steps:**

1. Add a transparent `CredentialPoolId` newtype beside `CredentialId`, using
   the same non-empty identifier validation and serde/display conventions.
2. Add the public immutable configuration types:

   ```rust
   pub enum PoolStrategy {
       RoundRobin,
       LeastLoaded,
       WeightedRoundRobin,
   }

   pub struct CredentialPoolMember {
       pub credential_id: CredentialId,
       pub quota_scope: String,
       pub max_in_flight: NonZeroU32,
       pub requests_per_minute: Option<NonZeroU32>,
       pub tokens_per_minute: Option<NonZeroU64>,
       pub weight: NonZeroU32,
   }

   pub struct CredentialPool {
       pub id: CredentialPoolId,
       pub provider_id: ProviderId,
       pub strategy: PoolStrategy,
       pub members: Vec<CredentialPoolMember>,
   }
   ```

   `quota_scope` is an opaque non-empty local label; it must not be rendered
   as an API key or inferred from secret content.
3. Replace the ambiguous optional credential on `ModelCandidate` with a
   typed binding:

   ```rust
   pub enum CredentialBinding {
       Anonymous,
       Direct(CredentialId),
       Pool(CredentialPoolId),
   }
   ```

   Migration helpers may retain the former `credential_id: Option<_>` only
   inside deserialization adapters. Runtime types must not represent
   `credential=None` and `pool=None` simultaneously as an accidental state.
4. Add `RouteTarget`, which keeps provider/model metadata and a
   `CredentialBinding`; it is deliberately not an HTTP-ready `AttemptTarget`.
   Keep `AttemptTarget` concrete: it always contains either a selected
   `CredentialId` or an explicit anonymous auth mode.
5. Add `CapacityDemand` with input estimate, target output estimate, and
   `estimated_tokens`. Compute the latter safely as
   `input.saturating_add(output).saturating_mul(125).saturating_add(99) / 100`,
   with a minimum of one.
6. Export the new public types through `provider/mod.rs` only if their current
   consumers use the module facade; avoid broad re-exports of scheduler
   internals.

**Tests before implementation:**

- Add unit tests for valid/invalid pool ids and all three binding variants.
- Add schema-facing tests proving a candidate cannot deserialize into both a
  direct credential and a pool binding.
- Add overflow/minimum tests for `CapacityDemand`.

**Verification:**

```bash
cargo test --locked --test provider_schema_v3_contract
cargo test --locked provider::types
```

**Commit:** `feat(provider): add credential pool domain types`

## Task 2 — Extend schema v3, registry validation, and store mutations

**Files:**

- Modify `src/provider/config.rs`
- Modify `src/provider/registry.rs`
- Modify `src/provider/store.rs`
- Modify `tests/provider_store_contract.rs`
- Modify `tests/provider_management_core.rs`
- Modify `tests/provider_schema_v3_contract.rs`

**Steps:**

1. Add a v3 TOML table shape that is human-editable and stable on render:

   ```toml
   [credential_pools.free_1m]
   provider = "bai"
   strategy = "weighted_round_robin"

   [[credential_pools.free_1m.members]]
   credential = "deepseek-a"
   quota_scope = "deepseek-account-a"
   max_in_flight = 1
   requests_per_minute = 20
   tokens_per_minute = 120000
   weight = 1
   ```

   A candidate chooses exactly one binding:

   ```toml
   [[aliases.claude-code-1m.candidates]]
   provider = "bai"
   model = "deepseek-v3.2"
   credential_pool = "free_1m"
   priority = 10
   ```

2. Keep legacy `credential = "..."` candidate syntax. Its compiled runtime
   binding becomes `CredentialBinding::Direct`. No renderer should expand a
   legacy direct binding into a synthetic named pool.
3. Parse `strategy` with a precise error that names the pool and allowed
   values. Default omitted strategy to `round_robin`; default member fields to
   `quota_scope="credential:<credential-id>"`, `max_in_flight=1`, `weight=1`,
   and disabled RPM/TPM.
4. Add semantic validation in the registry builder, not only TOML syntax:

   - pool id is unique and pool has at least one member;
   - pool provider exists and equals the provider of every member credential;
   - every member credential exists and belongs to the pool provider;
   - every candidate provider/model exists;
   - a candidate names only `credential` **or** `credential_pool`;
   - a pool cannot contain duplicate credential ids;
   - a credential can belong to only one explicit pool;
   - `max_in_flight` is in `1..=1024`, RPM in `1..=10_000_000`, TPM in
     `1..=10_000_000_000`, and weight in `1..=1000`;
   - non-default weight is accepted only for `weighted_round_robin`;
   - members sharing one `quota_scope` declare identical RPM/TPM limits;
   - direct bindings retain the credential/provider relationship check.

5. Add `ProviderSnapshot.pools: BTreeMap<CredentialPoolId, CredentialPool>`
   and lookups needed by routing and capacity. Keep sorted maps for stable
   config output and deterministic tests.
6. Extend the store API with transactional `list_pools`, `show_pool`,
   `upsert_pool`, and `remove_pool`. Deletion must reject a pool referenced by
   an alias candidate, returning a structured conflict that lists aliases,
   rather than creating a dangling route.
7. Centralize rendering of pool TOML under the v3 renderer; do not duplicate
   it between CLI and REST paths. Preserve existing comments/order guarantees
   where the current renderer already offers them.

**Tests before implementation:**

- Fixture round trip: parse → registry → render → parse retains pool/member
  fields and direct/anonymous candidates.
- Reject duplicate pool member, a credential reused by another explicit pool,
  unknown credential, wrong provider, out-of-range quota fields, invalid
  non-weighted strategy weight, mixed direct/pool binding, and inconsistent
  shared quota scope limits.
- Store CRUD persists atomically; removing a referenced pool fails without
  modifying the file.

**Verification:**

```bash
cargo test --locked --test provider_schema_v3_contract --test provider_store_contract
cargo test --locked --test provider_management_core
```

**Commit:** `feat(provider): persist validated credential pools`

## Task 3 — Make routing semantic and retain one atomic runtime snapshot

**Files:**

- Modify `src/provider/routing.rs`
- Modify `src/provider/registry.rs`
- Modify `src/state.rs`
- Modify `src/app/mod.rs`
- Modify `src/rest_api.rs`
- Modify `tests/provider_fallback_fixtures.rs`

**Steps:**

1. Change `Planner::plan` (or its equivalent provider-runtime route method)
   to return ordered `RouteTarget` values. It must preserve the current alias
   priority and same-priority ordering semantics, but it must not choose a
   credential-pool member.
2. Introduce an immutable runtime snapshot:

   ```rust
   pub struct ProviderRuntimeSnapshot {
       pub registry: Arc<ProviderSnapshot>,
       pub scheduler: Arc<CapacityScheduler>,
   }
   ```

   `ProviderRuntimeHandle` atomically swaps `Arc<ProviderRuntimeSnapshot>`.
   Build both registry and scheduler completely before the swap; if validation
   fails, retain the previous live snapshot.
3. Move mutable cooldown/breaker state into `CapacityScheduler` and remove
   `AppState.provider_route_state`. There must be no call path that clears a
   separate route state after a reload.
4. On reload, a request already holding a previous snapshot or dispatch lease
   continues safely against that snapshot. New requests see the new scheduler.
   This avoids use-after-reload behavior and avoids mutating a pool while an
   old request owns its reservation.
5. Update the provider-runtime REST summary to read both configured registry
   data and the scheduler's live sanitized summary from the same snapshot.

**Tests before implementation:**

- A two-candidate alias yields routes in existing priority order but leaves
  pool member choice unresolved.
- A failed reload leaves the former registry and live admission state active.
- A successful reload changes future routing without invalidating a lease held
  from the former snapshot.

**Verification:**

```bash
cargo test --locked --test provider_fallback_fixtures --test provider_management_http
cargo test --locked provider::routing
```

**Commit:** `refactor(provider): route through immutable runtime snapshots`

## Task 4 — Implement atomic admission, fairness, and lease lifecycle

**Files:**

- Complete `src/provider/capacity.rs`
- Modify `src/provider/resilience.rs`
- Modify `src/observability.rs`
- Add `tests/provider_capacity_scheduler.rs`

**Steps:**

1. Make `CapacityScheduler` process-local and internally synchronized:

   ```rust
   pub struct CapacityScheduler {
       state: std::sync::Mutex<SchedulerState>,
   }

   struct SchedulerState {
       pools: BTreeMap<CredentialPoolId, PoolRuntime>,
       scopes: BTreeMap<String, QuotaScopeRuntime>,
       route_state: RouteState,
       next_reservation_id: u64,
   }
   ```

   Do not hold this mutex across secret resolution, DNS, HTTP I/O, response
   streaming, or timers. Admission, reservation, cooldown update, and release
   each take the lock for a short bounded operation.
   Compile a direct binding into a non-persisted one-member implicit pool with
   `quota_scope=credential:<id>` and default limits, so legacy candidates get
   the same lease/cooldown path. An anonymous binding gets a lease without a
   credential quota reservation; it remains subject to the existing global
   bridge guard and provider/model cooldowns.
2. Model a request as an explicit reservation:

   ```rust
   pub struct DispatchLease {
       reservation_id: u64,
       target: AttemptTarget,
       scheduler: Arc<CapacityScheduler>,
       completed: AtomicBool,
   }
   ```

   Its `Drop` releases only in-flight counters exactly once. `fail(...)` and
   `observe_success(...)` change state first and mark terminal observations in
   a way that remains idempotent if a body wrapper later drops it.
3. Implement `admit(routes, demand, exclusions, now)` as a single mutex
   transaction:

   - visit `RouteTarget`s in planner priority order;
   - discard route/member identities tried in this request, preventing a loop;
   - reject members under active cooldown or circuit-open state;
   - check member max in-flight and the quota-scope max in-flight derived as
     the smallest member cap declared for that shared scope;
   - refill scope token buckets from monotonic elapsed time, then reserve one
     RPM token and `demand.estimated_tokens` TPM tokens only if both fit;
   - increment member and scope in-flight state;
   - allocate a reservation id and return its concrete selected target.

   If a member is unavailable, keep trying members in the same pool. If the
   entire pool is unavailable, continue to the next alias candidate. If no
   candidate admits, return `AdmissionError::Exhausted { retry_after }`, where
   `retry_after` is the earliest known cooldown/token refill time when one is
   available. If a single demand can never fit a scope's full TPM bucket,
   report it as exhausted immediately instead of retrying equivalent keys.
4. Define selection algorithms without building a repeated weight vector:

   - `RoundRobin`: scan from a per-pool cursor; advance the cursor only after
     a successful admission, not after a skipped unavailable member.
   - `WeightedRoundRobin`: use a bounded virtual weighted-cycle cursor
     equivalent to deterministic expanded rotation, without allocating a
     repeated member vector. Advance it only for an admitted member.
   - `LeastLoaded`: choose the admissible member with the lowest rational
     `member_in_flight / effective_limit`; compare via cross multiplication,
     then break ties by stable member ordering/cursor to avoid a permanent
     first-member bias.

   For shared `quota_scope`, scope availability is checked before all three
   strategies. This prevents two credential ids backed by one provider account
   from evading its shared limit.
5. Keep the existing `RouteState` cooldown policy and expose only methods that
   operate while scheduler state is locked. On upstream 429, honor a parsed
   `Retry-After` when present, otherwise use the existing exponential cooldown.
   Credential auth failures receive a long cooldown; connection/timeouts use
   the existing transient handling; non-retryable request validation failures
   do not poison a credential.
6. Add `CapacityObserver`/`TokenUsage` so a response can refine the reservation
   using actual usage or trusted `x-ratelimit-*` headers. Header parsing must
   be defensive: absent, malformed, or vendor-specific headers are ignored
   rather than failing an otherwise valid response. V1 uses local configured
   quotas as the admission authority; upstream headers improve cooldown and
   diagnostics only.
7. Expose a sanitized `CapacitySummary`: pool strategy, member state counts,
   in-flight count, cooldown remaining, and configured quota values. It must
   omit credential secrets, authorization headers, and raw upstream response
   bodies.
8. Add only low-cardinality aggregate metrics: admissions, rejects, fallback
   advances, cooldown events, in-flight count, and token estimates. Do not add
   a label per user, alias request, API key, model text, or quota scope.

**Tests before implementation:**

- Three concurrent admissions to a three-key `max_in_flight=1` round-robin
  pool select each credential once; a fourth fails fast.
- Releasing a lease permits a later admission and never decrements twice.
- A weight `3:1` pool produces the expected deterministic weighted sequence under
  unconstrained admission.
- Least-loaded balances equal members and has deterministic tie breaks.
- Shared scope blocks a sibling credential after scope RPM/TPM capacity is
  consumed, even when its individual member has spare in-flight capacity.
- Token bucket refills after a controlled clock advance; no real sleeps occur
  in unit tests. Inject a clock/`Instant` abstraction if the existing test
  helpers require it.
- One member in cooldown is skipped; all members cooling down report the
  shortest retry delay; next fallback route can still be selected.
- `Drop`, explicit failure, and success observation are idempotent.
- Summary/metric serialization contains no test secret.

**Verification:**

```bash
cargo test --locked --test provider_capacity_scheduler
cargo test --locked provider::capacity
cargo clippy --locked --all-targets -- -D warnings
```

**Commit:** `feat(provider): schedule credential pool capacity atomically`

## Task 5 — Wire capacity leases into retry, fallback, and response ownership

**Files:**

- Modify `src/opencode/retry/execute/mod.rs`
- Modify `src/opencode/retry/response.rs`
- Modify `src/opencode/forward/common/tokens.rs`
- Modify `src/opencode/forward/sync.rs`
- Modify `src/opencode/forward/stream/execute.rs`
- Modify `src/error.rs`
- Modify `tests/provider_resilience_contract.rs`
- Modify `tests/stream_retry_gates.rs`
- Modify `tests/provider_management_http.rs`

**Steps:**

1. Estimate capacity demand once before dispatch. Reuse the established input
   token estimator in `forward/common/tokens.rs`; add a generic provider
   request/JSON helper only where the OpenAI-shaped request helper cannot be
   used. Target output comes from `max_tokens`, otherwise the declared model
   maximum, otherwise zero. Do not claim tokenizer-exact accounting.
2. Replace the current loop that directly executes planner `AttemptTarget`s:

   ```text
   plan RouteTargets
   → scheduler.admit(next healthy concrete target)
   → resolve selected secret
   → make upstream request
   → classify result and update lease
   → either return body-owning response or try next route
   ```

   Maintain a request-local identity set so a direct credential or pool member
   is never retried twice in the same request.
3. Resolve credentials only after admission. If secret resolution fails or a
   required secret is missing, call `lease.fail(CredentialRejected)`, apply the
   configured credential cooldown, and continue eligible fallback routes.
4. On pre-body upstream failure, classify the result, call `lease.fail`, then
   allow existing retry/fallback gates to decide whether to continue. For a
   429, observe `Retry-After` before failure state is recorded. For exhausted
   local capacity with no eligible fallback, return a new structured
   `BridgeError::ProviderCapacityExhausted { retry_after }` mapped to HTTP 429
   with a bounded `Retry-After` response header when known.
5. Extend `LeasedResponse` to own both the current egress lease and an optional
   `DispatchLease`. The capacity lease must remain in-flight until `text()`,
   `bytes_stream()`, cancellation, or wrapper drop completes. This is critical:
   releasing it immediately after receiving headers would let long streaming
   requests overrun a `max_in_flight` limit.
6. For synchronous responses, parse OpenAI-compatible usage after the body is
   read and call `observe_success(TokenUsage)`. For streams, arrange one
   terminal observer on the stream completion/error path; cancellation/drop
   still releases in-flight capacity but does not invent token usage. Never
   replay an already-started stream merely to attempt a pool fallback.
7. Keep legacy single-key `upstream_api_keys` behavior untouched unless it is
   explicitly expressed through a v3 provider alias. Document it as legacy
   atomic key rotation, not a substitute for pool admission control.

**Tests before implementation:**

- 429 from a selected member marks it cooling down, then fallback selects a
  different member/candidate before body bytes are sent.
- All local pool capacity exhausted maps to 429 and a sanitized error code;
  an alias second candidate is used instead when it has capacity.
- A response stream holds the member in-flight slot while open and frees it on
  end/error/drop.
- An initial upstream request that has committed bytes is never retried.
- Missing credential secret neither leaks a name/value nor leaves in-flight
  capacity reserved.
- Actual usage/header observations do not make counters negative and malformed
  headers do not fail requests.

**Verification:**

```bash
cargo test --locked --test provider_resilience_contract --test stream_retry_gates
cargo test --locked --test provider_management_http
cargo test --locked opencode::retry
```

**Commit:** `feat(retry): admit provider capacity before dispatch`

## Task 6 — Add concise pool operator CLI and deterministic route simulation

**Files:**

- Modify `src/cli.rs`
- Modify `src/app/mod.rs`
- Modify `src/command/provider.rs`
- Modify `tests/cli_rearchitecture_contract.rs`
- Modify `tests/provider_management_core.rs`
- Update `README.md` and relevant CLI docs

**Steps:**

1. Add one top-level `Pool` command alongside `Provider`, `Credential`,
   `Alias`, `Route`, `Health`, and `Config`. Keep nesting at two levels:

   ```text
   opencode2api pool list
   opencode2api pool show POOL_ID
   opencode2api pool set POOL_ID --provider PROVIDER --strategy round_robin --member ...
   opencode2api pool remove POOL_ID
   ```

2. Require `--provider PROVIDER` on `pool set`, then use a strict repeatable
   member grammar:

   ```text
   --member CREDENTIAL,QUOTA_SCOPE,MAX_IN_FLIGHT,RPM,TPM,WEIGHT
   ```

   Empty `RPM` or `TPM` means disabled (for example
   `key-a,bai-acct-a,1,,,1`). Validate/parse it in the command boundary,
   convert to domain input, and route mutations through `ProviderStore`; no
   command should hand-edit TOML.
3. Keep existing candidate input compatible:

   ```text
   provider:model:credential
   provider:model:pool=POOL_ID
   ```

   Reject an invalid `pool=` target with a command error that shows the
   corrected grammar. Do not make bare names ambiguous between credentials and
   pools.
4. Extend `route simulate` with `--concurrent N` (`1..=128`, default `1`). It
   creates an ephemeral scheduler from the loaded configuration and simulates N
   admissions using the normal `CapacityDemand` default. It does not resolve a
   secret, call an API, change live cursor state, or write config.
5. Render table and JSON output with: candidate priority, selected member or
   `capacity-exhausted`, pool id, strategy, quota scope redacted to its local
   label, per-step fallback reason, and retry-after when known. Keep human
   table output compact; preserve automation stability in JSON.
6. Show provider configuration and live health as separate concepts. `pool
   list` is configured state. `health` already queries the daemon
   `/api/v1/provider-runtime` when it is reachable; extend that returned live
   view with capacity and cooldown values, while preserving its direct-probe
   fallback. Neither command emits API key values.
7. Update usage/help examples for an alias that represents a 1M Claude Code
   compatible route and clearly state that the advertised context comes from
   declared model metadata, not inferred from the provider name.

**Tests before implementation:**

- CLI parser accepts/rejects all pool subcommands and member grammar variants.
- `pool set/list/show/remove` calls store and respects referenced-pool removal
  conflict.
- Alias parsing maps `pool=free_1m` to `CredentialBinding::Pool`.
- `route simulate ALIAS --concurrent 3` demonstrates distribution without any mock
  HTTP request or secret access; JSON has no secret fixture.
- Existing top-level command/help snapshots remain readable and valid.

**Verification:**

```bash
cargo test --locked --test cli_rearchitecture_contract --test provider_management_core
cargo run --locked -- pool --help
cargo run --locked -- route simulate --help
```

**Commit:** `feat(cli): manage provider credential pools`

## Task 7 — Expose safe runtime health and operational diagnostics

**Files:**

- Modify `src/rest_api.rs`
- Modify `src/observability.rs`
- Modify `src/error.rs`
- Modify `tests/provider_management_http.rs`
- Modify `tests/provider_capacity_scheduler.rs`
- Update `docs/` operational reference

**Steps:**

1. Extend the existing provider-runtime endpoint rather than adding a parallel
   status service. Add a `capacity` object containing configured pool ids,
   strategy, member count, busy/available/cooling counts, aggregate in-flight,
   quota enabled flags, and next retry delay. Keep its former provider/alias
   fields stable for existing callers.
2. Define separate configured and live views in response types:

   - configured: model routes, pool membership, declared context/output limits;
   - live: scheduler counters/cooldown state from the current runtime snapshot.

   The endpoint must take one runtime snapshot at its start so configuration
   and live state cannot be rendered from different generations after reload.
3. Map `ProviderCapacityExhausted` into the normal error response envelope with
   a stable machine code such as `provider_capacity_exhausted`; do not expose a
   raw upstream 429 body. Return `Retry-After` only when it can be computed.
4. Make error logs useful but safe: include provider id, model id, pool id,
   failure class, and delay; omit credentials, endpoint query payloads, and
   authorization data. Follow current tracing redaction patterns.
5. Document operator interpretation:

   - `capacity_exhausted` means the local scheduler declined to send;
   - upstream 429 means a request was sent and the provider limited it;
   - cooldown shows why a member was skipped;
   - fallback advances are expected under per-key limits and should be watched
     only when all candidates become unavailable.

**Tests before implementation:**

- REST summary changes after controlled admission/release and does not show a
  configured secret or authorization fragment.
- Snapshot consistency test: simulated reload during endpoint construction
  returns one generation only.
- Local exhaustion returns stable HTTP 429 headers/body; malformed retry delay
  never creates an invalid header.
- Metric assertions use aggregate labels only.

**Verification:**

```bash
cargo test --locked --test provider_management_http --test provider_capacity_scheduler
cargo test --locked observability
```

**Commit:** `feat(api): report sanitized provider capacity health`

## Task 8 — Verify migration, regression behavior, and real CLI workflow

**Files:**

- Modify/add test fixtures under `tests/`
- Update `README.md`
- Update `docs/superpowers/specs/2026-09-02-provider-capacity-scheduler-design.md`
  only if implementation exposes an intentional deviation

**Steps:**

1. Run the complete locked test suite and static checks after every logical
   task, then once more after integration. Fix test failures by investigating
   their root cause; do not weaken assertions just to preserve old behavior
   that contradicts the approved capacity contract.
2. Perform a manual, non-secret CLI smoke workflow using a temporary provider
   config directory:

   ```bash
   opencode2api provider add bai https://example.invalid/v1 --kind bai \
     --protocol openai_chat_completions --config "$CFG"
   opencode2api provider model add bai deepseek-1m --context-window 1000000 \
     --max-output-tokens 8192 --free --config "$CFG"
   opencode2api credential set bai demo-a --env DEMO_A --config "$CFG"
   opencode2api credential set bai demo-b --env DEMO_B --config "$CFG"
   opencode2api pool set free-1m --provider bai --strategy round_robin \
     --member demo-a,demo-account-a,1,20,120000,1 \
     --member demo-b,demo-account-b,1,20,120000,1 --config "$CFG"
   opencode2api alias set claude-code-1m \
     --candidate bai:deepseek-1m:pool=free-1m --config "$CFG"
   opencode2api route simulate claude-code-1m --concurrent 3 --config "$CFG"
   opencode2api pool show free-1m --config "$CFG"
   opencode2api health --config "$CFG"
   ```

   Use environment indirection or fake test references only; never put live
   API secrets into shell history, fixture files, documentation, or output.
3. Perform the existing real Claude Code CLI compatibility check with a
   configured non-secret test path before considering the provider work done.
   Confirm `/context` reports the alias's declared 1M context and that the
   compact threshold remains at the configured 80% policy. This verifies the
   bridge's model metadata/adaptation path, not that an arbitrary upstream
   provider grants unlimited actual quota.
4. Run a controlled local mock upstream test with three simultaneous requests:
   assert the first three select distinct available members for a
   `max_in_flight=1` round-robin pool; assert a fourth returns 429 or uses an
   explicitly configured fallback candidate. Then force one mock 429 with a
   `Retry-After` and assert it is skipped on the next request.
5. Scan diffs, rendered config, test logs, and REST JSON for key material. Use
   existing repository secret scanning commands plus a targeted scan for
   fixture values. Review `git diff --check`, formatter, clippy, and docs link
   integrity.
6. Write a concise operator migration note: direct credentials keep working,
   new pools are opt-in, v1 capacity is process-local/fail-fast, and multi-node
   shared quotas require a later distributed coordinator rather than an
   unstated guarantee.

**Full verification gate:**

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --quiet
git diff --check
rg -n -i '(api[_-]?key|authorization|bearer)[=:][^ <]+' docs tests \
  --glob '!**/fixtures/**' || true
```

**Commit:** `test(provider): cover pool capacity and fallback workflow`

## Sequencing and review checkpoints

1. Tasks 1 and 2 establish types/configuration and can be reviewed as a
   standalone compatibility change.
2. Task 3 must land before Task 4 because the scheduler needs snapshot-owned
   registry data and must replace the old detached route state atomically.
3. Task 4 has the highest correctness risk. Review all deterministic scheduler
   tests before wiring any HTTP path.
4. Task 5 is the second review checkpoint: inspect lease ownership especially
   around response streaming and error/drop paths.
5. Tasks 6 and 7 expose the completed behavior to operators; do not invent a
   second store or scheduler instance for CLI simulation.
6. Task 8 is the completion gate. A test suite pass is necessary, but manual
   CLI and Claude Code compatibility checks are also required.

## Explicit non-goals for this iteration

- Redis/database distributed rate coordination across gateway processes.
- A queue, delayed dispatch worker, or request prioritization by end user.
- Automatic discovery of undocumented provider quota limits.
- A dashboard redesign; CLI output should be clear enough to operate the
  scheduler and remain compatible with the planned UI work.
- Arbitrary cross-model fallback. Alias configuration is the explicit contract
  that models are functionally/context compatible.

## Follow-up boundary

Once v1 has production evidence, a later design can add an optional distributed
quota backend behind the same admission interface. It must retain the
reservation/lease semantics, quota-scope identity, fail-fast behavior on an
unavailable coordinator, and no-secret observability guarantees. That work is
intentionally outside this implementation plan.
