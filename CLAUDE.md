# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development

```bash
# Build
cargo build
cargo build --release    # LTO, single codegen unit, strip

# Test (unit)
cargo test

# Test (integration — spawns bridge on a free port, requires release build first)
cargo build --release && cargo test --test integration -- --ignored

# Run a single test
cargo test test_name
cargo test --test integration test_shell_command_non_streaming -- --ignored

# Lint & format
cargo fmt --check
cargo clippy -- -D warnings

# Run
cargo run
RUST_LOG=debug cargo run
source start.sh                     # build + daemon + bridge + env export
./stop.sh                           # graceful shutdown + Docker cleanup
```

CI runs on push/PR: `cargo fmt --check` → `cargo clippy` → `cargo test` → `cargo build --release` (`.github/workflows/ci.yml`).

Release workflow (`.github/workflows/release.yml`): builds linux + macOS (amd64/arm64) binaries, publishes to crates.io, builds/pushes Docker image to ghcr.io.

## Project Overview

**OpenCode2Claude** (~4.2k LOC) is a local HTTP proxy that translates Anthropic Messages API requests into OpenAI-compatible API calls. It allows Claude Code to use any LLM provider supported by the OpenCode platform — DeepSeek, GPT-4o, Gemini, Llama, etc.

### Data Flow

```
Claude Code → opencode2claude (:4000) → opencode.ai/zen/v1/chat/completions → Any LLM
                              ↓
                        !shell commands (bypass LLM, local exec)
```

Unlike earlier versions, the bridge now communicates **directly with the upstream OpenAI-compatible API** (no subprocess to an OpenCode daemon). The daemon health check on `:4096/doc` is purely a monitoring indicator, not a routing dependency.

## Architecture

### Modules

```
src/
├── main.rs               # Router, startup, graceful shutdown
├── config.rs             # Config chain: CLI args > Env vars > TOML > Defaults
├── handlers.rs           # Parse Anthropic requests, route to shell/upstream
├── state.rs              # AppState: shared config, HTTP/search clients, proxy pool, rate limiter
├── error.rs              # BridgeError enum → HTTP error responses
├── middleware.rs         # Bearer token auth (skips /health)
├── proxy_pool.rs        # Multi-agent proxy routing with hash-based assignment & failover
├── shell.rs              # !command execution via sh -c with ShellPolicy
├── sse.rs                # SseEventBuilder — Anthropic SSE protocol
└── opencode/             # Direct upstream API gateway (no subprocess)
    ├── mod.rs            # Re-exports
    ├── forward.rs        # HTTP forwarding, WARP retry, search interception
    ├── mapper.rs         # Anthropic Messages → OpenAI Chat Completions format
    ├── search.rs         # Web search with 5-provider fallback chain
    └── types.rs          # OpenAI API request/response types
```

### Router (defined in `main.rs`)

| Method | Path           | Handler                    | Auth | Purpose                         |
|--------|----------------|----------------------------|------|---------------------------------|
| POST   | `/v1/messages` | `handle_messages`           | Yes  | Translate Anthropic request → upstream call or shell |
| GET    | `/v1/models`   | `handle_models`             | Yes  | Return configured model list    |
| GET    | `/health`      | `handle_health`             | No   | Health check (incl. daemon status) |

### Key Components

| Module | File | Role |
|--------|------|------|
| **handlers** | `handlers.rs` | Extract prompt from Anthropic Messages array, detect `!` shell prefix, route to shell or API forwarding, acquire rate limiter permit |
| **forward** | `opencode/forward.rs` | Core: sends OpenAI-compatible POST to `opencode.ai/zen/v1/chat/completions`, handles sync/streaming, **WARP IP rotation** on rate-limit, **search tool interception** (detects web search, loops back with results, max 5 loops) |
| **mapper** | `opencode/mapper.rs` | Converts Anthropic request format → OpenAI format: system prompts, tool results, tool choice mapping, model name normalization (e.g. `deepseek-v4-flash` → `deepseek-v4-flash-free`) |
| **search** | `opencode/search.rs` | `SearchClient` with 5-provider fallback: Tavily → Exa → Serper → SearXNG → DuckDuckGo. Shipped with `DEFAULT_MODEL` (`claude-3-5-sonnet`) as requested. DuckDuckGo works without any API key. |
| **proxy_pool** | `proxy_pool.rs` | Multi-agent proxy routing: hash API key → preferred proxy index, failover on 429/503, adaptive cooldown (exponential backoff 2^n min × 60s), per-key deterministic assignment |
| **shell** | `shell.rs` | `ShellPolicy` enum (Disabled/AllowList/Unrestricted), sync and streaming (`tokio::mpsc` + SSE) command execution |
| **sse** | `sse.rs` | `SseEventBuilder` — unified builder for Anthropic SSE events (message_start, content_block_start/delta/stop, message_delta, message_stop), used by both shell and upstream paths |
| **state** | `state.rs` | `AppState` holding `Arc<BridgeConfig>`, shared reqwest client, `SearchClient`, optional `Arc<Semaphore>` rate limiter, `Arc<RwLock<ProxyPool>>` |
| **middleware** | `middleware.rs` | Bearer token validation against configured auth tokens, skips `/health`, passes through when auth is disabled |
| **error** | `error.rs` | `BridgeError` enum mapped to Anthropic JSON error responses with correct HTTP status codes (400/401/403/502) |

### Key Design Decisions

1. **Direct API gateway** — The bridge posts directly to `https://opencode.ai/zen/v1/chat/completions` (OpenAI-compatible endpoint). No OpenCode subprocess is spawned. The daemon health check is purely for monitoring.

2. **Rate-limit resilience** — On 429/503 or network errors, the bridge enters a retry loop: proxy-based requests mark the failing proxy with adaptive cooldown (2^retry min × 60s); non-proxy requests rotate via `warp-cli disconnect/connect`. Exponential backoff starts at 2s, caps at 16s.

3. **Search tool interception** — When the model calls a web search tool (`web_search`, `web_fetch`, `webfetch`, `websearch`), the bridge intercepts, runs the search via its fallback chain, appends the results as a `tool_result` message, and sends the updated conversation back to the model. Limited to 5 loops.

4. **Multi-agent proxy routing** — Each API key is hashed to a deterministic proxy index in the pool. On rate-limit, the bridge fails over to the next active proxy. Containerized WARP setup via Docker is automated in `start.sh`/`stop.sh`.

5. **Streaming via Tokio MPSC** — Both shell execution and upstream API streaming use `tokio::sync::mpsc` + `tokio_stream::wrappers::ReceiverStream` to pipe real-time data into Anthropic SSE events.

6. **Shell interception** — Prompts starting with `!` run locally via `sh -c`, bypassing LLM entirely. Three policy levels: `disabled`, `allowlist`, `unrestricted`.

7. **Config priority chain** — CLI args > Env vars > TOML file > Hardcoded defaults.

8. **Request body limit** — 1MB default via `tower_http::limit::RequestBodyLimitLayer`.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `BRIDGE_PORT` | `4000` | Bridge listen port |
| `BRIDGE_HOST` | `127.0.0.1` | Bind address |
| `OPENCODE_PORT` | `4096` | OpenCode daemon health-check port |
| `OPENCODE_MODEL` | (none) | Target LLM model |
| `BRIDGE_SHELL_POLICY` | `unrestricted` | Shell policy: `disabled`, `allowlist`, `unrestricted` |
| `BRIDGE_SHELL_ALLOWLIST` | `git,ls,pwd,...` | Comma-separated allowed commands (when policy=allowlist) |
| `BRIDGE_AUTH_TOKEN` | (none) | Comma-separated Bearer tokens (empty = auth disabled) |
| `BRIDGE_RATE_LIMIT` | (none) | Max concurrent requests (via tokio::Semaphore) |
| `BRIDGE_MAX_BODY_SIZE` | `1048576` | Max request body (bytes) |
| `BRIDGE_MAX_SEARCH_LOOPS` | `5` | Max web search tool-call loops |
| `BRIDGE_STREAM_BUFFER_SIZE` | `4096` | Streaming read buffer size |
| `BRIDGE_CHANNEL_CAPACITY` | `256` | SSE channel queue capacity |
| `BRIDGE_PROXIES` | (none) | Comma-separated SOCKS5/HTTP proxies for multi-agent IP mapping |
| `TAVILY_API_KEY` | (none) | Tavily search API key (1st priority in fallback) |
| `EXA_API_KEY` | (none) | Exa search API key (2nd priority) |
| `SERPER_API_KEY` | (none) | Serper.dev search API key (3rd priority) |
| `SEARXNG_URL` | (none) | Self-hosted SearXNG instance URL (4th priority) |
| `PROXY_POOL_SIZE` | `3` | Docker WARP proxy containers to spawn (when Docker is available) |

### CLI Flags (override all config sources)

```
-p, --port              Bridge port
--host                  Bind address
-c, --config            TOML config path
-m, --model             Model override
--shell-policy          Shell policy override
--tavily-api-key        Tavily search API key
--exa-api-key           Exa search API key
--serper-api-key        Serper.dev search API key
--searxng-url           SearXNG instance URL
--searxng-api-key       SearXNG API key
-v, --version           Print version
```

### TOML Config

Create `opencode2claude.toml` in project root (or use `-c`):
```toml
port = 4000
model = "openai/gpt-4o"
shell_policy = "allowlist"
shell_allowlist = "git,ls,pwd,echo"
auth_tokens = "sk-123,sk-456"
proxies = ["socks5://127.0.0.1:40001", "socks5://127.0.0.1:40002"]
tavily_api_key = "tvly-..."
max_search_loops = 5
```

## Testing

**Unit tests** are colocated in `#[cfg(test)]` modules within each source file:
- `config.rs` — env var priority, TOML parsing, auth validation
- `handlers.rs` — prompt extraction from various content formats
- `mapper.rs` — Anthropic → OpenAI conversion, model name mapping, search tool detection
- `proxy_pool.rs` — hash assignment, failover on rate-limit, exclusion routing
- `search.rs` — URL encoding/decoding, HTML stripping
- `shell.rs` — ShellPolicy checks, base command extraction
- `sse.rs` — Event construction, non-streaming response format
- `state.rs` — AppState client creation

**Integration tests** (`tests/`):
- `cargo build --release && cargo test --test integration -- --ignored`
- Spawn a real bridge binary, test HTTP endpoints, streaming SSE, auth flow, shell policies, concurrent requests, rate limiting, multi-token auth, proxy failover
- Test harness in `tests/common/mod.rs`: `TestBridge` struct auto-assigns free ports, supports env overrides

Route admin — if changing endpoints, update the router in `main.rs` and the auth middleware's `/health` path check in `middleware.rs`.
