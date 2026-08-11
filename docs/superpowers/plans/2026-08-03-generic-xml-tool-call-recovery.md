# Generic XML Tool-Call Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore exact-once parsing and execution for generic XML tool-call forms such as `<tool_calls><invoke ...><parameter ...>` and fail closed without leakage for malformed or incomplete variants, including Agent calls.

**Architecture:** Extend the existing context-aware compatibility parser rather than adding a second ad-hoc parser. Treat `tvToolcalls/tvInvoke/tvParameter`, `tool_calls/invoke/parameter`, and `tool_call/invoke/parameter` as aliases of one strict XML grammar. Keep Markdown code-fence/inline/quote safety, atomic batch validation, bounded buffering, no-replay semantics, and sync/stream parity.

**Tech Stack:** Rust 2021, Axum, Tokio SSE, serde_json, cargo test, real Claude Code CLI PTY verification.

## Global Constraints

- Work in `/home/light/GitHub/opencode2claude` without reset, clean, checkout, or discarding unrelated dirty-tree changes.
- Do not stop unrelated Claude Code, bridge, relay, or test processes.
- No production deployment until automated tests and real CLI PTY verification pass.
- Valid XML tool calls emit exactly one `tool_use` and one matching `tool_result`; malformed or ambiguous XML emits zero tools, leaks no raw protocol markup, and requests bounded retry.
- Markers inside fenced code, inline code, quotes, or escaped text remain inert.
- Do not create a commit unless explicitly requested by the user.

---

### Task 1: Reproduce the reported XML regressions

**Files:**
- Modify: `src/opencode/forward/stream/tests.rs`
- Modify: `tests/protocol_conformance.rs`

**Interfaces:**
- Consumes: `extract_compat_tool_requests_detailed`, `process_openai_sse_line`, `StreamContext`, `MessagesRequest`.
- Produces: regression tests for valid Bash XML, valid Agent XML, malformed singular-wrapper XML, and incomplete XML across stream boundaries.

- [ ] **Step 1: Add a valid Bash XML parser test** using the exact `<tool_calls><invoke name="Bash"><parameter name="command">...</parameter>...</invoke></tool_calls>` shape and assert one call with exact command and description.
- [ ] **Step 2: Run the focused test and verify RED** because the current parser recognizes only `tvToolcalls`.
- [ ] **Step 3: Add a valid Agent XML stream test** and assert one `tool_use`, `stop_reason=tool_use`, exact arguments, and zero raw XML leakage.
- [ ] **Step 4: Run the focused test and verify RED** for the same grammar gap.
- [ ] **Step 5: Add malformed/incomplete XML tests** for `<tool_call>...Command:... </parameter>` and truncated `<tool_calls><invoke name="Bash">`; assert zero tool calls, `compat_retry_requested=true`, and no raw marker in emitted SSE.
- [ ] **Step 6: Run the focused tests and verify RED** because the current prefix detector/sanitizer does not classify generic wrappers atomically.

### Task 2: Generalize the strict XML compatibility parser

**Files:**
- Modify: `src/opencode/forward/common.rs`
- Test: `src/opencode/forward/stream/tests.rs`

**Interfaces:**
- Produces: tag-family helpers that recognize wrapper aliases `tvToolcalls`, `tool_calls`, and `tool_call`; invoke aliases `tvInvoke` and `invoke`; parameter aliases `tvParameter` and `parameter`.
- Preserves: `ParsedCompatMarker`, `CompatToolCall`, context scanner, size/call limits, entity decoding, duplicate-parameter rejection.

- [ ] **Step 1: Introduce static tag-family constants** and helpers that parse any supported alias case-insensitively while returning the matched tag for its closing-tag search.
- [ ] **Step 2: Update marker discovery and context scanning** so generic XML wrappers are recognized only in executable Markdown context.
- [ ] **Step 3: Update pending-suffix buffering** for partial prefixes `<tool_calls`, `<tool_call`, and `<tvToolcalls` without buffering ordinary `<tool...` prose broadly.
- [ ] **Step 4: Generalize wrapper/invoke/parameter parsing** while retaining strict attributes, atomic batch behavior, duplicate field rejection, limits, and XML entity decoding.
- [ ] **Step 5: Run the Task 1 focused tests and verify GREEN.**
- [ ] **Step 6: Run all compatibility/parser unit tests** and fix only regressions caused by the generalized grammar.

### Task 3: Fail-closed sanitization and sync/stream parity

**Files:**
- Modify: `src/opencode/sanitize.rs`
- Modify if required by failing tests: `src/opencode/forward/sync.rs`
- Modify: `tests/protocol_conformance.rs`

**Interfaces:**
- Consumes: `strip_system_tags_with_context`, compatibility extraction in sync and streaming paths.
- Produces: no raw generic XML tags on malformed/error paths, while code-fenced and inline examples remain unchanged.

- [ ] **Step 1: Add sanitizer tests** for generic opening tags with attributes and generic closing tags outside code; verify RED.
- [ ] **Step 2: Extend prefix-aware tag stripping** to generic `tool_calls`, `tool_call`, `invoke`, and `parameter` tags only in executable context.
- [ ] **Step 3: Add inert-context tests** proving fenced and inline XML examples remain visible and never execute.
- [ ] **Step 4: Add black-box protocol-conformance cases** for sync and stream valid/malformed generic XML.
- [ ] **Step 5: Run focused sanitizer and protocol tests and verify GREEN.**

### Task 4: Audit recent code and run automated quality gates

**Files:**
- Review only: commits `f984d9c`, `27895f3`, `52cdf0b`, `bc60b40` and all current uncommitted parser/protocol/retry/mapper changes.
- Append after verification: `REPO_WORKLOG.md`.

**Interfaces:**
- Produces: evidence-backed list of retained fixes, regressions, and residual risks.

- [ ] **Step 1: Review recent commit diffs** for parser, stream lifecycle, retry, mapper, sanitizer, and SSE changes.
- [ ] **Step 2: Run `git diff --check` and `cargo fmt --all -- --check`.**
- [ ] **Step 3: Run focused parser suites:** library parser/stream tests, `protocol_conformance`, `stream_retry_gates`, and `retry_compat_livelock`.
- [ ] **Step 4: Run `cargo clippy --locked --all-targets -- -D warnings`.**
- [ ] **Step 5: Run `cargo test --locked` and record exact pass/fail counts.**
- [ ] **Step 6: Build release binaries without deploying.**

### Task 5: Real Claude Code CLI manual verification

**Files:**
- Create: `artifacts/generic-xml-tool-call-recovery-20260803/` evidence files.
- Reuse: `artifacts/claude-upstream-reverse/tests/stub_openai.py`, PTY drivers, relay scripts where safe.

**Interfaces:**
- Produces: raw PTY transcript, wire log, parsed summary, terminal render screenshots/text, exact tool-use/result counts.

- [ ] **Step 1: Start a non-production bridge on an unused port** against a deterministic stub upstream; do not touch port 4000.
- [ ] **Step 2: Drive real Claude Code CLI through PTY** for two consecutive requests, valid Bash XML, valid Agent XML, fragmented streaming tool call, malformed XML retry, and an upstream terminal error.
- [ ] **Step 3: Verify exact-once invariants:** one semantic invocation, one tool ID, one start, one stop, one result, no raw XML, no duplicate side effect.
- [ ] **Step 4: Exercise `!` shell, Ctrl+C mid-stream, and at least ten consecutive clean turns.**
- [ ] **Step 5: Render terminal captures and assert no raw markers, duplicated lines, ANSI debris, or leftover spinner.**
- [ ] **Step 6: Only if every gate passes, atomically install/restart production using the repository restart rule, then repeat a minimal live Claude Code smoke.**
- [ ] **Step 7: Append exact evidence, binary hashes, PID, and residual risks to `REPO_WORKLOG.md`.**
