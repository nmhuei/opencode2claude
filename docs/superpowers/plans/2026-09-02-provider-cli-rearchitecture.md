# Provider Runtime and CLI Rearchitecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the split legacy/schema-v2 provider paths with one
provider-aware runtime and a small, debuggable CLI control plane while keeping
legacy configuration and protocol compatibility during migration.

**Architecture:** The provider registry becomes the single declarative source
for endpoint, credential reference, model metadata, alias, and candidate
policy. A compiled immutable provider snapshot serves requests while transient
route health lives in a separate runtime state. CLI arguments dispatch to
services that mutate a validated TOML transaction or query the daemon; renderers
never own provider logic.

**Tech Stack:** Rust 2021, Clap derive, Axum, Reqwest, Tokio, Serde,
`toml_edit`, existing `AtomicFileStore`, existing test fixture servers.

**Spec:** `docs/superpowers/specs/2026-09-02-provider-cli-rearchitecture-design.md`

## Global Constraints

- Keep Linux support and current public Anthropic Messages/OpenAI Chat API
  behavior.
- Keep provider secrets outside TOML, logs, JSON, `Debug`, and history.
- Use `AtomicFileStore::atomic_write(..., true)` for every provider config or
  managed-secret mutation.
- Make v1 legacy and v2 registry files loadable; only `config migrate --write`
  rewrites a file to schema v3.
- Every alias derives auto-compaction as `floor(context_window * 0.80)`;
  a 1M alias therefore compacts at 800,000 tokens. Keep a 1M alias strict:
  every selected candidate has verified context metadata of at least
  1,000,000 tokens.
- Do not spawn or control upstream provider CLIs.
- Do not retry a streaming request after the first content event.
- Preserve hidden legacy commands and their exit codes for one release cycle.
- All new management commands support human, `--quiet`, and `--json` output.
- `--config` selects mutation/read scope; daemon runtime state comes from the
  daemon API, not caller environment reconstruction.

---

## File Structure

| Path | Responsibility |
| --- | --- |
| `src/cli/mod.rs` | Root parser, globally valid arguments, command enum. |
| `src/cli/provider.rs` | Provider/credential/model/alias/route/health arguments. |
| `src/cli/output.rs` | Typed JSON envelopes and output mode handling. |
| `src/cli/render.rs` | Human-only headers, status lines, facts, tables, hints. |
| `src/command/mod.rs` | Parsed command dispatcher and `CommandContext`. |
| `src/command/{provider,credential,model,alias,route,health,config}.rs` | One service boundary per management noun. |
| `src/provider/domain.rs` | Public provider IDs, records, capabilities, targets. |
| `src/provider/registry.rs` | Cross-reference validation and snapshot compilation. |
| `src/provider/config.rs` | v1/v2/v3 decode and typed migration conversion. |
| `src/provider/store.rs` | Validated read-modify-write TOML transaction. |
| `src/provider/credentials.rs` | Env/file/managed secret sources and redaction. |
| `src/provider/catalog.rs` | Provider-scoped metadata cache and live discovery. |
| `src/provider/adapter.rs` | Adapter contract and generic/exception adapter lookup. |
| `src/provider/routing.rs` | Pure candidate eligibility and explain output. |
| `src/provider/resilience.rs` | Failure classification, cooldown, and next-action policy. |
| `src/provider/runtime.rs` | Immutable registry snapshot plus transient route state. |
| `src/gateway/provider_execute.rs` | Non-streaming provider attempt loop. |
| `src/gateway/provider_stream.rs` | Streaming pre-commit fallback boundary. |
| `src/legacy/config.rs` | Legacy singleton-to-synthetic-provider conversion. |
| `src/legacy/commands.rs` | Hidden command translation and deprecation presentation. |

`src/app/providers.rs` and the schema-v2 branch in
`src/opencode/retry/execute/mod.rs` are removed only after their replacements
pass compatibility tests.

---

### Task 1: Establish CLI parser and presentation boundaries

**Files:**

- Move: `src/cli.rs` → `src/cli/mod.rs`
- Create: `src/cli/provider.rs`
- Create: `src/cli/output.rs`
- Create: `src/cli/render.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/lib.rs`
- Test: `tests/cli_rearchitecture_contract.rs`
- Test: `tests/cli_e2e.sh`

**Interfaces:**

- Produce `Cli`, `GlobalArgs`, `Command`, `ProviderCommand`, `OutputFormat`,
  `JsonEnvelope<T>`, and `CliRenderer`.
- `GlobalArgs` has exactly `config`, `json`, `quiet`, `color`, and `verbose`.
- `CliRenderer` exposes `success`, `warning`, `error`, `facts`, `table`, and
  `hint`; no command service calls `println!` directly.

- [ ] **Step 1: Add parser/output regression tests before moving code.**

```rust
#[test]
fn global_arguments_parse_before_or_after_a_command() {
    let before = Cli::try_parse_from([
        "opencode2api", "--config", "x.toml", "provider", "list",
    ]).unwrap();
    let after = Cli::try_parse_from([
        "opencode2api", "provider", "list", "--config", "x.toml",
    ]).unwrap();
    assert_eq!(before.global.config, Some("x.toml".into()));
    assert_eq!(after.global.config, Some("x.toml".into()));
}

#[test]
fn quiet_and_json_are_mutually_exclusive() {
    assert!(Cli::try_parse_from([
        "opencode2api", "--json", "--quiet", "provider", "list",
    ]).is_err());
}
```

- [ ] **Step 2: Run the focused test and confirm the current parser cannot
  satisfy the single global-config contract.**

Run: `cargo test --locked --test cli_rearchitecture_contract global_arguments_parse_before_or_after_a_command`

Expected: FAIL because `--config` is command-local in the current CLI.

- [ ] **Step 3: Move the root module and introduce typed argument modules.**

Run `git mv src/cli.rs src/cli/mod.rs`. Keep `Cli` and the existing legacy
commands in `mod.rs`; move new provider-management argument structs to
`provider.rs`. Add `GlobalArgs` as a flattened parser field and mark only its
five fields as Clap globals. Do not make model, provider URL, credential, or
server lifecycle flags global.

- [ ] **Step 4: Introduce the renderer without changing provider behavior.**

```rust
pub struct CliRenderer {
    format: OutputFormat,
}

impl CliRenderer {
    pub fn json<T: serde::Serialize>(&self, value: &T) { /* serialize once */ }
    pub fn status(&self, symbol: &str, message: &str) { /* human only */ }
    pub fn hint(&self, command: &str) { /* human only */ }
}
```

Move reusable human output from `src/app/view.rs` into `src/cli/render.rs`.
Keep `OutputFormat::Json` free of ANSI, headers, and hints.

- [ ] **Step 5: Run parser, rendering, and legacy command checks.**

Run: `cargo test --locked --test cli_rearchitecture_contract && bash tests/cli_e2e.sh debug`

Expected: all focused parser tests and the existing CLI E2E suite pass.

- [ ] **Step 6: Commit the parser/presentation boundary.**

```bash
git add src/cli src/app/mod.rs src/app/view.rs src/lib.rs \
  tests/cli_rearchitecture_contract.rs tests/cli_e2e.sh
git commit -m "refactor: separate CLI parser and presentation"
```

### Task 2: Create command services and configuration scope reporting

**Files:**

- Create: `src/command/mod.rs`
- Create: `src/command/config.rs`
- Create: `src/command/health.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/config/loader.rs`
- Modify: `src/config/types.rs`
- Test: `tests/config_scope_contract.rs`

**Interfaces:**

- Produce `CommandContext { config_path, output, verbose }`.
- Produce `ConfigView::{File, Effective, Daemon}` and
  `ResolvedField<T> { value, source }`.
- Produce `GET /api/v1/provider-runtime` DTO in a later task; this task defines
  the client-facing distinction only.

- [ ] **Step 1: Write tests that capture the `.env`/daemon confusion.**

```rust
#[test]
fn explicit_file_view_reports_toml_without_claiming_runtime_state() {
    let view = config_file_view(fixture("model = 'file-model'\n")).unwrap();
    assert_eq!(view.model.value, "file-model");
    assert_eq!(view.model.source, ConfigSource::Toml);
    assert!(!view.is_daemon_runtime);
}

#[test]
fn effective_view_names_the_override_source() {
    let view = effective_view_with_env("OPENCODE_MODEL", "env-model").unwrap();
    assert_eq!(view.model.value, "env-model");
    assert_eq!(view.model.source, ConfigSource::Environment);
}
```

- [ ] **Step 2: Run the tests and confirm source provenance is absent.**

Run: `cargo test --locked --test config_scope_contract`

Expected: FAIL because current config snapshots contain resolved values but no
field source and no explicit file/effective distinction.

- [ ] **Step 3: Add `ConfigSource` and provenance capture in the loader.**

Every resolved operator-visible field records one of `Default`, `Toml`,
`Environment`, or `Cli`. Do not record secret values. Preserve current runtime
precedence; this task makes precedence observable rather than changing it.

- [ ] **Step 4: Route `config show --file` and `config show --effective` through
  `command/config.rs`.**

`--file` reads the selected TOML document and redacts secret fields. `--effective`
uses the normal resolver and includes provenance. Neither command claims it is
the daemon snapshot.

- [ ] **Step 5: Run focused and existing configuration tests.**

Run: `cargo test --locked --test config_scope_contract && cargo test --locked config::tests`

Expected: PASS; existing environment-overrides-TOML behavior stays unchanged.

- [ ] **Step 6: Commit configuration scope reporting.**

```bash
git add src/command src/config src/app/mod.rs tests/config_scope_contract.rs
git commit -m "feat: expose configuration source and scope"
```

### Task 3: Define provider domain capabilities and schema-v3 readers

**Files:**

- Create: `src/provider/domain.rs`
- Modify: `src/provider/mod.rs`
- Modify: `src/provider/types.rs`
- Modify: `src/provider/config.rs`
- Modify: `src/provider/registry.rs`
- Modify: `src/config/migration.rs`
- Test: `tests/provider_schema_v3_contract.rs`
- Test: `tests/provider_management_core.rs`

**Interfaces:**

- Produce `ModelCapabilities { streaming, tools, thinking, vision }`.
- Preserve `Provider`, `Credential`, `ModelInfo`, `ModelAlias`,
  `ModelCandidate`, and `AttemptTarget` as domain records.
- Make `ModelAlias::auto_compact_window()` a derived value of exactly
  `floor(context_window * 0.80)`; no provider, candidate, or alias field can
  override that ratio.
- Produce `ProviderFileV3`, `ProviderConfigError`,
  `load_provider_registry(path) -> Result<ProviderRegistry, ProviderConfigError>`.
- Bump `CURRENT_SCHEMA_VERSION` to `3` while keeping v1/v2 decoding.

- [ ] **Step 1: Write v3 parsing and strict-alias tests.**

```rust
#[test]
fn v3_names_provider_model_and_alias_records() {
    let registry = load_fixture("v3-free-1m.toml").unwrap();
    assert_eq!(registry.provider("bai").unwrap().kind, ProviderKind::OpenAiCompatible);
    assert!(registry.model("bai", "deepseek-v4-flash").unwrap().verified_context);
    assert_eq!(registry.alias("free-1m").unwrap().client_model, "sonnet[1m]");
}

#[test]
fn strict_alias_rejects_unverified_capability_metadata() {
    let error = load_fixture("v3-unverified-1m.toml").unwrap_err();
    assert!(error.to_string().contains("unknown context metadata"));
}

#[test]
fn every_alias_derives_the_fixed_eighty_percent_compaction_boundary() {
    let registry = load_fixture("v3-mixed-contexts.toml").unwrap();
    assert_eq!(registry.alias("free-1m").unwrap().auto_compact_window(), 800_000);
    assert_eq!(registry.alias("small").unwrap().auto_compact_window(), 160_000);
}
```

- [ ] **Step 2: Run the focused tests.**

Run: `cargo test --locked --test provider_schema_v3_contract`

Expected: FAIL because named v3 tables and capability fields are unsupported.

- [ ] **Step 3: Add capability data and v3 decode.**

Use `[providers.<id>]`, `[credentials.<id>]`,
`[models.<provider>."<model-id>"]`, `[aliases.<id>]`, and
`[[aliases.<id>.candidates]]`. Validate non-empty IDs, HTTPS or loopback HTTP
URLs, positive alias context windows, unique candidate priority order, existing
provider/model references, credential ownership, derived 80% compaction, and
strict alias metadata.

- [ ] **Step 4: Make migration read-only by default.**

`migrate_document` accepts v1, v2, and v3. v2 arrays compile to the same domain
registry as v3 tables. Legacy aliases gain a derived 80% compaction boundary
from their context window. It returns a typed migration report and never writes
the input path.

- [ ] **Step 5: Run schema, registry, and old provider tests.**

Run: `cargo test --locked --test provider_schema_v3_contract --test provider_management_core --test provider_management_contract`

Expected: PASS, including schema-v2 fixture compatibility.

- [ ] **Step 6: Commit the domain/schema reader.**

```bash
git add src/provider src/config/migration.rs tests/provider_schema_v3_contract.rs \
  tests/provider_management_core.rs tests/provider_management_contract.rs
git commit -m "feat: add schema v3 provider registry"
```

### Task 4: Add validated atomic provider configuration transactions

**Files:**

- Create: `src/provider/store.rs`
- Modify: `src/provider/config.rs`
- Modify: `src/infrastructure/file_store.rs`
- Test: `tests/provider_store_contract.rs`

**Interfaces:**

- Produce `ProviderConfigStore::open(path)`, `load`, `transaction`, and
  `migrate_to_v3`.
- Produce `ProviderMutation::{AddProvider, SetCredential, UpsertModel, SetAlias,
  RemoveProvider, RemoveCredential, RemoveAlias}`.
- `transaction` validates a complete registry before atomic replacement.

- [ ] **Step 1: Write transaction safety tests.**

```rust
#[test]
fn invalid_mutation_keeps_original_document_bytes() {
    let store = fixture_store("v3-free-1m.toml");
    let before = std::fs::read(store.path()).unwrap();
    let result = store.transaction(ProviderMutation::RemoveProvider("bai".into()));
    assert!(result.is_err());
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
}

#[test]
fn explicit_migration_writes_v3_and_backup() {
    let store = fixture_store("v2-free-1m.toml");
    let report = store.migrate_to_v3().unwrap();
    assert_eq!(report.to_version, 3);
    assert!(store.backup_path(&report).exists());
}
```

- [ ] **Step 2: Run the store contract tests.**

Run: `cargo test --locked --test provider_store_contract`

Expected: FAIL because current commands mutate `toml_edit::DocumentMut`
directly and do not provide full-transaction validation or backups.

- [ ] **Step 3: Implement `ProviderConfigStore`.**

Read a TOML document, convert it to a typed registry, apply one mutation,
compile the full registry, render v3 TOML, and atomically write it with
owner-only permissions. On `migrate_to_v3`, write a sibling
`<config>.v2-backup-<unix-seconds>` before the replacement.

- [ ] **Step 4: Give mutations stable error types.**

`ProviderStoreError` distinguishes unreadable file, invalid TOML, invalid
registry, missing record, referenced record, write failure, and backup failure.
It must not put secret values in error strings.

- [ ] **Step 5: Run store and file-store tests.**

Run: `cargo test --locked --test provider_store_contract && cargo test --locked infrastructure::file_store`

Expected: PASS; invalid mutations leave the byte-for-byte original intact.

- [ ] **Step 6: Commit the configuration store.**

```bash
git add src/provider/store.rs src/provider/config.rs src/infrastructure/file_store.rs \
  tests/provider_store_contract.rs
git commit -m "feat: add validated provider config transactions"
```

### Task 5: Harden provider credential storage and credential rotation inputs

**Files:**

- Modify: `src/provider/credentials.rs`
- Modify: `src/provider/domain.rs`
- Modify: `src/provider/store.rs`
- Test: `tests/provider_credential_contract.rs`
- Test: `src/provider/credentials.rs`

**Interfaces:**

- Produce `CredentialStatus { id, provider_id, source, auth_scheme, available }`.
- Produce `CredentialResolver::resolve(&Credential) -> Result<SecretString, CredentialError>`.
- Produce `CredentialSet` ordered by the alias candidate preference and
  provider-local rotation index.

- [ ] **Step 1: Write redaction and ownership tests.**

```rust
#[test]
fn credential_status_never_contains_managed_secret_value() {
    let status = configured_managed_credential("bai-main", "super-secret");
    let text = serde_json::to_string(&status).unwrap();
    assert!(!text.contains("super-secret"));
}

#[test]
fn a_candidate_cannot_resolve_a_foreign_provider_credential() {
    let error = resolve_for_candidate("kilo", "bai-main").unwrap_err();
    assert!(error.to_string().contains("belongs to provider bai"));
}
```

- [ ] **Step 2: Run the credential contract tests.**

Run: `cargo test --locked --test provider_credential_contract`

Expected: FAIL because status availability and candidate-scoped resolution are
not first-class interfaces.

- [ ] **Step 3: Implement source resolution and safe status.**

Support `env`, `file`, and `managed` sources. Trim file/stdin values, reject
empty values, and write managed values only to the existing owner-only
`provider-secrets.json` store. Do not introduce a mandatory OS keychain;
retain the isolated optional backend trait.

- [ ] **Step 4: Define rotation without global secret leakage.**

The planner asks a resolver for eligible credential IDs. The resolver returns
only a secret for the selected attempt and never returns all secrets as a
serializable collection. A credential cooldown key is `(provider_id,
credential_id)`.

- [ ] **Step 5: Run provider credentials and client API-key separation tests.**

Run: `cargo test --locked --test provider_credential_contract && cargo test --locked --lib provider::credentials && cargo test --locked --lib api_key`

Expected: PASS; no upstream credential enters `ApiKeyRegistry`.

- [ ] **Step 6: Commit credential hardening.**

```bash
git add src/provider/credentials.rs src/provider/domain.rs src/provider/store.rs \
  tests/provider_credential_contract.rs
git commit -m "feat: harden provider credential resolution"
```

### Task 6: Make catalog discovery and adapters provider-local

**Files:**

- Modify: `src/provider/catalog.rs`
- Move: `src/provider/adapters/mod.rs` → `src/provider/adapter.rs`
- Modify: `src/provider/adapters/{openai_compatible,bai,kilo,opencode}.rs`
- Modify: `src/application/prober.rs`
- Test: `tests/provider_catalog_adapter_contract.rs`

**Interfaces:**

- Produce `CatalogEntry { provider_id, model_id, metadata, source, checked_at }`.
- Produce `CatalogRepository::replace(provider_id, entries)` and
  `CatalogRepository::eligible(alias)`.
- Produce `ProviderAdapter::prepare` and `ProviderAdapter::classify_failure`.
- Generic OpenAI-compatible adapter accepts per-provider protocol/auth config.

- [ ] **Step 1: Write two-provider fixture tests.**

```rust
#[tokio::test]
async fn identical_model_ids_keep_endpoint_and_auth_isolated() {
    let (bai, kilo) = two_provider_fixture_servers().await;
    discover(&bai.provider()).await.unwrap();
    discover(&kilo.provider()).await.unwrap();
    assert_eq!(catalog().get("bai", "model-x").unwrap().provider_id, "bai".into());
    assert_eq!(catalog().get("kilo", "model-x").unwrap().provider_id, "kilo".into());
    assert_ne!(bai.received_auth().await, kilo.received_auth().await);
}

#[test]
fn generic_openai_adapter_uses_configured_auth_scheme() {
    let request = generic_request(AuthScheme::XApiKey);
    assert_eq!(request.headers["x-api-key"], "test-key");
    assert!(!request.headers.contains_key("Authorization"));
}
```

- [ ] **Step 2: Run the catalog/adapter contract tests.**

Run: `cargo test --locked --test provider_catalog_adapter_contract`

Expected: FAIL because catalog cache and adapter lookup do not expose the
full provider-local contract.

- [ ] **Step 3: Implement catalog cache keys and metadata rules.**

Key a catalog entry by provider ID and model ID. Cache endpoint fingerprint,
credential availability fingerprint, `checked_at`, and verified capabilities.
An explicit operator override can fill a missing context window but remains
`verified_context = false` until a verification command records proof.

- [ ] **Step 4: Centralize adapter failure classification.**

Return `FailureClass::{Authentication, RateLimited { retry_after },
UnsupportedModel, ProviderUnavailable, Transport, Timeout, ContextLimit,
PaymentRequired, ClientRequest}`. Parse `Retry-After` once and cap it with the
existing safe duration policy.

- [ ] **Step 5: Run existing provider HTTP fixtures and the new contracts.**

Run: `cargo test --locked --test provider_catalog_adapter_contract --test provider_management_http --test provider_fallback_fixtures`

Expected: PASS with provider-local URL, header, and model assertions.

- [ ] **Step 6: Commit catalog and adapter changes.**

```bash
git add src/provider src/application/prober.rs tests/provider_catalog_adapter_contract.rs \
  tests/provider_management_http.rs tests/provider_fallback_fixtures.rs
git commit -m "refactor: isolate provider catalog and adapters"
```

### Task 7: Implement pure route planning and 429-aware resilience state

**Files:**

- Modify: `src/provider/routing.rs`
- Create: `src/provider/resilience.rs`
- Create: `src/provider/runtime.rs`
- Modify: `src/provider/registry.rs`
- Test: `tests/provider_routing_resilience_contract.rs`

**Interfaces:**

- Produce `RouteExplanation { alias, eligible, excluded, selected_order }`.
- Produce `RouteState`, `RouteKey`, `RouteHealth`, `FailureClass`, and
  `RouteAction::{UseCredential, RotateCredential, NextCandidate, Wait, Fail}`.
- Produce `ProviderRuntimeSnapshot` and `ProviderRuntimeHandle::replace`.

- [ ] **Step 1: Write deterministic resilience tests with frozen time.**

```rust
#[test]
fn 429_rotates_credential_then_moves_to_next_provider_candidate() {
    let mut state = RouteState::new(test_clock());
    state.record_failure(key("bai", "bai-main"), rate_limited_secs(30));
    assert_eq!(state.action_for(candidate("bai", "bai-main")), RouteAction::RotateCredential);
    assert_eq!(state.action_for(candidate("bai", "bai-backup")), RouteAction::UseCredential);
    state.record_failure(key("bai", "bai-backup"), rate_limited_secs(30));
    assert_eq!(state.next_target(&route("free-1m")), Some(target("kilo", "kilo-free")));
}

#[test]
fn explain_lists_sub_million_candidate_as_excluded() {
    let explanation = planner().explain("free-1m", request_context(900_000)).unwrap();
    assert!(explanation.excluded.iter().any(|entry| entry.reason == ExclusionReason::InsufficientContext));
}
```

- [ ] **Step 2: Run the pure routing/resilience tests.**

Run: `cargo test --locked --test provider_routing_resilience_contract`

Expected: FAIL because current `RetryDecision` has no stateful cooldown or
route explanation.

- [ ] **Step 3: Implement candidate filtering and explanation.**

Filter candidates in this order: provider enabled, model exists, capability
matches protocol/tools/streaming, context is sufficient, model is not marked
unsupported, provider is not cooling, then credential is not cooling. Keep an
explicit exclusion reason for every removed candidate.

- [ ] **Step 4: Implement transient cooldown policy.**

Store health by provider and credential key. A 429 uses bounded `Retry-After`;
401/403 quarantines the credential; 404 disables the model candidate until
catalog refresh; 5xx/transport uses a short provider cooldown. Do not persist
this state across process restart.

- [ ] **Step 5: Run routing, registry, and 1M contract tests.**

Run: `cargo test --locked --test provider_routing_resilience_contract --test provider_management_core --test provider_management_contract`

Expected: PASS; strict aliases cannot select unverified or sub-1M targets.

- [ ] **Step 6: Commit routing resilience.**

```bash
git add src/provider/routing.rs src/provider/resilience.rs src/provider/runtime.rs \
  src/provider/registry.rs tests/provider_routing_resilience_contract.rs
git commit -m "feat: add provider-aware route resilience"
```

### Task 8: Move provider execution out of the legacy retry loop

**Files:**

- Create: `src/gateway/mod.rs`
- Create: `src/gateway/provider_execute.rs`
- Create: `src/gateway/provider_stream.rs`
- Modify: `src/opencode/retry/execute/mod.rs`
- Modify: `src/state.rs`
- Test: `tests/provider_execution_matrix.rs`
- Test: `tests/fallback_api_opencode_matrix.rs`

**Interfaces:**

- Produce `execute_provider_targets(state, routing_key, request, snapshot)`.
- Produce `AttemptOutcome::{Response, PreCommitFailure, StreamCommittedFailure}`.
- `AppState` owns `ProviderRuntimeHandle` and `Arc<RwLock<RouteState>>` when
  schema-v3 provider routing is active.

- [ ] **Step 1: Write execution matrix tests.**

```rust
#[tokio::test]
async fn rate_limited_bai_falls_back_to_kilo_before_same_provider_retry() {
    let fixture = matrix()
        .bai_status(429, Some("30"))
        .kilo_success("ok");
    let response = fixture.send_non_streaming("sonnet[1m]").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.requests().await, vec!["bai", "kilo"]);
}

#[tokio::test]
async fn stream_falls_back_only_before_first_content_event() {
    let before = streaming_fixture().first_target_status(503).second_target_sse("text");
    assert_eq!(before.send().await.status(), StatusCode::OK);
    let after = streaming_fixture().first_target_sse_then_disconnect();
    assert!(after.send().await.body_contains_terminal_error());
    assert_eq!(after.requested_providers().await, vec!["bai"]);
}
```

- [ ] **Step 2: Run the new execution matrix.**

Run: `cargo test --locked --test provider_execution_matrix`

Expected: FAIL because schema-v2 execution still delegates to a loop that
combines key rotation, model switching, and provider transport.

- [ ] **Step 3: Implement non-streaming execution.**

For each planner-selected target, resolve exactly one credential, let the
adapter prepare the request, send it, classify the outcome, update `RouteState`,
and request the next action. Do not reuse a provider endpoint when switching to
a candidate belonging to another provider.

- [ ] **Step 4: Implement streaming commit boundaries.**

Buffer only headers and the first event boundary. Before first content, a
typed upstream failure can advance a candidate. Once a content event passes to
the client, mark `stream_committed = true` and never issue a second upstream
request for that client stream.

- [ ] **Step 5: Preserve the legacy retry loop and run both matrices.**

Run: `cargo test --locked --test provider_execution_matrix --test fallback_api_opencode_matrix --test stream_retry_gates --test protocol_conformance`

Expected: PASS. Legacy singleton tests continue to exercise the old branch;
schema-v3 tests exercise `gateway/provider_execute.rs`.

- [ ] **Step 6: Commit provider execution integration.**

```bash
git add src/gateway src/opencode/retry/execute/mod.rs src/state.rs \
  tests/provider_execution_matrix.rs tests/fallback_api_opencode_matrix.rs \
  tests/stream_retry_gates.rs tests/protocol_conformance.rs
git commit -m "feat: execute provider routes with cross-provider fallback"
```

### Task 9: Expose daemon provider runtime, health, and safe metrics

**Files:**

- Modify: `src/management/dto.rs`
- Modify: `src/management/service.rs`
- Modify: `src/management/mod.rs`
- Modify: `src/rest_api.rs`
- Modify: `src/observability.rs`
- Modify: `src/server/routes.rs`
- Test: `tests/provider_runtime_api_contract.rs`

**Interfaces:**

- Produce authenticated `GET /api/v1/provider-runtime`.
- Produce `ProviderRuntimeDto`, `RouteHealthDto`, and `RouteAttemptMetrics`.
- Add safe counters: credential rotations, cross-provider fallbacks, cooldowns,
  unsupported-model exclusions, and stream-committed failures.

- [ ] **Step 1: Write management API redaction and truth tests.**

```rust
#[tokio::test]
async fn runtime_endpoint_returns_daemon_alias_and_never_secret() {
    let body = get_runtime(app_with_runtime("free-1m", "top-secret")).await;
    assert_eq!(body["runtime_alias"], "free-1m");
    assert_eq!(body["active_candidate"]["provider"], "bai");
    assert!(!body.to_string().contains("top-secret"));
}

#[tokio::test]
async fn runtime_endpoint_requires_management_auth() {
    assert_eq!(get_without_token(app()).await.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run the management API contract test.**

Run: `cargo test --locked --test provider_runtime_api_contract`

Expected: FAIL because no provider runtime endpoint exists.

- [ ] **Step 3: Add DTOs and route handler.**

Expose configured alias, runtime alias, snapshot fingerprint, active/last
candidate, candidate health, cooldown timestamp, and safe counters. Do not
expose base authorization headers, secrets, raw error bodies, or prompt data.

- [ ] **Step 4: Record attempt metrics at the execution boundary.**

Increment counters only after an attempt result is classified. Associate all
counters with `provider_id`, `model_id`, alias, candidate index, and a hashed
credential identifier, never the credential value.

- [ ] **Step 5: Run management/OpenAPI/observability tests.**

Run: `cargo test --locked --test provider_runtime_api_contract --lib management --lib observability`

Expected: PASS; the OpenAPI registry includes the new DTO and auth is enforced.

- [ ] **Step 6: Commit runtime observability.**

```bash
git add src/management src/rest_api.rs src/server/routes.rs src/observability.rs \
  tests/provider_runtime_api_contract.rs
git commit -m "feat: expose safe provider runtime health"
```

### Task 10: Implement provider-management command services and new CLI groups

**Files:**

- Create: `src/command/provider.rs`
- Create: `src/command/credential.rs`
- Create: `src/command/model.rs`
- Create: `src/command/alias.rs`
- Create: `src/command/route.rs`
- Modify: `src/command/health.rs`
- Modify: `src/cli/provider.rs`
- Modify: `src/command/mod.rs`
- Modify: `src/app/providers.rs`
- Test: `tests/provider_cli_contract.rs`
- Test: `tests/cli_e2e.sh`

**Interfaces:**

- Every service accepts `&CommandContext` and returns a serializable response
  struct or `CommandError`.
- `route explain` is offline and consumes only a config registry plus current
  daemon health when available.
- `route test` uses fixture/integration endpoints only in tests; real command
  probes the selected provider explicitly and never submits a model completion
  unless the user invokes it.

- [ ] **Step 1: Write end-to-end CLI contract tests.**

```rust
#[test]
fn provider_add_and_alias_set_write_v3_records() {
    let root = TestCli::new();
    root.run(["provider", "add", "bai", "--base-url", "https://b.ai/v1"])
        .assert_success();
    root.run(["alias", "set", "free-1m", "--client-model", "sonnet[1m]",
              "--context-window", "1000000", "--strict-context",
              "--candidate", "bai:deepseek:bai-main:10"])
        .assert_success();
    assert!(root.config_text().contains("[aliases.free-1m]"));
}

#[test]
fn route_explain_json_is_secret_free_and_machine_readable() {
    let output = TestCli::configured().json(["route", "explain", "free-1m"]);
    assert_eq!(output["alias"], "free-1m");
    assert!(output["eligible"].is_array());
    assert!(!output.to_string().contains("secret"));
}
```

- [ ] **Step 2: Run the CLI contract tests.**

Run: `cargo test --locked --test provider_cli_contract`

Expected: FAIL because provider operations still live under the old nested
`provider credential|model|alias` parser and render directly.

- [ ] **Step 3: Implement one noun service per file.**

`provider.rs` uses `ProviderConfigStore` for list/show/add/remove/enable/disable;
`credential.rs` reads stdin or source references; `model.rs` owns
discover/list/show/verify; `alias.rs` owns list/show/set/remove/use;
`route.rs` owns explain/simulate; and `health.rs` queries daemon runtime first.
Every mutation calls one store transaction and returns `reloaded` or
`restart_required` explicitly.

- [ ] **Step 4: Replace direct TOML mutation in `src/app/providers.rs`.**

Route all canonical commands through `src/command/*`. Keep the file only as a
temporary call-through until Task 11 removes it. A service does not call
`println!`; it returns data to `CliRenderer`.

- [ ] **Step 5: Run focused CLI, output mode, and existing shell E2E tests.**

Run: `cargo test --locked --test provider_cli_contract --test cli_rearchitecture_contract && bash tests/cli_e2e.sh debug`

Expected: PASS for human, quiet, and JSON modes; all new command groups appear
in help exactly once.

- [ ] **Step 6: Commit CLI services.**

```bash
git add src/command src/cli src/app/providers.rs tests/provider_cli_contract.rs tests/cli_e2e.sh
git commit -m "feat: add provider management command groups"
```

### Task 11: Add explicit migration, legacy adapters, and operator documentation

**Files:**

- Create: `src/legacy/mod.rs`
- Create: `src/legacy/config.rs`
- Create: `src/legacy/commands.rs`
- Modify: `src/app/models.rs`
- Modify: `src/app/providers.rs`
- Modify: `docs/cli.md`
- Modify: `docs/configuration.md`
- Modify: `docs/provider-management.md`
- Modify: `docs/troubleshooting.md`
- Modify: `examples/provider-template.toml`
- Test: `tests/provider_migration_compatibility.rs`
- Test: `tests/claude_code_e2e.py`

**Interfaces:**

- Produce `LegacyProviderConfig::into_registry()`.
- Produce `translate_legacy_command(command) -> CanonicalCommand`.
- `config migrate --write` returns `{ from_version, to_version, backup_path,
  restart_required }`.

- [ ] **Step 1: Write migration and legacy command tests.**

```rust
#[test]
fn legacy_singleton_loads_as_one_synthetic_provider_route() {
    let runtime = load_legacy("upstream_base_url = 'https://api.example/v1'\nmodel = 'x'\n");
    assert_eq!(runtime.registry.providers().count(), 1);
    assert_eq!(runtime.registry.resolve_alias("legacy-default").unwrap().len(), 1);
}

#[test]
fn legacy_provider_api_prints_deprecation_only_in_human_mode() {
    assert!(run_human(["provider", "api", "https://x/v1", "x"]).stderr.contains("deprecated"));
    assert!(!run_json(["provider", "api", "https://x/v1", "x"]).stdout.contains("deprecated"));
}
```

- [ ] **Step 2: Run the compatibility tests.**

Run: `cargo test --locked --test provider_migration_compatibility`

Expected: FAIL because current legacy/synthetic conversions and v3 migration
responses do not share a compatibility boundary.

- [ ] **Step 3: Implement explicit v1/v2 conversion and command translation.**

Convert legacy singleton settings in memory to a synthetic provider,
credential-reference set, model, and `legacy-default` alias. Translate old
commands to canonical service calls while retaining their documented exit
codes. Do not silently write a v3 file when a legacy command executes.

- [ ] **Step 4: Document exact migration and 429 diagnosis procedures.**

Add a v1/v2/v3 comparison, commands for creating a cross-provider 1M alias,
the difference between model fallback and provider fallback, and how to use
`config show --file`, `config show --effective`, `server status`, `health`,
and `route explain` to diagnose state drift.

- [ ] **Step 5: Run migration and Claude Code verification.**

Run: `cargo test --locked --test provider_migration_compatibility && python3 tests/claude_code_e2e.py --provider-alias free-1m`

Expected: PASS; the installed Claude Code verification proves the active 1M
alias reports the configured context window.

- [ ] **Step 6: Commit compatibility and docs.**

```bash
git add src/legacy src/app/models.rs src/app/providers.rs docs examples \
  tests/provider_migration_compatibility.rs tests/claude_code_e2e.py
git commit -m "feat: migrate provider config through canonical services"
```

### Task 12: Remove superseded provider paths and verify the full repository

**Files:**

- Delete: `src/app/providers.rs` after all callers use `src/command/*`
- Modify: `src/opencode/retry/execute/mod.rs`
- Modify: `src/provider/mod.rs`
- Modify: `README.md`
- Modify: `docs/architecture/README.md`
- Test: `tests/fallback_api_opencode_matrix.rs`
- Test: `tests/provider_execution_matrix.rs`
- Test: `tests/cli_e2e.sh`

**Interfaces:**

- The only schema-v3 request path is `gateway::provider_execute` and
  `gateway::provider_stream`.
- The only canonical provider mutation path is `ProviderConfigStore` through a
  `command/*` service.
- The legacy loop remains only behind `LegacyProviderConfig` detection.

- [ ] **Step 1: Add a structural regression test.**

```rust
#[test]
fn schema_v3_execution_never_calls_legacy_model_retry_list() {
    let trace = schema_v3_fixture().send("sonnet[1m]").await.trace();
    assert!(trace.iter().all(|event| event.component != "legacy_retry"));
    assert!(trace.iter().any(|event| event.component == "provider_execute"));
}
```

- [ ] **Step 2: Run the structural test.**

Run: `cargo test --locked --test provider_execution_matrix schema_v3_execution_never_calls_legacy_model_retry_list`

Expected: FAIL until stale schema-v3 retry calls are removed.

- [ ] **Step 3: Delete only unreachable duplicate code.**

Remove `src/app/providers.rs` after `rg 'app::providers|app/providers' src tests`
returns no callers. Remove schema-v3-specific selection branches from the
legacy retry loop, retaining the legacy singleton code path and its tests.

- [ ] **Step 4: Update architecture and CLI references.**

Ensure README and docs name `provider`, `credential`, `model`, `alias`,
`route`, `health`, and `config` commands. Mark old nested provider commands as
compatibility aliases and link the migration guide.

- [ ] **Step 5: Run the complete verification set.**

Run:

```bash
cargo fmt --check
cargo test --locked
bash tests/cli_e2e.sh debug
python3 tests/claude_code_e2e.py --provider-alias free-1m
bash scripts/check_docs.py
bash scripts/check_secrets.py
```

Expected: every command exits `0`; the test suite has no failures; secret scan
reports no credential values in tracked files.

- [ ] **Step 6: Commit the completed rearchitecture.**

```bash
git add -A
git commit -m "refactor: unify provider runtime and CLI"
```

## Requirement Coverage Review

| Spec requirement | Implemented by |
| --- | --- |
| Focused CLI parser/service/renderer | Tasks 1, 2, 10 |
| Provider config without code for generic APIs | Tasks 3, 4, 6, 10 |
| Secret isolation | Tasks 4, 5, 9 |
| Cross-provider 429 recovery | Tasks 7, 8 |
| Strict 1M aliases and 80% compaction for every alias | Tasks 3, 7, 8, 11 |
| Daemon truth versus caller environment | Tasks 2, 9, 10 |
| v1/v2 compatibility and atomic v3 migration | Tasks 3, 4, 11 |
| Human/quiet/JSON contracts | Tasks 1, 10, 12 |
| Full protocol, CLI, and Claude Code verification | Tasks 8, 10, 11, 12 |
