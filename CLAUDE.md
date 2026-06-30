# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development

```bash
# Build
cargo build
cargo build --release     # LTO, single codegen unit, strip

# Run (foreground)
cargo run
RUST_LOG=debug cargo run

# Run (background daemon via supervisor)
cargo build && ./target/debug/opencode2claude start
./target/debug/opencode2claude status
./target/debug/opencode2claude stop

# Quick start with Docker proxy pool (legacy wrapper)
cargo build && source start.sh
./stop.sh

# Test (unit + fast integration)
cargo test
cargo test --locked       # respect Cargo.lock

# Test (full integration — requires release build first)
cargo build --release && cargo test --test integration -- --ignored

# Run a single test
cargo test test_name
cargo test --test integration test_shell_command_non_streaming -- --ignored
cargo test --test fast test_health_endpoint_fast

# Lint & format
cargo fmt --check
cargo clippy -- -D warnings

# Verification (8 phases, 90+ gates)
./scripts/verify.sh phase-3 --profile ci   # Single phase
./scripts/verify.sh all --profile ci        # All enabled phases
```

**CI** (`.github/workflows/ci.yml`): fmt → clippy → test → build-release + shellcheck.
**Release** (`.github/workflows/release.yml`): 4-target binary build (linux amd64/arm64, macOS amd64/arm64) → GitHub Releases → crates.io publish → ghcr.io Docker image.

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

### Modules (~10k LOC)

```
src/
├── main.rs               # Router, startup, graceful shutdown
├── lib.rs                # Library entry point (in-process embedding)
├── cli.rs                # CLI argument parsing via Clap subcommands
├── config.rs             # Config chain: CLI args > Env vars > TOML > Defaults
├── handlers.rs           # Parse Anthropic requests, route to shell/upstream
├── state.rs              # AppState: shared config, HTTP/search clients, proxy pool, rate limiter
├── error.rs              # BridgeError enum → HTTP error responses
├── middleware.rs         # Bearer token auth (skips /health)
├── shell.rs              # !command execution via sh -c with ShellPolicy
├── sse.rs                # SseEventBuilder — Anthropic SSE protocol
├── supervisor.rs         # Bridge daemon lifecycle (start/stop/status)
├── docker.rs             # Docker proxy container management
├── pidfile.rs            # PID file read/write
├── runtime.rs            # Runtime path resolution (.runtime/ dir)
├── opencode/             # Direct upstream API gateway
│   ├── mod.rs            # Re-exports
│   ├── forward.rs        # HTTP forwarding, WARP retry, search interception
│   ├── mapper.rs         # Anthropic Messages → OpenAI Chat Completions
│   ├── search.rs         # Web search with 5-provider fallback chain
│   ├── retry.rs          # Retry loop with adaptive cooldown
│   ├── sanitize.rs       # DSML tag stripping and tool-call parsing
│   └── types.rs          # OpenAI API request/response types
└── proxy_pool/           # 2-tier proxy pool (split module)
    ├── mod.rs            # Pool lifecycle, health telemetry, cooldown/recovery
    ├── routing.rs        # Primary-first Rendezvous routing + WarmStandby failover
    ├── maintenance.rs    # Docker container restart/purge queue
    └── types.rs          # ProxyNode, ProxyTier, ProxyStatus enums

tests/
├── common/mod.rs         # TestBridge harness (auto-assigns free ports, env overrides)
├── fast.rs               # 5 fast smoke tests (no release build needed)
└── integration.rs        # 18 full integration tests (ignored, require --release)
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
| **proxy_pool** | `proxy_pool/mod.rs`, `routing.rs`, `maintenance.rs`, `types.rs` | 2-tier proxy pool: Primary (40001–40003) + WarmStandby (40004–40005). Primary-first Rendezvous routing with affected-agent-only failover, adaptive cooldown, auto-recovery after RECOVERY_SUCCESS_COUNT successes, /health snapshot telemetry |
| **shell** | `shell.rs` | `ShellPolicy` enum (Disabled/AllowList/Unrestricted), sync and streaming (`tokio::mpsc` + SSE) command execution |
| **sse** | `sse.rs` | `SseEventBuilder` — unified builder for Anthropic SSE events (message_start, content_block_start/delta/stop, message_delta, message_stop), used by both shell and upstream paths |
| **state** | `state.rs` | `AppState` holding `Arc<BridgeConfig>`, shared reqwest client, `SearchClient`, optional `Arc<Semaphore>` rate limiter, `Arc<RwLock<ProxyPool>>` |
| **supervisor** | `supervisor.rs` | Bridge daemon lifecycle — `start`/`stop`/`status` subcommands with PID-based supervision |
| **docker** | `docker.rs` | Docker proxy container management — create container, list containers, get logs, remove container |
| **pidfile** | `pidfile.rs` | PID file serialization for supervisor daemon tracking |
| **runtime** | `runtime.rs` | Runtime path resolution: `.runtime/`, PID file, log file paths |
| **middleware** | `middleware.rs` | Bearer token validation against configured auth tokens, skips `/health`, passes through when auth is disabled |
| **error** | `error.rs` | `BridgeError` enum mapped to Anthropic JSON error responses with correct HTTP status codes (400/401/403/502) |

### Key Design Decisions

1. **Direct API gateway** — The bridge posts directly to `https://opencode.ai/zen/v1/chat/completions` (OpenAI-compatible endpoint). No OpenCode subprocess is spawned. The daemon health check is purely for monitoring.

2. **Rate-limit resilience** — On 429/503 or network errors, the bridge enters a retry loop: proxy-based requests mark the failing proxy with adaptive cooldown (2^retry min × 60s); non-proxy requests rotate via `warp-cli disconnect/connect`. Exponential backoff starts at 2s, caps at 16s. Note: rate-limit cooldown (`mark_rate_limited`) is separate from transport failure (`record_failure`). HTTP-level errors do NOT mark proxy transport failure.

3. **Proxy telemetry** — `forward.rs` calls `record_success(idx)` after any HTTP response (including 4xx/429/5xx), and `record_failure(idx)` only on transport/network errors. After `FAILURE_THRESHOLD` (2) consecutive failures, the proxy enters cooldown. After `RECOVERY_SUCCESS_COUNT` (2) consecutive successes, it auto-recovers from cooldown. The `/health` endpoint exposes full proxy pool snapshots.

4. **Search tool interception** — When the model calls a web search tool (`web_search`, `web_fetch`, `webfetch`, `websearch`), the bridge intercepts, runs the search via its fallback chain, appends the results as a `tool_result` message, and sends the updated conversation back to the model. Limited to 5 loops.

5. **Multi-agent proxy routing** — 2-tier architecture: Primary Managed Pool (40001–40003) for normal traffic, Warm-Standby Protected Pool (40004–40005) for failover only. Each API key is hashed via Rendezvous (highest random weight) to a deterministic primary proxy. On failure, only the affected agent fails over to WarmStandby — healthy primaries keep their agents. Containerized WARP setup via Docker is automated in `start.sh`/`stop.sh`.

6. **Streaming via Tokio MPSC** — Both shell execution and upstream API streaming use `tokio::sync::mpsc` + `tokio_stream::wrappers::ReceiverStream` to pipe real-time data into Anthropic SSE events.

7. **Shell interception** — Prompts starting with `!` run locally via `sh -c`, bypassing LLM entirely. Three policy levels: `disabled`, `allowlist`, `unrestricted`.

8. **Config priority chain** — CLI args > Env vars > TOML file > Hardcoded defaults.

9. **Request body limit** — 1MB default via `tower_http::limit::RequestBodyLimitLayer`.

10. **Supervisor daemon** — The `start`/`stop`/`status` subcommands use a process supervisor with PID file tracking in `.runtime/`. `start` spawns `serve` as a detached child via `setsid()`. `stop` sends SIGTERM, waits, then SIGKILL if needed.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `BRIDGE_PORT` | `4000` | Bridge listen port |
| `BRIDGE_HOST` | `127.0.0.1` | Bind address |
| `OPENCODE_PORT` | `4096` | OpenCode daemon health-check port |
| `OPENCODE_MODEL` | (none) | Target LLM model |
| `BRIDGE_SHELL_POLICY` | `disabled` | Shell policy: `disabled`, `allowlist`, `unrestricted` |
| `BRIDGE_SHELL_ALLOWLIST` | `git,ls,pwd,...` | Comma-separated allowed commands (when policy=allowlist) |
| `BRIDGE_AUTH_TOKEN` | (none) | Comma-separated Bearer tokens (empty = auth disabled) |
| `BRIDGE_RATE_LIMIT` | (none) | Max concurrent requests (via tokio::Semaphore) |
| `BRIDGE_MAX_BODY_SIZE` | `1048576` | Max request body (bytes) |
| `BRIDGE_MAX_SEARCH_LOOPS` | `5` | Max web search tool-call loops |
| `BRIDGE_STREAM_BUFFER_SIZE` | `4096` | Streaming read buffer size |
| `BRIDGE_CHANNEL_CAPACITY` | `256` | SSE channel queue capacity |
| `BRIDGE_PRIMARY_PROXIES` | (none) | Primary proxy URLs, comma-separated (e.g. `socks5://127.0.0.1:40001,...`) |
| `BRIDGE_WARM_STANDBY_PROXIES` | (none) | Warm-standby proxy URLs (protected from CLI modification) |
| `BRIDGE_PROXIES` | (none) | Legacy — comma-separated SOCKS5/HTTP proxies (deprecated, prefer PRIMARY + WARM_STANDBY) |
| `TAVILY_API_KEY` | (none) | Tavily search API key (1st priority in fallback) |
| `EXA_API_KEY` | (none) | Exa search API key (2nd priority) |
| `SERPER_API_KEY` | (none) | Serper.dev search API key (3rd priority) |
| `SEARXNG_URL` | (none) | Self-hosted SearXNG instance URL (4th priority) |

### CLI Subcommands

```
opencode2claude serve [OPTIONS]   Start API bridge server (foreground)
opencode2claude start [OPTIONS]   Start bridge as background daemon
opencode2claude status [OPTIONS]  Show bridge status
opencode2claude stop [OPTIONS]    Stop the bridge
opencode2claude restart           Restart the bridge
opencode2claude logs              View bridge logs
opencode2claude env               Display environment information
opencode2claude proxy status/ps   List proxy pool with roles and health
opencode2claude proxy restart     Recreate primary proxy containers (40001-40003)
opencode2claude proxy purge       Remove + recreate primary proxy containers
opencode2claude proxy logs        View proxy container logs
```

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
primary_proxies = ["socks5://127.0.0.1:40001", "socks5://127.0.0.1:40002", "socks5://127.0.0.1:40003"]
warm_standby_proxies = ["socks5://127.0.0.1:40004", "socks5://127.0.0.1:40005"]
tavily_api_key = "tvly-..."
max_search_loops = 5
```

## Testing

**Unit tests** are colocated in `#[cfg(test)]` modules within each source file.

**Test tiers** (3 files in `tests/`):

| File | Type | Count | Run with |
|------|------|-------|----------|
| `tests/fast.rs` | Smoke (no release build) | 5 | `cargo test --test fast` |
| `tests/integration.rs` | Full (spawns real bridge) | 18 | `cargo build --release && cargo test --test integration -- --ignored` |
| `tests/common/mod.rs` | Shared `TestBridge` harness | — | imported by both fast and integration |

Key test modules in source:
- `config.rs` — env var priority, TOML parsing, auth validation, **security validation** (public-bind guards, default shell policy)
- `proxy_pool/` — Rendezvous hashing, routing, cooldown/recovery, telemetry snapshot
- `shell.rs` — ShellPolicy checks, metacharacter detection
- `opencode/` — mapper conversion, search URL encoding, DSML sanitization, SSE events
- `middleware.rs` — Bearer auth, health-skip
- `state.rs` — AppState client creation

**Verification**: 8 phase scripts in `scripts/phases/`, each with 7–17 gates. Invoked via `./scripts/verify.sh all --profile ci`. Phase gates are bash functions that return 0 (pass) or 1 (fail).

Route admin — if changing endpoints, update the router in `main.rs` and the auth middleware's `/health` path check in `middleware.rs`.
