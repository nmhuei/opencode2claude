# Provider Management and Claude Code 1M Alias Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the singleton upstream model/key flow with provider-scoped management, stable Claude Code aliases, and strict same-context 1M fallback.

**Architecture:** Add a provider domain and registry underneath the existing CLI, HTTP handlers, and retry engine. Claude Code receives a known `[1m]` compatibility model identity, while the registry resolves it to an upstream provider/model/credential target. Provider adapters own protocol, endpoint, authentication, and model-ID behavior; the retry engine only executes validated `AttemptTarget` values.

**Tech Stack:** Rust 2021, Tokio, Axum, Reqwest, Clap, Serde, TOML/TOML-edit, existing `AtomicFileStore`, existing SQLite history, existing streaming/retry infrastructure.

**Spec:** `docs/superpowers/specs/2026-09-02-provider-management-1m-design.md`

## Global Constraints

- Do not spawn or manage OpenCode/Kilo CLI processes; integrate their HTTP gateways only.
- Keep upstream provider credentials separate from client API keys in `src/api_key`.
- Never persist raw upstream API keys in the main TOML configuration.
- The strict 1M alias accepts only candidates with `context_window >= 1_000_000`.
- The 1M alias must expose `CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000`, `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=80`, and `CLAUDE_CODE_DISABLE_1M_CONTEXT=0`; `CLAUDE_CODE_AUTO_COMPACT_WINDOW` must be unset because Claude Code treats it as the context ceiling.
- Streaming retries are allowed only before the first content event.
- Existing legacy provider commands remain functional until the migration is complete.
- Every task ends with targeted tests and a focused commit when this plan is executed.
- Do not claim `/context` is 1M without a black-box test against the installed Claude Code version.

## File Map

Create the provider domain under `src/provider/`:

- `types.rs`: IDs, provider kinds, protocols, credentials, models, aliases, candidates, and retry decisions.
- `config.rs`: version-2 TOML representation and conversion to domain objects.
- `registry.rs`: provider/credential/model/alias registry and compiled runtime snapshot.
- `credentials.rs`: secret references and provider-scoped secret resolution.
- `catalog.rs`: `/models` discovery, cache, metadata normalization, and catalog freshness.
- `health.rs`: live provider/model health and quarantine state.
- `routing.rs`: alias resolution and same-capability candidate selection.
- `adapters/mod.rs`: adapter trait and adapter lookup.
- `adapters/kilo.rs`: Kilo Gateway behavior.
- `adapters/opencode.rs`: OpenCode Zen behavior.
- `adapters/bai.rs`: B.AI protocol/auth behavior.
- `adapters/openai_compatible.rs`: generic OpenAI-compatible behavior.

Modify the existing integration seams:

- `src/lib.rs`: export `provider`.
- `src/config/file.rs`, `src/config/types.rs`, `src/config/loader.rs`: load schema version 2 and synthesize legacy providers.
- `src/application/models.rs`: delegate model metadata to the provider catalog instead of global name heuristics.
- `src/application/integration.rs`, `src/app/mod.rs`, `src/app/view.rs`: export the client alias and 1M environment contract.
- `src/opencode/retry/execute/mod.rs`, `src/opencode/retry/policy.rs`: execute `AttemptTarget` values and enforce strict context filtering.
- `src/opencode/forward/sync.rs`, `src/opencode/forward/stream/*`: preserve client model identity while recording the effective upstream model.
- `src/state.rs`, `src/runtime.rs`: hold and atomically replace the compiled provider snapshot.
- `src/cli.rs`, `src/app/models.rs`: add canonical provider/credential/alias commands while preserving aliases.
- `src/management/dto.rs`, `src/management/service.rs`, `src/rest_api.rs`: expose provider management and runtime state.
- `src/dashboard/control/catalog.rs`: read active aliases and catalogs from the provider registry.
- `src/history/store/*`: record requested alias, route alias, provider, effective model, and context class.

## Task 1: Establish the baseline and executable 1M contract

**Files:**

- Create: `tests/provider_management_contract.rs`
- Modify: `src/application/integration.rs:30-150`
- Modify: `src/application/models.rs:1-60`
- Modify: `src/app/mod.rs:129-170`
- Test: `tests/claude_code_e2e.py`

**Interfaces:**

- Produce `ModelProfile::client_model_alias() -> &str`.
- Produce `ModelProfile::from_context(id, context_window, max_output_tokens, supports_thinking) -> ModelProfile`.
- Produce `model_claude_code_vars(profile: &ModelProfile) -> Vec<(&'static str, String)>` with the 1M variables.
- Produce a black-box test command that starts the local gateway and verifies Claude Code model/context output.

- [ ] **Step 1: Write a failing unit test for the 1M profile.**

```rust
#[test]
fn one_million_profile_uses_a_known_1m_client_identity() {
    let profile = ModelProfile::from_context("route/free-1m", 1_000_000, 128_000, true);
    assert_eq!(profile.client_model_alias(), "claude-sonnet-5[1m]");
    let vars = model_claude_code_vars(&profile);
    assert!(vars.iter().any(|(key, value)| {
        *key == "CLAUDE_CODE_MAX_CONTEXT_TOKENS" && value == "1000000"
    }));
    assert!(vars.iter().any(|(key, value)| {
        *key == "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE" && value == "80"
    }));
}
```

- [ ] **Step 2: Run the focused test and verify it fails.**

Run: `cargo test --lib application::models::tests::one_million_profile_uses_a_known_1m_client_identity`

Expected: FAIL because the current profile API returns the hard-coded `claude-opus-5` identity and does not expose the new constructor.

- [ ] **Step 3: Implement the profile contract.**

Add a client alias field or resolver to `ModelProfile`. For context windows at or above 1M, return the configured compatibility identity `claude-sonnet-5[1m]`; for smaller models return `claude-sonnet-5`. Keep the upstream model ID separate.

- [ ] **Step 4: Add the Claude Code black-box smoke test.**

The test must:

1. Start the test gateway on an isolated port.
2. Set `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_MODEL=claude-sonnet-5[1m]`, and the three context variables.
3. Invoke the installed `claude` binary with a non-network `/context`-capable session fixture.
4. Assert that the displayed model identity contains `[1m]` and the denominator contains `1m` or `1000000`.
5. Mark the test unavailable with a clear diagnostic if Claude Code is not installed.

- [ ] **Step 5: Run the focused unit and smoke tests.**

Run: `cargo test --lib application::models application::integration`

Run: `python3 tests/claude_code_e2e.py --provider-alias free-1m`

Expected: PASS on the supported Claude Code version, or a compatibility failure that names the detected version and model identity.

- [ ] **Step 6: Commit the client contract.**

```bash
git add src/application/models.rs src/application/integration.rs src/app/mod.rs tests/provider_management_contract.rs tests/claude_code_e2e.py
git commit -m "feat: define Claude Code 1M provider alias contract"
```

## Task 2: Add provider domain types and registry

**Files:**

- Create: `src/provider/mod.rs`
- Create: `src/provider/types.rs`
- Create: `src/provider/registry.rs`
- Modify: `src/lib.rs:12-40`
- Test: `src/provider/registry.rs`

**Interfaces:**

- Produce `ProviderId`, `CredentialId`, `AliasId`, `Provider`, `Credential`, `ModelInfo`, `ModelAlias`, `ModelCandidate`, and `AttemptTarget`.
- Produce `ProviderRequest`, `ProviderHttpRequest`, `ProviderResponse`, and `NormalizedResponse` as the adapter boundary types.
- Produce `ProviderRegistry::new()`, `ProviderRegistry::register_provider`, `ProviderRegistry::register_alias`, `ProviderRegistry::resolve_alias`, and `ProviderRegistry::compile_snapshot`.

- [ ] **Step 1: Write failing tests for provider/model separation.**

```rust
#[test]
fn identical_model_ids_from_two_providers_remain_distinct() {
    let mut registry = ProviderRegistry::new();
    registry.insert_model(ModelInfo::test("bai", "model-x", 1_000_000));
    registry.insert_model(ModelInfo::test("kilo", "model-x", 128_000));
    assert_eq!(registry.model("bai", "model-x").unwrap().context_window, 1_000_000);
    assert_eq!(registry.model("kilo", "model-x").unwrap().context_window, 128_000);
}
```

- [ ] **Step 2: Run the test and verify it fails.**

Run: `cargo test --lib provider::registry::identical_model_ids_from_two_providers_remain_distinct`

Expected: FAIL because the `provider` module and registry do not exist.

- [ ] **Step 3: Implement the pure domain types.**

Use owned `String` values in runtime state. Derive `Debug`, `Clone`, `Serialize`, `Deserialize`, `PartialEq`, and `Eq` where the type is persisted or tested. Keep secrets out of all `Debug` and `Serialize` implementations.

- [ ] **Step 4: Implement alias resolution and snapshot compilation.**

`compile_snapshot` must sort candidates by priority, validate provider references, validate credentials belong to the referenced provider, and reject a strict 1M alias containing a candidate with an unknown or sub-1M context window.

- [ ] **Step 5: Run provider unit tests.**

Run: `cargo test --lib provider`

Expected: PASS with validation errors covering missing provider, missing model, cross-provider credential, and strict-context violations.

- [ ] **Step 6: Commit the domain layer.**

```bash
git add src/lib.rs src/provider
git commit -m "feat: add provider registry domain model"
```

## Task 3: Implement provider-scoped credential storage

**Files:**

- Create: `src/provider/credentials.rs`
- Modify: `src/provider/mod.rs`
- Modify: `src/infrastructure/file_store.rs:1-70`
- Test: `src/provider/credentials.rs`

**Interfaces:**

- Produce `SecretSource`, `AuthScheme`, `CredentialStore`, `CredentialStore::resolve`, `CredentialStore::put_file_secret`, and `CredentialStore::remove`.
- Consume `AtomicFileStore` and `Credential.provider_id`.

- [ ] **Step 1: Write failing tests for reference resolution and permissions.**

```rust
#[cfg(unix)]
#[test]
fn file_secret_is_atomic_and_owner_only() {
    let store = test_store();
    store.put_file_secret("bai_main", "secret-value").unwrap();
    assert_eq!(store.resolve("bai_main").unwrap().expose(), "secret-value");
    assert_eq!(file_mode(store.path("bai_main")) & 0o777, 0o600);
}

#[test]
fn credential_provider_mismatch_is_rejected() {
    let credential = credential("bai_main", "bai");
    assert!(credential.belongs_to("kilo").is_err());
}
```

- [ ] **Step 2: Run the focused test and verify it fails.**

Run: `cargo test --lib provider::credentials`

Expected: FAIL because credential storage does not exist.

- [ ] **Step 3: Implement environment and secure-file sources.**

Use `AtomicFileStore::atomic_write(..., true)`. Store a JSON map of credential IDs to secret values only in the fallback secure file. Use `SecretString` for all resolved values. Do not print or serialize secret contents.

- [ ] **Step 4: Add keychain abstraction without making it mandatory.**

Define the `KeychainBackend` trait and a disabled backend that returns a typed unavailable error. The first working backend is environment plus secure file; the trait keeps OS keychain integration isolated from provider logic.

- [ ] **Step 5: Verify existing client API-key behavior remains separate.**

Run: `cargo test --lib api_key`

Expected: PASS; no provider credential is imported into `ApiKeyRegistry`.

- [ ] **Step 6: Commit credential storage.**

```bash
git add src/provider/credentials.rs src/provider/mod.rs src/infrastructure/file_store.rs
git commit -m "feat: add provider-scoped credential storage"
```

## Task 4: Add schema-v2 configuration and legacy conversion

**Files:**

- Create: `src/provider/config.rs`
- Modify: `src/config/file.rs:10-120`
- Modify: `src/config/types.rs:1-180`
- Modify: `src/config/loader.rs:1-300`
- Modify: `src/config/mod.rs:1-80`
- Test: `src/config/tests.rs`

**Interfaces:**

- Produce `ProviderFileConfig`, `AliasFileConfig`, `CandidateFileConfig`, `ProviderConfigError`, and `ProviderRegistry::from_legacy(base_url, model, credentials) -> ProviderRegistry`.
- Produce `load_provider_registry(config_path: &Path) -> Result<ProviderRegistry, ProviderConfigError>`.
- Preserve `BridgeConfig::from_env_and_cli` as the public bootstrap entry point.

- [ ] **Step 1: Write failing parser tests.**

```rust
#[test]
fn schema_v2_parses_provider_credentials_and_strict_alias() {
    let registry = parse_fixture(SCHEMA_V2_FIXTURE).unwrap();
    let alias = registry.alias("free-1m").unwrap();
    assert_eq!(alias.client_model, "claude-sonnet-5[1m]");
    assert_eq!(alias.context_window, 1_000_000);
    assert!(alias.strict_context);
}

#[test]
fn legacy_singleton_config_becomes_one_provider_and_alias() {
    let registry = ProviderRegistry::from_legacy("https://example.test/v1", "model-x", None);
    assert_eq!(registry.providers().len(), 1);
    assert_eq!(registry.aliases().len(), 1);
}
```

- [ ] **Step 2: Run parser tests and verify they fail.**

Run: `cargo test --lib config::tests::schema_v2_parses_provider_credentials_and_strict_alias`

Expected: FAIL because schema-v2 sections are not recognized.

- [ ] **Step 3: Implement schema-v2 deserialization and validation.**

Reject duplicate IDs, invalid URLs, unknown provider kinds, empty alias candidates, cross-provider credentials, and strict aliases with missing context metadata. Keep the existing TOML loader precedence for legacy fields.

- [ ] **Step 4: Implement legacy conversion.**

Convert `upstream_base_url`, `upstream_api_key`, `upstream_api_keys`, and `model` to a synthetic provider and alias without changing current behavior. Do not silently convert malformed TOML into the OpenCode default; return a typed configuration error.

- [ ] **Step 5: Run configuration tests.**

Run: `cargo test --lib config`

Expected: PASS for schema-v2, legacy conversion, malformed TOML, and precedence tests.

- [ ] **Step 6: Commit configuration migration.**

```bash
git add src/provider/config.rs src/config
git commit -m "feat: add provider management configuration schema"
```

## Task 5: Implement catalog discovery and 1M capability validation

**Files:**

- Create: `src/provider/catalog.rs`
- Create: `src/provider/health.rs`
- Modify: `src/application/prober.rs:1-520`
- Modify: `src/application/models.rs:1-360`
- Test: `tests/provider_catalog_fixtures.rs`

**Interfaces:**

- Produce `CatalogSource::Live`, `CatalogSource::Configured`, `ModelCatalog::refresh`, `ModelCatalog::get`, and `ModelCatalog::eligible_for_alias`.
- Produce `ProviderHealthState` and `HealthManager::check`.

- [ ] **Step 1: Create two independent HTTP fixture servers.**

Server A must return a credential-scoped catalog containing a DeepSeek 1M model and a 128K model. Server B must return a GLM 1M model. Each server must record request URL, auth headers, and model ID.

- [ ] **Step 2: Write failing catalog tests.**

```rust
#[tokio::test]
async fn strict_1m_catalog_excludes_128k_models() {
    let catalog = fixture_catalog().await;
    let eligible = catalog.eligible_for_alias("free-1m").unwrap();
    assert!(eligible.iter().all(|model| model.context_window >= 1_000_000));
    assert!(!eligible.iter().any(|model| model.model_id == "small-128k"));
}
```

- [ ] **Step 3: Implement provider-scoped catalog refresh.**

Cache by provider ID, endpoint, and credential fingerprint. Support providers that expose `/models` without auth and providers whose model list is credential-dependent. Preserve explicit context overrides when upstream metadata omits context size, but mark unverified values so strict aliases reject them.

- [ ] **Step 4: Move model profile resolution behind the catalog.**

Remove global substring-based capability decisions from the provider route. A model ID may receive provider-specific capability metadata; the same model string under another provider must not share a profile.

- [ ] **Step 5: Run catalog and prober tests.**

Run: `cargo test --test provider_catalog_fixtures`

Run: `cargo test --lib application::prober`

Run: `cargo test --lib application::models`

Expected: PASS with provider isolation, credential-scoped model lists, 1M filtering, and unknown-context rejection.

- [ ] **Step 6: Commit catalog support.**

```bash
git add src/provider/catalog.rs src/provider/health.rs src/application/prober.rs src/application/models.rs tests/provider_catalog_fixtures.rs
git commit -m "feat: add provider catalogs and strict context metadata"
```

## Task 6: Add provider protocol adapters

**Files:**

- Create: `src/provider/adapters/mod.rs`
- Create: `src/provider/adapters/kilo.rs`
- Create: `src/provider/adapters/opencode.rs`
- Create: `src/provider/adapters/bai.rs`
- Create: `src/provider/adapters/openai_compatible.rs`
- Test: `tests/provider_adapter_fixtures.rs`

**Interfaces:**

- Produce `ProviderAdapter`, `AdapterRegistry`, `KiloAdapter`, `OpenCodeAdapter`, `BaiAdapter`, and `OpenAiCompatibleAdapter`.
- Consume `Provider`, `Credential`, `ModelInfo`, `AttemptTarget`, and `ProviderRequest`.

- [ ] **Step 1: Write failing adapter tests.**

Each fixture must assert:

```text
Kilo: /api/gateway/chat/completions and Bearer/optional auth
OpenCode: configured protocol endpoint and exact wire model ID
B.AI: Bearer or x-api-key auth and /v1 protocol path
Generic: custom headers preserved without provider inference
```

- [ ] **Step 2: Run adapter tests and verify they fail.**

Run: `cargo test --test provider_adapter_fixtures`

Expected: FAIL because provider adapters do not exist.

- [ ] **Step 3: Implement adapter lookup by provider kind.**

Do not infer adapter type from hostname inside request execution. The configured provider kind selects the adapter.

- [ ] **Step 4: Implement request/response normalization.**

Keep `client_model` and `upstream_model_id` separate. Remove the client compatibility suffix `[1m]` before upstream model selection. Do not forward Anthropic-specific 1M compatibility markers to providers that do not support them.

- [ ] **Step 5: Run adapter tests and protocol conformance tests.**

Run: `cargo test --test provider_adapter_fixtures --test protocol_conformance`

Expected: PASS with exact endpoint, header, model, streaming, and response normalization assertions.

- [ ] **Step 6: Commit adapters.**

```bash
git add src/provider/adapters tests/provider_adapter_fixtures.rs
git commit -m "feat: add provider protocol adapters"
```

## Task 7: Replace model-only retry with strict route-target retry

**Files:**

- Modify: `src/opencode/retry/execute/mod.rs:180-620`
- Modify: `src/opencode/retry/policy.rs:39-120`
- Modify: `src/opencode/retry/response.rs`
- Modify: `src/opencode/forward/sync.rs:200-245`
- Modify: `src/opencode/forward/stream/execute.rs`
- Delete after migration: `src/opencode/retry/execute/route.rs`
- Test: `tests/provider_fallback_fixtures.rs`

**Interfaces:**

- Replace model-only retry construction with `RoutePlanner::plan(request, alias) -> Vec<AttemptTarget>`.
- Produce `RetryDecision::{RetrySameTarget, NextTarget, QuarantineCredential, QuarantineProvider, Fail}`.
- Preserve current metrics and cancellation behavior.

- [ ] **Step 1: Write failing two-provider fallback tests.**

```rust
#[tokio::test]
async fn one_m_alias_falls_from_deepseek_to_glm_without_using_128k() {
    let result = send_large_request("oc2api/free-1m").await;
    assert_eq!(result.effective_provider, "bai");
    assert_eq!(result.attempted_models, vec!["deepseek-1m", "glm-5.3-flash-1m"]);
    assert!(!result.attempted_models.contains(&"small-128k".to_string()));
}
```

- [ ] **Step 2: Run fallback tests and verify they fail.**

Run: `cargo test --test provider_fallback_fixtures`

Expected: FAIL because current retry builds one URL and changes only the model string.

- [ ] **Step 3: Implement route planning.**

Resolve alias candidates, calculate required context from input/output budgets, filter incompatible candidates, then produce ordered `AttemptTarget` values. Credential rotation happens only within the target provider.

- [ ] **Step 4: Implement adapter-based request execution.**

Use the target provider’s adapter for URL, auth, model ID, body normalization, and response decoding. Record target identity in retry metrics and history.

- [ ] **Step 5: Enforce streaming retry gates.**

Track whether any content event has been emitted. Retry only when the stream fails before content. Add tests for pre-content failure, post-content failure, and multi-candidate stream fallback.

- [ ] **Step 6: Remove duplicate inline route logic.**

After all call sites use the provider adapter path, remove the dead duplicate implementation in `src/opencode/retry/execute/route.rs` and keep one request preparation seam.

- [ ] **Step 7: Run the complete retry suite.**

Run: `cargo test --lib opencode::retry`

Run: `cargo test --test provider_fallback_fixtures`

Run: `cargo test --test stream_retry_gates`

Run: `cargo test --test retry_compat_livelock`

Expected: PASS with provider URL switching, credential isolation, strict 1M fallback, and streaming safety.

- [ ] **Step 8: Commit route-target retry.**

```bash
git add src/opencode/retry src/opencode/forward tests/provider_fallback_fixtures.rs
git commit -m "feat: route retries across provider targets"
```

## Task 8: Integrate runtime snapshots and launcher identity

**Files:**

- Modify: `src/state.rs:1-150`
- Modify: `src/runtime.rs`
- Modify: `src/app/mod.rs:100-170`
- Modify: `src/application/integration.rs:30-150`
- Modify: `src/app/server.rs`
- Test: `tests/provider_runtime_reload.rs`

**Interfaces:**

- Produce `ProviderRuntimeSnapshot` and `ProviderRuntimeHandle`.
- Produce `ProviderRuntimeHandle::load`, `ProviderRuntimeHandle::snapshot`, and `ProviderRuntimeHandle::replace`.
- Produce status fields for configured alias, runtime alias, active target, context window, and `restart_required`.

- [ ] **Step 1: Write a failing switch-while-running test.**

Start a server with alias A, update config to alias B, invoke the runtime reload path, and assert the next request uses B without restarting the process. If hot reload is not enabled by the implementation, assert `restart_required=true` and verify a controlled restart applies B.

- [ ] **Step 2: Run the test and verify current split-brain behavior.**

Run: `cargo test --test provider_runtime_reload`

Expected: FAIL because the current daemon owns an immutable `Arc<BridgeConfig>` and provider commands only rewrite files.

- [ ] **Step 3: Implement immutable compiled snapshots.**

Store the provider registry and route plan in a replaceable `Arc`. Requests capture one snapshot at start, so a reload cannot mutate an in-flight request halfway through.

- [ ] **Step 4: Update launcher environment generation.**

Set `ANTHROPIC_MODEL` to the alias’s `client_model`, not the upstream model. Set the context and auto-compact variables from the alias policy. Keep `OPENCODE_MODEL` as the upstream target only for compatibility output.

- [ ] **Step 5: Fix lifecycle identity.**

Make `server status`, `stop`, `restart`, and `logs` use the same resolved config/runtime identity as `server start`. Persist the resolved config path or instance ID in runtime metadata.

- [ ] **Step 6: Run runtime and launcher tests.**

Run: `cargo test --test provider_runtime_reload`

Run: `cargo test --lib application::integration`

Run: `cargo test --lib app::models`

Expected: PASS with no configured/runtime provider split-brain.

- [ ] **Step 7: Commit runtime integration.**

```bash
git add src/state.rs src/runtime.rs src/app src/application/integration.rs tests/provider_runtime_reload.rs
git commit -m "feat: apply provider aliases through runtime snapshots"
```

## Task 9: Add canonical provider, credential, alias, and health CLI

**Files:**

- Modify: `src/cli.rs:65-641`
- Modify: `src/app/models.rs:736-1070`
- Create: `src/app/providers.rs`
- Modify: `src/app/mod.rs:40-115`
- Test: `tests/cli_provider_management.sh`

**Interfaces:**

- Add `provider list`, `provider add`, `provider remove`, `provider status`, `provider health`, `provider models`, `provider activate`.
- Add `provider credential list|set|remove`.
- Add `provider alias list|show|set`.
- Keep `provider api`, `provider opencode`, and `upstream *` as compatibility aliases.
- Produce `ProviderActivateArgs { alias: String }` for the `provider activate <alias>` parser branch.

- [ ] **Step 1: Write parser tests.**

```rust
#[test]
fn provider_activate_accepts_alias_and_client_model_is_not_an_upstream_id() {
    let cli = parse(&["provider", "activate", "free-1m"]);
    let Command::Provider(args) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let Some(ProviderSubcommand::Activate(args)) = args.command else {
        panic!("expected provider activate");
    };
    assert_eq!(args.alias, "free-1m");
}
```

- [ ] **Step 2: Run parser tests and verify missing commands fail.**

Run: `cargo test --lib cli`

Expected: FAIL for the new subcommands.

- [ ] **Step 3: Implement CLI services through `ProviderRegistry`.**

Do not duplicate TOML mutation logic in each command. All commands call one provider management service that validates, stages, persists, and reloads the runtime snapshot.

- [ ] **Step 4: Implement safe output.**

Human output may show provider, alias, model, context, health, and credential status. JSON output may show credential IDs and fingerprints but never raw secret values.

- [ ] **Step 5: Add black-box CLI tests.**

Verify provider isolation, key input via stdin, JSON output, `--config` path behavior, `provider activate`, strict 1M rejection, and `restart_required` reporting.

- [ ] **Step 6: Run CLI tests and existing E2E tests.**

Run: `bash tests/cli_provider_management.sh`

Run: `bash tests/cli_e2e.sh debug`

Expected: provider management tests PASS; the two existing empty-pool failures are fixed or explicitly covered by the provider pool semantics.

- [ ] **Step 7: Commit the CLI.**

```bash
git add src/cli.rs src/app src/application tests/cli_provider_management.sh
git commit -m "feat: add provider management CLI"
```

## Task 10: Expose management API, dashboard catalog, and history

**Files:**

- Modify: `src/management/dto.rs`
- Modify: `src/management/service.rs`
- Modify: `src/rest_api.rs`
- Modify: `src/dashboard/control/catalog.rs`
- Modify: `src/history/store/types.rs`
- Modify: `src/history/store/capture.rs`
- Modify: `src/webui/app.js`
- Test: `tests/provider_management_http.rs`

**Interfaces:**

- Add `GET /api/v1/providers`.
- Add `GET /api/v1/providers/:id`.
- Add `GET /api/v1/providers/:id/models`.
- Add `POST /api/v1/providers/:id/health`.
- Add `GET /api/v1/aliases`.
- Add `POST /api/v1/aliases/:id/activate`.
- Extend status with configured/runtime alias and effective target.

- [ ] **Step 1: Write failing HTTP tests.**

Assert that API and dashboard receive the same provider/alias catalog as the CLI, that active aliases expose 1M metadata, and that secret values never appear in JSON.

- [ ] **Step 2: Implement typed DTOs and service methods.**

Management handlers remain thin. Validation, staging, persistence, health, and runtime reload stay in the provider service.

- [ ] **Step 3: Replace static OpenCode-only dashboard catalog.**

Render the active provider catalog and synthetic aliases. Show `1M`, free/paid status, privacy notice, health, and credential status.

- [ ] **Step 4: Extend history capture.**

Record:

```text
requested_client_model
route_alias
effective_provider
effective_model
context_window
candidate_index
credential_id/fingerprint
```

Never store raw credentials.

- [ ] **Step 5: Run HTTP, dashboard asset, and history tests.**

Run: `cargo test --test provider_management_http`

Run: `cargo test --lib management`

Run: `cargo test --lib dashboard`

Run: `cargo test --lib history`

Expected: PASS with consistent CLI/API/dashboard state.

- [ ] **Step 6: Commit management surfaces.**

```bash
git add src/management src/rest_api.rs src/dashboard src/history src/webui/app.js tests/provider_management_http.rs
git commit -m "feat: expose provider management and route history"
```

## Task 11: Complete migration, documentation, and verification

**Files:**

- Modify: `docs/cli.md`
- Modify: `docs/health-status.md`
- Modify: `README.md`
- Create: `docs/provider-management.md`
- Modify: `verification/FEATURE_MATRIX.md`
- Modify: `scripts/manual_verify_cli_redesign.py`
- Modify: `scripts/check_docs.py`
- Test: all existing suites

- [ ] **Step 1: Document the operator workflow.**

Document:

```text
provider add bai
provider credential set bai
provider models bai --probe
provider alias set free-1m
provider activate free-1m
provider health --live
```

Include the exact storage paths, credential reference rules, `[1m]` client alias behavior, strict fallback rules, and privacy warnings for free providers.

- [ ] **Step 2: Add documentation behavior tests.**

The docs check must assert that every canonical provider command, `--config` lifecycle command, alias command, and management endpoint is documented. Structural file-existence checks are insufficient.

- [ ] **Step 3: Run formatting and linting.**

Run: `cargo fmt --check`

Run: `cargo clippy --locked --all-targets --all-features -- -D warnings`

Expected: PASS.

- [ ] **Step 4: Run the complete Rust and CLI verification.**

Run: `cargo test --locked`

Run: `bash tests/cli_e2e.sh debug`

Run: `bash tests/cli_provider_management.sh`

Run: `python3 tests/claude_code_e2e.py --provider-alias free-1m`

Expected: all required tests PASS; tests requiring external WARP or unavailable provider credentials remain explicitly skipped with reasons.

- [ ] **Step 5: Verify the real long-context contract.**

Use a fixture provider that accepts a request above 200K tokens and advertises 1M. Start Claude Code through the launcher, run `/context`, send the large conversation, force the first provider to fail before content, and assert the second 1M candidate succeeds.

- [ ] **Step 6: Remove legacy behavior only after compatibility evidence.**

Remove singleton provider fields, duplicate persistence code, static OpenCode-only dashboard model selection, and legacy commands only after the compatibility suite passes for two release cycles.

- [ ] **Step 7: Commit the final migration documentation.**

```bash
git add docs README.md verification scripts
git commit -m "docs: document multi-provider 1M alias management"
```

## Rollout order

1. Tasks 1-4 establish the contract, domain, secrets, and config without changing live request routing.
2. Tasks 5-7 add catalogs, adapters, and real provider fallback behind tests.
3. Task 8 prevents stale runtime configuration and fixes Claude Code environment identity.
4. Tasks 9-10 move CLI, HTTP, dashboard, and history to the same registry.
5. Task 11 closes documentation and removes duplicate legacy paths only after verification.

## Required final demonstration

```text
$ opencode2api provider status
active alias: free-1m
client model: claude-sonnet-5[1m]
context: 1,000,000
runtime: active

$ opencode2api
Claude Code starts with ANTHROPIC_MODEL=claude-sonnet-5[1m]

Claude Code> /context
... 1m tokens ...

large request
  -> bai/deepseek-1m
  -> failure before content
  -> bai/glm-5.3-flash-1m
  -> success
```
