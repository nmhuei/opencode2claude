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

# Lint & format
cargo fmt --check
cargo clippy -- -D warnings

# Run
cargo run
RUST_LOG=debug cargo run
source start.sh                     # build + daemon + bridge + env export
./stop.sh                           # graceful shutdown
```

CI runs on push/PR: `cargo fmt --check` → `cargo clippy` → `cargo test` → `cargo build --release` (`.github/workflows/ci.yml`).

## Project Overview

**OpenCode2Claude** is a local HTTP proxy that translates Anthropic Messages API requests into OpenCode CLI commands. It allows Claude Code (or any Anthropic-compatible agent) to use any LLM provider supported by OpenCode — DeepSeek, GPT-4o, Gemini, Llama, etc.

### Data Flow

```
Claude Code → opencode2claude (:4000) → OpenCode CLI (:4096) → Any LLM
                              ↓
                        !shell commands (bypass LLM, local exec)
```

## Architecture

### Router (defined in `src/main.rs`)

| Method | Path              | Handler                  | Auth | Purpose                     |
|--------|-------------------|--------------------------|------|-----------------------------|
| POST   | `/v1/messages`    | `handlers::handle_messages` | Yes | Main API — translate Anthropic request to OpenCode/shell |
| GET    | `/v1/models`      | `handlers::handle_models`   | Yes | Return configured model list |
| GET    | `/health`         | `handlers::handle_health`   | No  | Health check for Docker/monitoring |

### Module Responsibilities

| Module | File | Role |
|--------|------|------|
| **handlers** | `handlers.rs` | Parse Anthropic Messages API requests, extract prompt, detect `!` shell prefix, route to shell or opencode |
| **config** | `config.rs` | Config chain: CLI args > Env vars > TOML file > Defaults |
| **opencode** | `opencode.rs` | Daemon detection (HTTP check on `:4096/doc`), build `opencode run` command with `--attach` or standalone mode, sync/stream execution |
| **shell** | `shell.rs` | `!` command execution via `sh -c` with configurable security policy (Disabled / AllowList / Unrestricted) |
| **sse** | `sse.rs` | `SseEventBuilder` — constructs Anthropic-compatible SSE events (message_start, content_block_start/delta/stop, message_delta, message_stop) |
| **middleware** | `middleware.rs` | Bearer token auth (skips `/health`). When `auth_tokens` is `None`, all requests pass through. |
| **error** | `error.rs` | `BridgeError` enum → Anthropic JSON error responses with correct HTTP status codes |
| **state** | `state.rs` | `AppState` — shared config (`Arc<BridgeConfig>`) + pooled reqwest client (500ms timeout) |

### Key Design Decisions

1. **Streaming via Tokio MPSC** — Both shell and OpenCode execution use `tokio::sync::mpsc` + `tokio_stream::wrappers::ReceiverStream` to pipe subprocess stdout/stderr into SSE events
2. **Shell interception** — Prompts starting with `!` run locally via `sh -c`, bypassing LLM entirely
3. **Smart daemon detection** — Pings OpenCode daemon `/doc` before running; attaches with `--attach` if available, falls back to standalone mode
4. **Config priority chain** — CLI > Env > TOML > Defaults (see `config.rs:161`)
5. **Graceful shutdown** — Listens for SIGINT/SIGTERM via `tokio::signal` (see `main.rs:139`)
6. **`--dangerously-skip-permissions`** — OpenCode runs with this flag to suppress permission prompts (see `opencode.rs:35`)
7. **Request body limit** — 1MB default via `tower_http::limit::RequestBodyLimitLayer`

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `BRIDGE_PORT` | `4000` | Bridge listen port |
| `BRIDGE_HOST` | `127.0.0.1` | Bind address (warns if `0.0.0.0`) |
| `OPENCODE_PORT` | `4096` | OpenCode daemon port |
| `OPENCODE_MODEL` | (none) | Target LLM model (defaults to `claude-3-5-sonnet` fallback in DEFAULT_MODEL) |
| `BRIDGE_SHELL_POLICY` | `unrestricted` | Shell policy: `disabled`, `allowlist`, `unrestricted` |
| `BRIDGE_SHELL_ALLOWLIST` | `git,ls,pwd,cat,...` | Comma-separated allowed commands (when policy=allowlist) |
| `BRIDGE_AUTH_TOKEN` | (none) | Comma-separated Bearer tokens (unset = auth disabled) |
| `BRIDGE_MAX_BODY_SIZE` | `1048576` | Max request body (bytes) |
| `BRIDGE_STREAM_BUFFER_SIZE` | `4096` | Streaming read buffer size |
| `BRIDGE_CHANNEL_CAPACITY` | `256` | SSE channel queue capacity |

### CLI Flags (override all other config sources)

```
-p, --port              Bridge port
--host                  Bind address
-c, --config            TOML config path
-m, --model             Model override
--shell-policy          Shell policy override
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
```

Route admin — if changing endpoints, update the router in `main.rs` and the auth middleware's `/health` path check in `middleware.rs`.
