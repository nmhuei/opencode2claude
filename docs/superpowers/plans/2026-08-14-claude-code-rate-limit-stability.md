# Claude Code Rate-Limit Stability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent opencode2claude from amplifying upstream 429s and remove Claude Code's unnecessary upstream title-generation call.

**Architecture:** Add a narrow local title fast-path in the Anthropic Messages handler and distinguish in-flight retry routing from fresh-request routing. Fresh requests stay behind the recovery gate; retries never switch egress identities to evade quota.

**Tech Stack:** Rust, Axum, Tokio, reqwest, SQLite request history, Claude Code CLI 2.1.229.

## Global Constraints
- Do not bypass provider quota by rotating or switching IPs.
- Do not silently change the main task model.
- Preserve existing dirty working-tree changes.

---

### Task 1: Claude Code local title fast-path
**Files:** Modify `src/handlers/messages.rs`; create `src/handlers/title.rs`; modify `src/handlers/mod.rs`.
- [ ] Write tests that recognize the exact Claude Code title request and reject near-miss structured-output requests.
- [ ] Run focused tests and observe failure before implementation.
- [ ] Implement narrow detection, local title synthesis, and streaming/non-streaming Anthropic responses.
- [ ] Run focused tests until green.

### Task 2: Retry/recovery state machine
**Files:** Modify `src/opencode/retry/execute.rs`; tests in the same module.
- [ ] Write a regression test proving an in-flight retry is not treated as a fresh request and cannot select a different proxy.
- [ ] Run focused test and observe failure before implementation.
- [ ] Add explicit retry-route semantics with bounded waiting and no cross-proxy failover on rate limit.
- [ ] Run focused tests until green.

### Task 3: Verification and production rollout
**Files:** No new source files beyond Tasks 1-2.
- [ ] Run `cargo fmt --check`, focused tests, full tests, `git diff --check`, and release build.
- [ ] Restart only the existing production service using its current service unit/command.
- [ ] Verify `/health/ready`.
- [ ] Run one simple Claude Code CLI smoke request and inspect history: title request must be local/no upstream attempt.
- [ ] Run one Bash tool-call flow and inspect history/logs for 429/retry storm behavior.
