# Code Optimization Plan — 2026-07-24

## Objective

Optimize the parser and streaming hot paths without changing protocol semantics, retry guarantees, or tool-execution safety.

## Evidence and constraints

- The current release profile already enables LTO and one codegen unit, so the next gains should come from algorithmic and allocation reductions rather than more compiler flags.
- `BytesMut::split_to` is documented as an O(1) operation and is appropriate for incremental network buffers.
- `memchr` provides SIMD-accelerated byte and substring search and is already present in the lockfile transitively.
- Async performance changes must remain observable through structured tracing and must preserve cancellation behavior.

## Baseline findings

1. **SSE framing hot path**
   - `Vec<u8>::drain(..pos + 1).collect::<Vec<u8>>()` allocates a new vector and shifts the remaining buffer for every SSE line.
   - Newline search uses scalar iterator traversal.
   - The configured line-size limit is checked against the whole accumulated chunk before complete lines are consumed, so one network chunk containing many valid small lines can be rejected incorrectly.

2. **Tool lookup hot path**
   - `matching_tool_name` lowercases the requested name and every declared tool name for each lookup.
   - `eq_ignore_ascii_case` can perform the same comparison without temporary strings.

3. **Compatibility marker scanning**
   - ASCII marker candidate scans use generic string matching for `'['`.
   - Candidate positions can be found with byte search because ASCII `[` is always a UTF-8 boundary.

4. **Sanitizer allocation/scanning**
   - `strip_system_tags_with_context` repeatedly calls `cleaned.trim().is_empty()` when removing tags.
   - `extract_attribute` allocates a `String` from an existing `&str` only to search it.

## Implementation phases

### Phase 1 — Reproducible hot-path benchmark

Add a release-mode microbenchmark example comparing:

- old `Vec::drain + collect` SSE extraction vs `BytesMut::split_to + memchr`;
- lowercase-allocation tool matching vs allocation-free case-insensitive matching;
- generic bracket scanning vs byte scanning.

The benchmark is diagnostic only; correctness remains enforced by unit, fuzz, protocol, and E2E tests.

### Phase 2 — SSE buffer optimization

- Change the incremental line buffer from `Vec<u8>` to `BytesMut`.
- Find newlines with `memchr`.
- Extract complete lines with O(1) `split_to`.
- Enforce `max_sse_line_bytes` per logical SSE line, not per received network chunk.
- Add regressions for:
  - a chunk whose aggregate size exceeds the limit while every individual line is valid;
  - one oversized logical line split across chunks;
  - preservation of the final unterminated line.

### Phase 3 — Parser allocation reductions

- Use `memchr_iter` for compatibility-marker candidate brackets.
- Replace lowercase-allocation tool matching with `eq_ignore_ascii_case`.
- Remove avoidable sanitizer allocations and repeated trim scans.

### Phase 4 — Verification

Required gates:

```text
git diff --check
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked --bins
```

Focused validation:

- parser/stream tests;
- protocol conformance;
- parser fuzz smoke;
- real service health and one Claude Code smoke request.

## Acceptance criteria

- No parser, protocol, fuzz, integration, or E2E regression.
- No new retry path after a tool has been emitted.
- No raw compatibility marker leak.
- SSE line limits operate on logical lines.
- Benchmark shows a measurable reduction in hot-path time or allocation-heavy work.
- Production service remains healthy after deployment.
