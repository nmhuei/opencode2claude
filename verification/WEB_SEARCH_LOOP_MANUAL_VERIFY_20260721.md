# Web Search Loop Manual Verification — 2026-07-21

## Status

**PASS**

Verified with Claude Code CLI `2.1.216` through the release `opencode2api` bridge.

## Original failure

Claude Code research subagents terminated with:

```text
API Error: Upstream response did not contain content blocks
(reason: search_loop_protection)
```

The exact affected agent descriptions were:

- `Search Claude Code security skills`
- `Search MCP servers for security`

## Root causes

1. Streaming tool-call arguments were stored in one global buffer rather than by OpenAI `tool_call.index`.
2. Argument fragments arriving before the tool name could be discarded.
3. Parallel tool calls could mix their argument fragments.
4. Query extraction only supported a shallow JSON shape, so nested WebSearch payloads became an empty query.
5. DSML and native WebSearch calls followed different interception paths.
6. Free models sometimes emitted a compatibility marker as text:

   ```text
   [Requesting Tool execution: 'WebSearch' with arguments: {...}]
   ```

7. Search-loop exhaustion emitted an Anthropic `error` event without a content block.
8. The unauthenticated DuckDuckGo HTML endpoint returned a bot CAPTCHA page in this runtime, producing no search results.

## Implementation

### Tool-call parsing

- Added per-index streamed tool-call accumulation.
- Preserved argument fragments received before the function name.
- Prevented parallel fragments from being combined.
- Added recursive query extraction for nested arrays/objects and common fields.
- Added bounded fallback from the latest user request when the provider sends malformed/empty arguments.
- Unified native and DSML WebSearch interception.
- Intercepted free-model compatibility markers without leaking them to Claude Code.

### Loop recovery

- Added normalized query cache.
- Duplicate queries no longer execute the network search twice.
- Search budget exhaustion removes WebSearch/WebFetch and schedules a final synthesis turn.
- If the model still requests another search, the bridge returns the sourced results already collected.
- Hard limits now finish with Anthropic text content plus `message_stop`, never an empty-content `api_error`.

### Provider fallback

Fallback order is now:

```text
Tavily → Exa → Serper → SearXNG → DuckDuckGo → Yahoo
```

Yahoo HTML search is the final no-key fallback. Redirect result URLs are decoded to their real target URLs. Responses remain bounded by the configured timeout, result count, snippet size, and response-size limits.

New configuration:

```toml
yahoo_url = "https://search.yahoo.com/search"
```

Environment override:

```text
YAHOO_SEARCH_URL
```

## Automated verification

Final suite:

```text
327 unit
87 fast/dashboard
18 integration
2 parser fuzz
12 protocol conformance
-----------------------
446 PASS
```

One real WARP system test remains ignored by design because it requires live local WARP SOCKS proxies and Internet access.

Quality gates:

```text
git diff --check                          PASS
node --check src/webui/app.js            PASS
cargo fmt --check                        PASS
cargo clippy ... -D warnings             PASS
cargo test --all-targets --all-features  PASS
cargo build --release --bins             PASS
```

## Manual scenarios

### Direct WebSearch

Artifact:

```text
artifacts/search-loop-manual/direct-websearch-final.json
```

Result:

- Valid sourced response
- Official Claude Code security URL present
- No `search_loop_protection`
- No `API Error`
- No compatibility marker
- DuckDuckGo CAPTCHA/no-result correctly fell back to Yahoo

### Exact parallel subagents

Artifact:

```text
artifacts/search-loop-manual/final-parallel-agents.json
artifacts/search-loop-manual/final-parallel-agents-summary.json
```

Result:

- Both exact agent descriptions ran in parallel
- Six or more source URLs returned
- No premature agent termination
- No `search_loop_protection`
- No `API Error`
- No compatibility marker

### Forced loop exhaustion

Script:

```text
scripts/manual_verify_search_loop_guard.py
```

Configuration:

```toml
max_search_loops = 1
```

Artifact:

```text
artifacts/search-loop-manual/loop-budget-one-summary.json
```

Result:

- Exit code 0
- Valid final content with source URLs
- No error event
- No search-loop reason
- No compatibility marker

## Final runtime

Stable bridge:

```text
http://127.0.0.1:4000
model: opencode/deepseek-v4-flash-free
version: 0.5.0
```
