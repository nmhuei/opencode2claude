# Super-Router Multi-Account and Combo Fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port 9Router's core provider management capabilities (Multi-Account rotation, per-model cooldown locks, combo fallback chains) natively into `opencode2claude` (`opencode2api`) in Rust.

**Architecture:** A native `src/router/` subsystem in Rust containing Account Pool & Per-Model Lock tracking, Model Combos, and Provider Registry, cleanly integrated into the existing request pipeline, proxy pool, and Claude Code protocol adapter.

**Tech Stack:** Rust (Edition 2021), Tokio, Axum, Reqwest, Serde, Toml.

**Spec:** `docs/superpowers/specs/2026-09-08-super-router-design.md`

## Global Constraints
- All 156 existing unit and integration tests must continue to pass without regressions.
- Preserve backward compatibility for existing CLI commands: `provider opencode`, `provider cline`, `provider api`.
- Clean error handling with no unwrap/panic on untrusted upstream responses.

---

### Task 1: Account Pool & Per-Model Lock Tracker

**Files:**
- Create: `src/router/mod.rs`
- Create: `src/router/accounts.rs`
- Test: `tests/router_accounts.rs`

**Interfaces:**
- Consumes: Standard Rust collections, `std::time::Instant`, `std::time::Duration`.
- Produces: `Account`, `AccountPool`, `ModelLockTracker`, `AccountSelectionStrategy`.

- [ ] **Step 1: Write failing unit tests for `ModelLockTracker` and `AccountPool`**
  Write tests in `tests/router_accounts.rs` verifying:
  - Adding multiple accounts with priorities.
  - Locking a specific model on an account (`lock_model(account_id, model, duration)`).
  - Verifying that locking model A on account 1 does NOT lock model B on account 1.
  - Selecting next available account when the primary account is locked.

- [ ] **Step 2: Run test to verify failure**
  Run: `cargo test --test router_accounts`
  Expected: Compilation error (module does not exist yet).

- [ ] **Step 3: Implement `AccountPool` and `ModelLockTracker`**
  In `src/router/accounts.rs`:
  - Struct `Account`: `id`, `name`, `provider`, `api_key`, `priority`, `is_active`.
  - Struct `ModelLockTracker`: in-memory lock map with `(account_id, model_id) -> Instant`.
  - Methods: `lock_model`, `is_locked`, `cleanup_expired`.
  - Struct `AccountPool`: holds accounts, selects best available account based on strategy (`FillFirst`, `StickyRoundRobin`).
  Register `pub mod router;` in `src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**
  Run: `cargo test --test router_accounts`
  Expected: PASS.

- [ ] **Step 5: Commit**
  Run: `git add src/router tests/router_accounts.rs src/lib.rs && git commit -m "feat(router): add AccountPool and per-model ModelLockTracker"`

---

### Task 2: Model Combos and Fallback Chain

**Files:**
- Create: `src/router/combos.rs`
- Modify: `src/router/mod.rs`
- Test: `tests/router_combos.rs`

**Interfaces:**
- Consumes: `src/router/accounts.rs`.
- Produces: `ModelCombo`, `ComboStrategy`, `ComboResolver`.

- [ ] **Step 1: Write failing unit tests for `ComboResolver`**
  Write tests in `tests/router_combos.rs` testing:
  - Registering a combo `coder = ["cline/z-ai/glm-5.3-flash", "deepseek/deepseek-chat", "opencode/free"]`.
  - Resolving a plain model vs resolving a combo name.
  - Iterating candidates in fallback order.
  - Round-robin rotation of combo models.

- [ ] **Step 2: Run test to verify failure**
  Run: `cargo test --test router_combos`
  Expected: FAIL (types not found).

- [ ] **Step 3: Implement `ModelCombo` and `ComboResolver`**
  In `src/router/combos.rs`:
  - `ModelCombo`: `name`, `models: Vec<String>`, `strategy: ComboStrategy`.
  - `ComboResolver`: store of registered combos and resolution methods `resolve(&self, model: &str) -> Vec<TargetModel>`.

- [ ] **Step 4: Run test to verify it passes**
  Run: `cargo test --test router_combos`
  Expected: PASS.

- [ ] **Step 5: Commit**
  Run: `git add src/router/combos.rs src/router/mod.rs tests/router_combos.rs && git commit -m "feat(router): implement ModelCombo and ComboResolver"`

---

### Task 3: Provider Registry with OpenCode Session & Cline Connectors

**Files:**
- Create: `src/router/registry.rs`
- Modify: `src/router/mod.rs`
- Test: `tests/router_registry.rs`

**Interfaces:**
- Consumes: `src/router/accounts.rs`, `src/router/combos.rs`.
- Produces: `ProviderRegistry`, `ProviderConnector`, `PreparedRequest`.

- [ ] **Step 1: Write failing tests for Provider Registry**
  Verify:
  - OpenCode provider resolves to `http://127.0.0.1:4096` with valid session handling.
  - Cline provider resolves to `https://api.cline.bot/api/v1` with Cline token headers.
  - Generic OpenAI-compatible provider resolves with bearer token.

- [ ] **Step 2: Run test to verify failure**
  Run: `cargo test --test router_registry`
  Expected: FAIL.

- [ ] **Step 3: Implement `ProviderRegistry`**
  Implement the connector definitions and endpoint/header builders in `src/router/registry.rs`.

- [ ] **Step 4: Run test to verify it passes**
  Run: `cargo test --test router_registry`
  Expected: PASS.

- [ ] **Step 5: Commit**
  Run: `git add src/router/registry.rs src/router/mod.rs tests/router_registry.rs && git commit -m "feat(router): implement ProviderRegistry with OpenCode and Cline support"`

---

### Task 4: Integration into Request Pipeline & Full Test Suite

**Files:**
- Modify: `src/state.rs`
- Modify: `src/handlers/messages.rs`
- Modify: `src/app/models.rs`
- Test: Full cargo test suite

- [ ] **Step 1: Wire `RouterState` into `AppState`**
  Mount `AccountPool`, `ComboResolver`, and `ModelLockTracker` into `AppState`.

- [ ] **Step 2: Connect fallback loop in `handlers/messages.rs`**
  When receiving request with combo or rate-limited account, automatically execute next candidate from combo/account pool.

- [ ] **Step 3: Run full regression tests**
  Run: `cargo test`
  Expected: All 156+ unit and integration tests pass.

- [ ] **Step 4: Commit**
  Run: `git add src/state.rs src/handlers/messages.rs src/app/models.rs && git commit -m "feat(router): integrate super-router into server request pipeline"`
