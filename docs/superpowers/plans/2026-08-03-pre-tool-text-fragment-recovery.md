# Pre-tool Text Fragment Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent Claude Code from displaying an unfinished word or sentence when an upstream streaming response switches from visible text to a native or compatibility tool call.

**Architecture:** Keep the current visible sentence tail buffered while tools are available. If the turn ends normally, flush the whole tail. If a tool call begins, emit only text through the last trustworthy sentence boundary and discard the unfinished tail; never synthesize missing words from reasoning.

**Tech Stack:** Rust, Tokio, Anthropic SSE, OpenAI-compatible streaming, Cargo tests.

## Global Constraints

- Work only in `/home/light/GitHub/opencode2claude`.
- Preserve the existing dirty tree and concurrent-session changes; no reset, clean, checkout, or commit.
- Write a failing regression test before production code.
- Do not expose reasoning as visible text or invent missing words.
- Run the mandatory real Claude Code CLI verification before production deployment.

---

### Task 1: Reproduce the clipped pre-tool text

**Files:**
- Modify: `src/opencode/forward/stream/tests.rs`

**Interfaces:**
- Consumes: `process_openai_sse_line`, `StreamContext`, `SseBlockTracker`, `MessagesRequest`.
- Produces: regression test proving incomplete visible text is not allowed immediately before a native Bash `tool_use`.

- [ ] **Step 1: Write the failing test**

Add an async test that streams `Proxy up (200), env đủ. Copy tinyctfer sang tools/ và đọc code container conf`, then a native Bash call with `finish_reason=tool_calls`. Assert that the complete first sentence and Bash tool block are emitted, while the unfinished second sentence is absent.

- [ ] **Step 2: Run the narrow test and verify RED**

Run: `cargo test native_tool_call_discards_unfinished_visible_sentence_tail -- --nocapture`

Expected: FAIL because current code emits the unfinished second sentence.

### Task 2: Buffer and resolve the current sentence tail

**Files:**
- Modify: `src/opencode/forward/stream/context.rs`
- Modify: `src/opencode/forward/stream/tests.rs`

**Interfaces:**
- Produces: a Unicode-safe helper that splits text at the last completed sentence boundary and integrates with streaming/native/compat tool transitions.

- [ ] **Step 1: Implement the minimal helper**

Treat `.`, `!`, `?`, and newline as trustworthy boundaries, including trailing whitespace/closing punctuation. Preserve UTF-8 boundaries and cap retained tail size without cutting a code point.

- [ ] **Step 2: Hold the active sentence while tools are available**

Extend the existing pending-text splitter so complete sentences stream immediately while the active unfinished sentence remains in `text_stream_buffer`.

- [ ] **Step 3: Resolve the buffer before tool emission**

Before native or compatibility tool blocks are emitted, flush only the completed prefix and discard the unfinished tail with a diagnostic warning. Normal `end_turn` continues to flush all retained text.

- [ ] **Step 4: Run the narrow test and verify GREEN**

Run: `cargo test native_tool_call_discards_unfinished_visible_sentence_tail -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Run focused parser and protocol regressions**

Run the stream unit tests, protocol conformance, retry compatibility, and stream retry gates.

### Task 3: Verify with real Claude Code and deploy atomically

**Files:**
- Create evidence under: `artifacts/pre-tool-text-fragment-recovery-20260803/`
- Append: `REPO_WORKLOG.md`

**Interfaces:**
- Consumes: release binaries and existing PTY/stub harnesses.
- Produces: real CLI evidence and a local implementation workspace.

- [ ] **Step 1: Run fmt, clippy, focused/full tests, and release build**

Use `/home/light/rust-target` as the release target and verify binary hashes.

- [ ] **Step 2: Run mandatory real-CLI side-bridge matrix**

Exercise normal text, Bash, Agent, error recovery, Ctrl+C, ten turns, and the clipped pre-tool fixture. Verify no raw markers, duplicate side effects, or unfinished visible text.

- [ ] **Step 3: Atomically deploy production**

Use the reviewed restart script only after the side matrix passes; verify listener ownership, hashes, health, and rollback protection.

- [ ] **Step 4: Implement the supplied CTF loop with real Claude Code**

Copy `claude-loop-autonomous-ctf-pentest.md` into `/home/light/Workspace/CTF/Test_claude_code`. Run Claude Code there with the file as requirements, substituting the current directory for the hard-coded `/home/light/GitHub/CTF`, and verify created state/research artifacts without modifying the original CTF repo.

- [ ] **Step 5: Record evidence**

Append root cause, code changes, commands, CLI artifacts, production PID/hash, and remaining risks to `REPO_WORKLOG.md`.
