# Parser Pipeline Map — OpenCode2Claude

Date: 2026-07-24

Repository: `/home/light/GitHub/opencode2claude`

## 1. End-to-end trust boundaries

```text
Claude Code JSON
  -> Axum JSON extractor
  -> MessagesRequest semantic validation
  -> Anthropic-to-OpenAI mapper
  -> upstream HTTP response
     -> sync JSON parser
     OR
     -> bounded SSE line/chunk parser
  -> native tool-call assembler
  -> DSML parser
  -> compatibility-marker parser
  -> search interception / retry policy
  -> Anthropic JSON or SSE formatter
  -> Claude Code tool execution
  -> tool_result continuation
```

Every arrow is a trust boundary. Data is not considered executable merely because it resembles a tool call in visible text.

## 2. Inbound Anthropic request

Entry point:

```text
src/handlers/messages.rs::handle_messages
```

Wire types:

```text
src/handlers/types.rs
```

Validation occurs before history capture, routing, shell handling, mapping, or upstream traffic.

Validated invariants:

- At least one message.
- Roles are `user` or `assistant`.
- Known content blocks contain their required fields.
- `tool_use` is assistant-only and requires non-empty id/name plus object input.
- `tool_result` is user-only and requires `tool_use_id` plus content.
- Tool names are non-empty, bounded, and case-insensitively unique.
- Every tool schema is a JSON object.
- Named `tool_choice` references an available tool.
- Numeric sampling fields are within supported ranges.
- Future content block types remain accepted instead of being destructively rejected.

Failure policy: HTTP invalid-request response; no upstream request is made.

## 3. Request mapping

Entry point:

```text
src/opencode/mapper/request.rs::map_anthropic_to_openai_with_policy
```

Supporting parsers:

```text
src/opencode/mapper/helpers.rs
src/opencode/mapper/policy.rs
```

Mapping guarantees:

- Tool history is correlated by `tool_use_id`.
- Free-model compatibility history is encoded as canonical markers.
- Native-capable models receive structured OpenAI `tool_calls`.
- Tool schemas and tool choice are preserved after inbound validation.
- Tool-result content is converted deterministically.

## 4. Upstream sync response

Entry point:

```text
src/opencode/forward/sync.rs::forward_to_llm_sync
```

Response body is read with a configured byte bound before JSON deserialization.

Execution order:

```text
OpenAI response JSON
  -> native tool validation
  -> context-safe DSML extraction
  -> context-safe compatibility extraction
  -> atomic protocol validation
  -> optional search interception
  -> Anthropic response formatting
```

Sync retry is allowed only before any client-side tool execution. It is used for malformed DSML/compat markers, unknown tools, malformed native arguments, and batches that combine search with another invocation.

## 5. Upstream SSE response

Entry points:

```text
src/opencode/forward/stream/execute.rs
src/opencode/forward/stream/context.rs
```

Transport safeguards:

- Bounded SSE line length.
- UTF-8 fragments are reassembled safely.
- Malformed SSE lines cannot panic the stream.
- Terminal events are emitted once.
- Client disconnect cancels upstream body consumption.

Rolling state is held separately for reasoning, visible text, native calls, DSML, and compatibility markers.

## 6. Native tool calls

Native OpenAI tool fragments are stored by source index in a `BTreeMap`.

They are not emitted incrementally. At the end of the upstream turn—or at EOF/`[DONE]`—the complete set is validated atomically:

- Every call has a name.
- Every name exists in the request tool inventory.
- Arguments are strict JSON objects.
- All calls in the set validate before any `tool_use` is emitted.
- A search call cannot be mixed with another call in the same set.

Failure policy:

```text
No tool emitted yet -> retry upstream with correction instruction.
A tool was already emitted -> suppress replay retry to prevent duplicate side effects.
```

## 7. Markdown/code context scanner

Shared state:

```text
src/opencode/forward/common.rs::CompatMarkdownState
```

Tracked contexts:

- Backtick and tilde fenced code blocks.
- Inline backtick code.
- Double-quoted/JSON string regions.
- Markdown blockquote lines.
- Escaped markers.

Compatibility and DSML syntax is executable only outside these contexts.

This prevents examples such as the following from executing:

```text
```text
[Requesting Read with arguments: {"file_path":"secret"}]
<｜DSML｜tool_calls>...</｜DSML｜tool_calls>
```
```

## 8. Compatibility-marker parser

Core structures:

```text
CompatToolCall
ParsedCompatMarker
CompatExtraction
```

Core functions:

```text
parse_compat_tool_requests_with_consumed
parse_compat_tool_requests_at_eof
extract_compat_tool_requests_detailed
```

Supported syntax:

```text
[Requesting Tool execution: 'Read' with arguments: {...}]
[Requesting Read with arguments: {...}]
[Requesting TaskUpdate with arguments: {...}, {...}]
```

Batch representation is explicit `Vec<CompatToolCall>`. A legitimate JSON array remains one tool input and is never used as a batch sentinel.

Limits:

- 64 KiB argument sequence.
- 32 calls per marker batch.
- 128 compatibility calls per complete response.
- Streaming buffer limits remain enforced independently.

JSON policy:

1. Strict JSON first.
2. Conservative recovery only after strict parsing fails.
3. Semantically duplicate recovery candidates are collapsed.
4. Recovery is accepted only when exactly one semantic interpretation remains.
5. Malformed multi-call batches are never partially executed.

Recovery policy:

- A malformed marker may resynchronize at a later independent valid marker.
- Recovery cannot absorb a later marker into a repaired JSON string.
- `consumed` is always checked through tests to be within input and on a UTF-8 boundary.

## 9. DSML parser

Entry points:

```text
src/opencode/sanitize.rs::parse_dsml_tool_calls_detailed
src/opencode/sanitize.rs::extract_and_clean_dsml_detailed
```

DSML uses the same context scanner as compatibility markers.

A DSML wrapper is executable only when:

- The wrapper is closed.
- Invoke and parameter open/close counts match.
- Every invocation parses.
- All parsed calls pass the same atomic tool availability/search policy used by compatibility calls.

Incomplete wrappers and partially malformed batches emit zero calls.

## 10. Search interception

Bridge-executed search tools are intercepted only after complete arguments have been validated.

Policy:

- Exactly one search invocation may be intercepted in a turn.
- A batch containing search plus any other invocation is rejected and retried.
- Duplicate queries and configured search budget use the existing final-synthesis guard.
- `WebFetch` remains a Claude Code client tool and is forwarded as ordinary `tool_use`.

## 11. Retry and duplicate-side-effect invariant

The critical invariant is:

```text
Once any tool_use has been emitted to Claude Code, the same upstream turn is never replayed.
```

This applies to compatibility markers, DSML, and native tool calls.

Before emission, retries are bounded and correction prompts enumerate the actual available tools and require one invocation per marker.

## 12. Outbound Anthropic protocol

Formatting components:

```text
src/sse.rs
src/stream_tracker.rs
src/opencode/forward/sync.rs
src/opencode/forward/stream/context.rs
```

Output invariants:

- Tool input is a JSON object.
- Tool blocks are closed exactly once.
- `stop_reason=tool_use` only when a real tool was emitted or a valid search was intercepted.
- Raw executable markers do not leak after classification as protocol intent.
- Unknown tools do not become apparently successful final text.
- A response always contains a valid Anthropic content shape.

## 13. Verification layers

Unit and fixture tests:

```text
src/opencode/forward/stream/tests.rs
tests/fixtures/compat_markers.json
src/opencode/forward/common.rs::compat_parser_invariant_tests
src/opencode/sanitize.rs tests
src/handlers/messages.rs::request_validation_tests
```

Protocol tests:

```text
tests/protocol_conformance.rs
```

Fuzz smoke:

```text
tests/parser_fuzz_smoke.rs
```

Required final verification:

```text
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked --bins
real bridge restart
fake-upstream E2E
Claude Code CLI E2E
```
