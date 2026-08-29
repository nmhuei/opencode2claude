# OpenCode Free Model Auto-Detection & Context Tuning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement automatic detection and live probing of OpenCode free models, dynamic 80% autocompact context window tuning, and CLI management commands (`opencode2api list`, `opencode2api model set <model>`).

**Architecture:** Extend model catalog with context profiles, implement an upstream prober to detect responsive free models, calculate dynamic Claude Code environment variables (80% autocompact window), and expose CLI commands for model inspection and selection.

**Tech Stack:** Rust, Tokio, Reqwest, Axum, Clap, Comfy-Table, Yansi, Serde JSON.

**Spec:** `docs/superpowers/specs/2026-08-29-model-detection-and-autoconfig-design.md`

## Global Constraints
- Target only free models on OpenCode Zen (`*-free`, `big-pickle`, etc.).
- Auto-compact window is strictly calculated as `context_window * 80 / 100`.
- All tests must compile and pass without warnings (`cargo clippy -- -D warnings`).
- Maintain backward compatibility with existing CLI arguments and dashboard APIs.

---

### Task 1: Model Context Profiles & Dynamic Claude Code Tuning (80% Auto-Compact)

**Files:**
- Modify: `src/application/models.rs`
- Modify: `src/application/integration.rs`
- Test: Unit tests in `src/application/models.rs` and `src/application/integration.rs`

**Interfaces:**
- Produces: `ModelProfile` struct with fields `id`, `context_window`, `max_output_tokens`, `supports_thinking`.
- Produces: `ModelProfile::auto_compact_window(&self) -> usize` returning `context_window * 80 / 100`.
- Produces: `model_claude_code_vars(profile: &ModelProfile) -> Vec<(String, String)>`.

- [ ] **Step 1: Write failing tests in `src/application/models.rs` and `src/application/integration.rs`**
- [ ] **Step 2: Run `cargo test --lib application` and verify tests fail**
- [ ] **Step 3: Implement `ModelProfile`, `auto_compact_window()`, and dynamic `model_claude_code_vars`**
- [ ] **Step 4: Run `cargo test --lib application` and verify tests pass**

---

### Task 2: Upstream Free Model Probing & Auto-Detection

**Files:**
- Create: `src/application/prober.rs`
- Modify: `src/application/mod.rs`
- Test: Unit tests in `src/application/prober.rs`

**Interfaces:**
- Produces: `enum ModelStatus { Online, RateLimited, Unavailable, Unknown }`
- Produces: `struct ProbedModel { profile: ModelProfile, status: ModelStatus, latency_ms: Option<u64> }`
- Produces: `pub async fn fetch_and_probe_free_models(client: &reqwest::Client, base_url: &str) -> Vec<ProbedModel>`
- Produces: `pub async fn detect_best_free_model(client: &reqwest::Client, base_url: &str) -> Option<ModelProfile>`

- [ ] **Step 1: Write failing tests for model status classification and parsing**
- [ ] **Step 2: Run `cargo test --lib application::prober` and verify failure**
- [ ] **Step 3: Implement `prober.rs` with upstream probing and candidate selection**
- [ ] **Step 4: Run `cargo test --lib application::prober` and verify tests pass**

---

### Task 3: CLI Subcommands (`opencode2api list` and `opencode2api model`)

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/app/utility.rs` (or create `src/app/models.rs`)
- Test: CLI parsing tests in `src/cli.rs` and command execution tests

**Interfaces:**
- Adds CLI subcommands:
  - `opencode2api list [--probe] [--json]` (or alias `opencode2api models`)
  - `opencode2api model set <model>`
  - `opencode2api model get` / `opencode2api model status`
- Produces formatted table output with comfy-table / yansi.

- [ ] **Step 1: Write CLI parser tests for `list` and `model` subcommands**
- [ ] **Step 2: Run `cargo test --lib cli` and verify failure**
- [ ] **Step 3: Implement CLI commands in `src/cli.rs`, `src/app/models.rs`, and wire into `src/app/mod.rs`**
- [ ] **Step 4: Run `cargo test --lib` and verify tests pass**

---

### Task 4: Dynamic Auto-Config Injection in Bare `opencode2api` Launcher

**Files:**
- Modify: `src/app/mod.rs` (in `launch_claude_code`)
- Test: Launcher environment tests in `src/app/mod.rs`

**Interfaces:**
- Resolves effective model: if configured model is invalid or unset, uses detected working free model (e.g. `mimo-v2.5-free`).
- Injects dynamic environment variables: `ANTHROPIC_MODEL`, `CLAUDE_CODE_AUTO_COMPACT_WINDOW` (80%), `CLAUDE_CODE_MAX_OUTPUT_TOKENS`, `CLAUDE_CODE_DISABLE_1M_CONTEXT`, `CLAUDE_CODE_DISABLE_THINKING`.

- [ ] **Step 1: Write test for dynamic launcher environment generation**
- [ ] **Step 2: Update `launch_claude_code` in `src/app/mod.rs`**
- [ ] **Step 3: Run `cargo test --lib app` and verify pass**

---

### Task 5: End-to-End Verification & Documentation

**Files:**
- Modify: `REPO_WORKLOG.md`
- Run: Full test suite (`cargo test`, `cargo clippy`, `cargo fmt`)
- Test: Real CLI commands `opencode2api list`, `opencode2api model status`

- [ ] **Step 1: Run `cargo fmt --check` and `cargo clippy -- -D warnings`**
- [ ] **Step 2: Run all unit and fast integration tests (`cargo test`)**
- [ ] **Step 3: Test CLI commands locally**
- [ ] **Step 4: Update `REPO_WORKLOG.md`**
