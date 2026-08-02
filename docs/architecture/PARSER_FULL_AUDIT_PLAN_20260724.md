# Full Parser Audit Plan — OpenCode2Claude

Date: 2026-07-24

Repository: `/home/light/GitHub/opencode2claude`

## 1. Objective

Audit and harden the complete parsing and protocol-conversion pipeline, not only the two recently fixed shorthand markers.

The audit must prove these properties end to end:

1. Valid input is preserved semantically.
2. Invalid or ambiguous input is rejected rather than guessed.
3. No raw tool marker leaks after being classified as protocol intent.
4. No tool invocation is silently dropped.
5. No tool can execute twice because an upstream retry replayed an already-emitted call.
6. Streaming, synchronous, and EOF-recovery paths produce equivalent results for equivalent input.
7. Parser state remains safe across arbitrary SSE chunk boundaries and UTF-8 boundaries.
8. Code examples and quoted marker text cannot become executable tool calls.
9. Unknown or unavailable tools produce an explicit recoverable protocol outcome, not an apparently successful final answer.
10. Search interception never loses batched calls or changes ordering without an explicit policy.
11. Parser memory, nesting, marker count, and repair work are bounded.
12. Claude Code CLI completes the real tool-use loop without leaked markers, malformed tool blocks, duplicate side effects, or session interruption.

## 2. Scope

The audit covers the full request/response path:

```text
Claude Code Anthropic request
  -> inbound JSON/content parsing
  -> request mapping and tool schema mapping
  -> upstream OpenAI-compatible request
  -> sync JSON or streaming SSE parsing
  -> reasoning/text/native tool-call assembly
  -> DSML parser
  -> compatibility-marker parser
  -> JSON normalization/repair
  -> search interception and loop injection
  -> retry/recovery logic
  -> Anthropic content blocks/SSE output
  -> Claude Code tool_result continuation
```

Primary code areas:

```text
src/handlers/messages.rs
src/handlers/types.rs
src/opencode/mapper/request.rs
src/opencode/mapper/helpers.rs
src/opencode/mapper/policy.rs
src/opencode/sanitize.rs
src/opencode/types.rs
src/opencode/forward/common.rs
src/opencode/forward/sync.rs
src/opencode/forward/stream/context.rs
src/opencode/forward/stream/execute.rs
src/opencode/forward/stream/tests.rs
src/opencode/retry/*
src/opencode/search/*
src/sse.rs
```

Test and E2E areas:

```text
tests/parser_fuzz_smoke.rs
tests/protocol_conformance.rs
tests/fast.rs
tests/integration.rs
artifacts/parser-soc-audit/
```

## 3. Confirmed architectural risks in the current code

These are concrete audit starting points, not hypothetical backlog items.

### P0 — Batch encoded through a JSON-array sentinel

`parse_compat_argument_sequence()` serializes multiple marker arguments as one JSON array. `expand_compat_tool_arguments()` then treats an array of two or more objects as a batch.

A legitimate tool whose actual input is an array of objects can therefore be split into multiple invocations.

### P0 — Retry can replay an already-emitted side effect

The stream execution loop retries whenever `compat_retry_requested` is set. It does not currently gate the retry on `has_emitted_tool_use`.

A response shaped as `valid tool -> malformed marker` can emit the first tool, retry the entire upstream turn, and replay that tool.

### P0 — Marker execution is not Markdown-context aware

Compatibility detection scans for `[` and a `Requesting` grammar. It does not maintain fenced-code, inline-code, quote, JSON-string, or escaped-marker context.

A tool marker shown as an example can be executed.

### P1 — Stream/sync/EOF behavior is not one parser contract

Streaming uses rolling buffers, intent detection, EOF recovery, and compatibility retry. Sync uses `extract_compat_tool_requests()` over complete message text and has no equivalent EOF/retry state.

Equivalent upstream responses can therefore produce different outcomes.

### P1 — Search batches can lose calls

Streaming stops expanding compatibility calls as soon as the first search call sets `intercepting_search`. Sync selects the first matching search call and ignores later calls in the same response.

This is silent dropping unless a deliberate single-search policy is made explicit.

### P1 — Unknown tool handling diverges

Streaming converts an unavailable compatibility tool into visible text. Sync and native-tool paths mostly skip unavailable calls. This can produce `end_turn` and make Claude Code believe the assistant completed normally.

### P1 — Streaming malformed-marker resynchronization is incomplete

Sync advances after a malformed marker and can recover a later valid marker. Streaming retains the first intent-shaped marker until EOF or buffer overflow, so a later valid marker may be blocked.

### P1 — Prefix buffering is overly broad

Any short suffix beginning with normalized `[requesting` is retained. Ordinary prose can be delayed and eventually classified as malformed protocol intent.

### P1 — JSON repair can change semantics

Quote repair, invalid-escape repair, control-character escaping, and detached-field merging are permissive. Bash commands, regexes, Windows paths, and embedded JSON need semantic-preservation tests. Ambiguous repairs must fail closed.

## 4. Audit strategy

The implementation order is deliberately test-first and risk-first.

No parser refactor begins until the current behavior is captured and the P0 failure tests reproduce the predicted defects.

## 5. Phase 0 — Safety snapshot and reproducible baseline

### Actions

1. Read `CLAUDE.md` and `REPO_WORKLOG.md`.
2. Record `git status --short` without resetting or discarding anything.
3. Save diffs for parser-related files only.
4. Record current service PID, executable, command line, model, and health.
5. Run the existing focused and global gates.
6. Archive current Claude Code CLI smoke output.

### Commands

```bash
cd /home/light/GitHub/opencode2claude

git status --short
git diff -- src/opencode/forward/common.rs \
  src/opencode/forward/sync.rs \
  src/opencode/forward/stream/context.rs \
  src/opencode/forward/stream/execute.rs \
  src/opencode/forward/stream/tests.rs \
  > artifacts/parser-full-audit/baseline-parser.diff

cargo test opencode::forward::stream::tests
cargo test --test parser_fuzz_smoke
cargo test --test protocol_conformance
cargo test --locked
cargo build --release --locked --bins
```

### Exit gate

- Baseline results and failures are recorded.
- No existing user changes are reset.
- Port 4000 remains healthy.

## 6. Phase 1 — Build a parser inventory and data-flow map

### Deliverable

Create:

```text
docs/architecture/PARSER_PIPELINE_MAP_20260724.md
```

For every parser or normalizer, document:

- Input type and trust boundary.
- Caller and downstream consumer.
- Streaming state, if any.
- Strict grammar versus recovery grammar.
- Size/nesting/count bounds.
- Error outcome.
- Whether any output has already been emitted when failure is detected.
- Whether retry is possible.
- Whether the operation can cause side effects.

### Required inventory

1. Anthropic request JSON and content union parsing.
2. Tool schema and tool-result history mapping.
3. Upstream sync JSON parsing.
4. SSE framing and chunk parsing.
5. Native OpenAI `tool_calls` assembly.
6. DSML parsing and normalization.
7. Compatibility marker intent/header/argument parsing.
8. JSON repair and detached-field merging.
9. Search tool detection and result injection.
10. Anthropic SSE/content-block emission.
11. Retry state and attempt replay.

### Exit gate

Every parser entry point has one documented owner and one explicit error policy.

## 7. Phase 2 — Define invariants and a unified result model

Before changing code, encode the target behavior as types and test assertions.

### Proposed compatibility parser model

```rust
#[derive(Debug, Clone, PartialEq)]
struct CompatToolCall {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
struct ParsedCompatMarker {
    prefix: String,
    calls: Vec<CompatToolCall>,
    consumed: usize,
    syntax: CompatMarkerSyntax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatMarkerSyntax {
    Canonical,
    Shorthand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseMode {
    Streaming,
    CompleteResponse,
    EofRecovery,
}

enum CompatParseResult {
    NoMarker,
    Pending { start: usize },
    Complete(ParsedCompatMarker),
    Malformed {
        start: usize,
        reason: CompatParseError,
        recoverable_at: Option<usize>,
    },
}
```

### Required invariants

- `consumed <= input.len()`.
- `consumed` is always a UTF-8 boundary.
- A complete marker returns calls directly; batch is never encoded as an argument array sentinel.
- Parsing has no side effects and emits no SSE.
- Parsed JSON values remain typed until final Anthropic serialization.
- Retry is forbidden after any externally visible `tool_use` has been emitted.
- A malformed batch emits zero calls.
- A marker in a non-executable context emits zero calls.

## 8. Phase 3 — Add failing P0/P1 regression tests

Create the tests before fixing production logic.

### P0 tests

1. `legitimate_array_input_is_one_tool_call`
2. `valid_tool_then_malformed_marker_never_retries_or_duplicates`
3. `fenced_code_marker_is_not_executed`
4. `inline_code_marker_is_not_executed`
5. `partially_malformed_batch_emits_zero_calls`
6. `retry_output_cannot_replay_prior_tool_use`

### P1 tests

7. `malformed_marker_then_valid_marker_resynchronizes`
8. `valid_marker_then_malformed_marker_has_explicit_terminal_policy`
9. `stream_sync_eof_parity_matrix`
10. `unknown_tool_has_same_outcome_in_stream_and_sync`
11. `search_batch_is_queued_or_explicitly_rejected`
12. `ordinary_requesting_prose_is_not_buffered_as_tool_intent`
13. `quoted_marker_is_not_executed`
14. `escaped_marker_is_not_executed`
15. `marker_inside_json_string_is_not_executed`
16. `marker_inside_tool_result_text_is_not_executed`
17. `reasoning_and_visible_text_follow_the_same_execution-context_policy`

### Chunk-boundary tests

For every valid marker fixture, split at:

- Every byte boundary that is also a UTF-8 boundary.
- Every two bytes.
- Every token boundary in `Requesting`, tool name, `arguments`, JSON keys/strings, commas, and closing brackets.
- Before and after multibyte Unicode content inside JSON strings.

### Exit gate

Each predicted defect is either reproduced by a failing test or documented as already safe with supporting evidence.

## 9. Phase 4 — Fixture-driven parser harness

Create:

```text
tests/fixtures/compat_markers.json
```

Each fixture should contain:

```json
{
  "name": "shorthand_taskupdate_batch",
  "input": "...",
  "available_tools": ["TaskUpdate"],
  "context": "plain_text",
  "expected_calls": [],
  "expected_cleaned_text": "",
  "expected_state": "complete|pending|malformed|none",
  "expected_retry": false,
  "expected_leak": false
}
```

### Fixture groups

- Canonical grammar.
- Shorthand grammar.
- Multiple markers.
- True batches.
- Legitimate array arguments.
- Nested object/array values.
- Malformed batches.
- Missing wrapper bracket at EOF.
- Malformed then valid.
- Valid then malformed.
- Unknown tools.
- Code/quote/string/escape contexts.
- Oversized markers.
- Unicode and control characters.
- Search calls.

The same fixture must run through:

1. Complete-response parser.
2. Streaming parser with chunk permutations.
3. EOF recovery.
4. Sync formatter.
5. Stream formatter.

## 10. Phase 5 — Refactor compatibility parsing into one core

### Actions

1. Move compatibility parsing into a dedicated module, for example:

```text
src/opencode/forward/compat_parser.rs
```

2. Replace tuple return values with structured results.
3. Delete the JSON-array batch sentinel and `expand_compat_tool_arguments()` behavior.
4. Return `Vec<CompatToolCall>` directly.
5. Add a lexical execution-context scanner for Markdown fences, inline code, escapes, and quoted/non-executable regions.
6. Make pending-prefix detection tool-aware using tools present in the payload where possible.
7. Implement bounded resynchronization after malformed markers.
8. Enforce maximum marker size, nesting depth, and batch count.

### Resynchronization rule

A later marker may be considered only when the parser can prove it is outside:

- The malformed marker's quoted JSON string.
- Nested JSON containers.
- Markdown code fences or inline code.

No blind scan through arbitrary malformed JSON strings.

### Exit gate

Stream, sync, and EOF paths call the same parser core with different `ParseMode` values only.

## 11. Phase 6 — Make emission transactional and retry-safe

### Core rule

```text
No upstream full-turn retry after any tool_use has been emitted to Claude Code.
```

### Actions

1. Parse and validate a complete compatibility batch before emitting its first call.
2. Track an explicit turn state:

```rust
struct ToolEmissionState {
    emitted_count: usize,
    emitted_fingerprints: HashSet<ToolCallFingerprint>,
    retry_allowed: bool,
}
```

3. Permit compatibility retry only when `emitted_count == 0`.
4. If malformed protocol appears after emission, terminate with an explicit protocol error policy without replaying the turn.
5. Add normalized fingerprints for defense-in-depth logging/deduplication, not as a substitute for transactional parsing.
6. Ensure native, DSML, and compatibility calls follow the same no-replay invariant.

### Side-effect E2E tools

Use a fake tool/result harness that counts actual invocations for:

- `Write`
- `Edit`
- `Bash`
- `TaskUpdate`

Each must execute exactly once across malformed remainder and retry scenarios.

## 12. Phase 7 — Unify unknown-tool and search policies

### Unknown tools

Choose one explicit policy shared by stream and sync:

- Prefer safe retry with the exact available tool list when no call has been emitted.
- Never convert a protocol error into a normal final user-visible answer.
- Never return `stop_reason=tool_use` without a valid tool block.

### Search batches

Choose and enforce one policy:

1. Queue search calls and execute them in original order, or
2. Reject multi-search compatibility batches and request canonical separate calls.

Silent dropping is forbidden.

The policy must cover native, DSML, and compatibility tool formats.

## 13. Phase 8 — Audit and restrict JSON recovery

### Required rules

1. Strict JSON always wins and must round-trip semantically.
2. A repair is accepted only when there is one unambiguous interpretation.
3. Each repair returns a typed `RepairReport` describing what changed.
4. Ambiguous quote, escape, delimiter, or detached-field repairs fail closed.
5. Recovery must not alter valid Bash commands, regexes, paths, or embedded JSON.

### Property tests

- Valid JSON input is semantic-equivalent after normalization.
- Invalid escape repair preserves decoded command bytes.
- Quote repair never changes object structure.
- Detached fields cannot overwrite an existing non-null field.
- Multiple possible repairs are rejected.
- Deep nesting and huge strings respect limits.

## 14. Phase 9 — Audit SSE and native/DSML parser parity

The compatibility parser cannot be considered safe in isolation.

### SSE checks

- CRLF and LF framing.
- Multiple `data:` lines.
- Malformed lines.
- Duplicate `[DONE]`.
- Split JSON tokens.
- Empty deltas.
- Interleaved reasoning, text, and tool-call deltas.
- Tool name arriving after argument fragments.
- Multiple native tool-call indices.
- Stream termination during open tool JSON.

### Native/DSML checks

- Unknown tools.
- Multiple calls and ordering.
- Search mixed with non-search calls.
- Malformed one among valid calls.
- No empty `{}` substitution for invalid arguments unless explicitly safe.
- Same stop reason and content-block behavior as compatibility calls.

## 15. Phase 10 — Fuzzing and bounded-resource tests

Extend `tests/parser_fuzz_smoke.rs` or add a focused parser test target.

### Properties

```text
compat_parser_never_panics
compat_parser_never_overconsumes
compat_parser_consumed_is_utf8_boundary
compat_parser_never_leaks_claimed_intent
compat_parser_never_splits_legitimate_array_input
compat_parser_malformed_batch_is_atomic
retry_never_duplicates_emitted_calls
stream_sync_eof_are_equivalent
parser_respects_size_depth_and_count_limits
```

### Mutation corpus

- Delete each character.
- Insert `]`, `}`, `"`, `\\`, comma, colon.
- Duplicate delimiters.
- Truncate at every boundary.
- Deep nesting.
- Huge strings.
- Unicode content.
- Null/control bytes at the transport boundary.
- Thousands of batch items.

## 16. Phase 11 — End-to-end fake-upstream matrix

Build deterministic fake-upstream scenarios under:

```text
artifacts/parser-full-audit/
```

Required scenarios:

1. Canonical valid call.
2. Shorthand valid call.
3. Valid true batch.
4. Legitimate array argument.
5. Malformed before valid.
6. Valid before malformed.
7. Partially malformed batch.
8. Marker in code block.
9. Unknown tool.
10. Search batch.
11. Native + DSML + compatibility mixed response.
12. Retry correction response.
13. Retry budget exhaustion.
14. Side-effect invocation counter.

For each scenario capture:

- Raw upstream SSE.
- Bridge SSE.
- Tool-use blocks.
- Tool-result blocks.
- Stop reason.
- Retry attempts.
- Invocation count.
- Raw marker leak count.
- Final Claude Code result.

## 17. Phase 12 — Real Claude Code CLI verification

Run Claude Code CLI without relying on `settings.json`.

### Minimum assertions

```text
exit code = 0 for valid scenarios
raw marker count = 0
tool_use count = expected
tool_result count = expected
no duplicate side effect
session continues after recoverable tool error
final stop_reason = end_turn or tool_use as appropriate
no malformed_tool_use
no permission denial in the controlled fixture
```

Test both:

- `--output-format json`
- `--output-format stream-json --verbose --include-partial-messages`

## 18. Phase 13 — Quality gates and deployment

Run:

```bash
git diff --check
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test opencode::forward::stream::tests
cargo test --test parser_fuzz_smoke
cargo test --test protocol_conformance
cargo test --locked
cargo build --release --locked --bins
```

Restart only the confirmed process holding port 4000. After restart verify:

```bash
opencode2api server status --json
curl -sS http://127.0.0.1:4000/health
readlink /proc/$pid/exe
ps -p "$pid" -o pid,ppid,pgid,stat,etime,cmd
```

The executable must not show `(deleted)`.

## 19. Deliverables

```text
docs/architecture/PARSER_FULL_AUDIT_PLAN_20260724.md
docs/architecture/PARSER_PIPELINE_MAP_20260724.md
tests/fixtures/compat_markers.json
focused parser unit/property tests
stream/sync parity tests
retry side-effect E2E tests
artifacts/parser-full-audit/raw-upstream/
artifacts/parser-full-audit/bridge-sse/
artifacts/parser-full-audit/claude-cli/
artifacts/parser-full-audit/REPORT.md
REPO_WORKLOG.md entry
```

## 20. Completion criteria

The audit is complete only when all conditions below are met:

1. The JSON-array batch sentinel is removed.
2. Stream, sync, and EOF use one parser core.
3. Marker context prevents code-example execution.
4. Malformed-marker resynchronization is tested and bounded.
5. Partial batches are atomic.
6. Retry cannot duplicate an emitted tool.
7. Unknown-tool behavior is explicit and identical across paths.
8. Search calls are queued or explicitly rejected; none are silently dropped.
9. JSON repair is demonstrably semantic-preserving or fails closed.
10. Fuzz/property tests pass.
11. Full Rust quality gates pass.
12. Fake-upstream E2E passes.
13. Real Claude Code CLI passes with correct counts and no raw marker leak.
14. Port 4000 runs the rebuilt release binary and remains healthy.
15. No unrelated working-tree changes are reset, discarded, or committed.

## 21. Execution checkpoints

Work should be reported at these checkpoints:

1. Baseline and parser map complete.
2. P0 tests reproduce defects.
3. Unified parser core compiles and focused tests pass.
4. Retry and side-effect invariants pass.
5. Stream/sync/EOF parity passes.
6. Fuzz and full quality gates pass.
7. Real Claude Code CLI E2E passes.
8. Final report and worklog are updated.

No commit is created unless explicitly requested by the user.
