<div align="center">

# 🌉 OpenCode2Claude

### **Use Claude Code with any LLM — for free.**

A blazing-fast local API bridge written in **Rust** that lets you connect **Claude Code** (or any Anthropic-compatible agent) to [**OpenCode CLI**](https://github.com/opencode-ai/opencode) and its universe of models.

[![Rust](https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Axum](https://img.shields.io/badge/Axum-0.7-blue?style=for-the-badge)](https://github.com/tokio-rs/axum)
[![Tokio](https://img.shields.io/badge/Tokio-async-green?style=for-the-badge)](https://tokio.rs/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=for-the-badge)](LICENSE)

<br>

**Claude Code** → `opencode2claude` → **OpenCode CLI** → **Any LLM**
`<br>`
`<sub>`DeepSeek · GPT-4o · Gemini · Llama · Mistral · Qwen · and more...`</sub>`

<br>

[Quick Start](#-quick-start) •
[How It Works](#-how-it-works) •
[Features](#-features) •
[Configuration](#-configuration) •
[Contributing](#-contributing)

</div>

---

## ❓ Why OpenCode2Claude?

**Claude Code** is an incredible AI coding agent — but it's locked to Anthropic's API and pricing. **OpenCode** supports dozens of LLM providers (including free tiers). This bridge connects them seamlessly.

| Without this bridge | With this bridge |
| :-----------------: | :--------------: |
| Claude Code → Anthropic API only | Claude Code → **Any LLM provider** |
| 💸 Pay per token | 🆓 Free models available |
| 1 model | 🌐 50+ models |

### Key Benefits

- 🆓 **Use free models** — Route Claude Code through free-tier models like `deepseek-v4-flash-free`
- ⚡ **Near-zero latency** — Rust + Axum + Tokio delivers hardware-level I/O performance
- 🔌 **Drop-in replacement** — Just set 2 environment variables, no code changes needed
- 📡 **Real-time streaming** — Full SSE (Server-Sent Events) support matching the Anthropic protocol
- 🖥️ **Shell passthrough** — Execute local commands instantly with `!` prefix (0.01s response)
- 🛡️ **Auto WARP IP Rotation** — Automatically rotates IP on rate limits (429/400) via `warp-cli`
- 🚦 **Inbound rate limiting** — Configurable max concurrent requests via `BRIDGE_RATE_LIMIT`
- 📦 **Single binary** — Compile once, copy anywhere. No runtime dependencies

---

## 🚀 Quick Start

### Prerequisites

- **Rust** ≥ 1.70 ([install](https://rustup.rs/))
- **OpenCode CLI** installed and configured ([install](https://github.com/opencode-ai/opencode))

### One-liner Setup

```bash
git clone https://github.com/nmhuei/opencode2claude.git && cd opencode2claude
source start.sh
```

That's it! The script will automatically:

1. ✅ Compile the Rust binary (first run only)
2. ✅ Start the OpenCode daemon in the background
3. ✅ Launch the API bridge on port `4000`
4. ✅ Export all required environment variables

Now just run `claude` in the same terminal and start coding with your chosen model.

---

## ⚙️ How It Works

```
┌─────────────────────────────────────────────────────────────────┐
│                        Your Terminal                            │
│                                                                 │
│  ┌──────────────┐     ┌──────────────────┐    ┌──────────────┐  │
│  │              │     │                  │    │              │  │
│  │  Claude Code │────▶│ opencode2claude  │───▶│ OpenCode CLI │  │
│  │   (Agent)    │◀────│  :4000 (Rust)    │◀───│   Daemon     │  │
│  │              │ SSE │                  │    │   :4096      │  │
│  └──────────────┘     └────────┬─────────┘    └──────┬───────┘  │
│                                │                     │          │
│                     ┌──────────▼─────────┐           │          │
│                     │  ! Shell Commands  │           ▼          │
│                     │  Direct execution  │    ┌──────────────┐  │
│                     │  (bypasses LLM)    │    │  Any LLM API │  │
│                     └────────────────────┘    │  (DeepSeek,  │  │
│                                               │   GPT-4o,    │  │
│                                               │   Gemini...) │  │
│                                               └──────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

The bridge translates between two protocols:

| Incoming (Anthropic API) | Outgoing (OpenCode CLI) |
| ------------------------ | ----------------------- |
| `POST /v1/messages` | `opencode run --attach <daemon>` |
| SSE streaming events | Realtime stdout/stderr streaming |
| `{"role": "user", "content": ...}` | Extracted prompt text |

---

## ✨ Features

### 📡 Full Anthropic SSE Protocol

The bridge implements the complete Anthropic streaming protocol, so Claude Code thinks it's talking to the real API:

```
event: message_start       ─── Session initialization
event: content_block_start ─── Begin response block
event: content_block_delta ─── Streamed text chunks (real-time)
event: content_block_stop  ─── End response block
event: message_delta       ─── Completion metadata
event: message_stop        ─── Session complete
```

### 🖥️ Shell Command Interception

Prefix any prompt with `!` to execute it directly on your local machine — **bypassing the LLM entirely**:

```
You: !git status
→ Executes instantly on local shell (0.01s, zero tokens used)

You: !docker ps
→ Direct terminal output streamed back via SSE

You: What is recursion?
→ Routed through OpenCode → LLM as normal
```

### 🔄 Smart Daemon Detection

The bridge automatically detects whether the OpenCode daemon is running:

- **Daemon active** → Attaches for instant responses (shared context, warm model)
- **Daemon not found** → Falls back to standalone mode (cold start, still works)

### 🛡️ Auto WARP IP Rotation (Anti Rate-Limit)

If you are using free-tier models (like `deepseek-v4-flash-free`), you may occasionally hit provider rate limits. The bridge has a built-in automatic recovery mechanism:

- **Auto-Detect:** Detects upstream `429 Too Many Requests` or `400 Bad Request` rate-limiting responses.
- **Auto-Rotate:** Invokes `warp-cli disconnect` + `warp-cli connect` to acquire a fresh public IP.
- **Auto-Retry:** Seamlessly retries the failed request (up to 3 times).

To use with WARP in local SOCKS5 proxy mode:
```bash
# 1. Set warp-cli to proxy mode (one-time setup)
warp-cli mode proxy

# 2. Run the startup script (auto-detects WARP Proxy and routes bridge traffic through it)
source start.sh
```

### 🚦 Inbound Rate Limiting

```bash
export BRIDGE_RATE_LIMIT=10    # Max 10 concurrent requests (unset = unlimited)
```

---

## 🛠️ Configuration

Configuration priority: **CLI args > Environment vars > TOML file > Defaults**

### Full Environment Variables

| Variable | Default | Description |
| -------- | ------- | ----------- |
| `BRIDGE_PORT` | `4000` | Port the API bridge listens on |
| `BRIDGE_HOST` | `127.0.0.1` | Bind address |
| `OPENCODE_PORT` | `4096` | Port of the OpenCode daemon |
| `OPENCODE_MODEL` | `opencode/deepseek-v4-flash-free` | Target LLM model identifier |
| `BRIDGE_SHELL_POLICY` | `unrestricted` | Shell policy: `disabled`, `allowlist`, `unrestricted` |
| `BRIDGE_SHELL_ALLOWLIST` | `git,ls,pwd,cat,...` | Comma-separated allowed commands |
| `BRIDGE_AUTH_TOKEN` | (none) | Comma-separated Bearer tokens |
| `BRIDGE_MAX_BODY_SIZE` | `1048576` | Max request body (bytes) |
| `BRIDGE_STREAM_BUFFER_SIZE` | `4096` | Streaming read buffer size |
| `BRIDGE_CHANNEL_CAPACITY` | `256` | SSE channel queue capacity |
| `BRIDGE_MAX_SEARCH_LOOPS` | `5` | Max search-interception retries |
| `BRIDGE_RATE_LIMIT` | (none) | Max concurrent requests (unset = unlimited) |
| `TAVILY_API_KEY` | (none) | Tavily web search API key |
| `EXA_API_KEY` | (none) | Exa web search API key |
| `SERPER_API_KEY` | (none) | Serper.dev web search API key |
| `SEARXNG_URL` | (none) | SearXNG self-hosted instance URL |
| `SEARXNG_API_KEY` | (none) | SearXNG API key |

### Using a different model

```bash
# Use GPT-4o
export OPENCODE_MODEL="openai/gpt-4o"
source start.sh

# Use Gemini
export OPENCODE_MODEL="google/gemini-2.5-pro"
source start.sh

# Use a local Ollama model
export OPENCODE_MODEL="ollama/llama3"
source start.sh
```

### CLI Flags

All flags override env vars and TOML config:

| Flag | Maps to |
| ---- | ------- |
| `-p, --port` | `BRIDGE_PORT` |
| `--host` | `BRIDGE_HOST` |
| `-m, --model` | `OPENCODE_MODEL` |
| `-c, --config` | TOML config path |
| `--shell-policy` | `BRIDGE_SHELL_POLICY` |
| `--tavily-api-key` | `TAVILY_API_KEY` |
| `--exa-api-key` | `EXA_API_KEY` |
| `--serper-api-key` | `SERPER_API_KEY` |
| `--searxng-url` | `SEARXNG_URL` |
| `--searxng-api-key` | `SEARXNG_API_KEY` |

### TOML Config File

Create `opencode2claude.toml` in project root (or use `-c`):
```toml
port = 4000
model = "openai/gpt-4o"
shell_policy = "allowlist"
shell_allowlist = "git,ls,pwd,echo"
auth_tokens = "sk-123,sk-456"
tavily_api_key = "tvly-..."
```

### Manual Setup (Advanced)

```bash
# 1. Build
cargo build --release

# 2. Start OpenCode daemon
opencode serve --port 4096 --hostname 127.0.0.1

# 3. Start the bridge with custom options
./target/release/opencode2claude --port 4000 --model "openai/gpt-4o"

# 4. Configure Claude Code (in a new terminal)
export ANTHROPIC_API_KEY="opencode-bridge"
export ANTHROPIC_BASE_URL="http://127.0.0.1:4000/v1"
claude
```

### Stopping

```bash
./stop.sh
```

---

## 📁 Project Structure

```
opencode2claude/
├── src/
│   ├── main.rs               # Entry point, Axum router, CLI args, graceful shutdown
│   ├── config.rs              # Config chain: CLI > Env > TOML > Defaults
│   ├── handlers.rs            # Anthropic API request parsing & routing
│   ├── middleware.rs          # Bearer token auth middleware
│   ├── shell.rs               # Shell command execution with security policy
│   ├── sse.rs                 # SSE event builder (Anthropic protocol)
│   ├── state.rs               # AppState: shared config, HTTP client, rate limiter
│   ├── error.rs               # BridgeError → Anthropic JSON error responses
│   └── opencode/              # OpenCode API gateway (module directory)
│       ├── mod.rs             # Module re-exports
│       ├── types.rs           # OpenAI API type definitions
│       ├── search.rs          # Web search providers with enum dispatch
│       ├── mapper.rs          # Anthropic → OpenAI request mapper
│       └── forward.rs         # Sync/stream forwarding + WARP retry
├── tests/
│   ├── common/
│   │   └── mod.rs             # Shared integration test harness
│   └── integration.rs         # 17 integration tests
├── Cargo.toml                 # Rust dependencies
├── start.sh                   # One-command setup: compile → daemon → bridge → env
├── stop.sh                    # Graceful shutdown of all background processes
└── README.md
```

### Tech Stack

| Component | Technology | Why |
| --------- | ---------- | --- |
| Web Framework | [Axum](https://github.com/tokio-rs/axum) 0.7 | Type-safe, ergonomic, fastest Rust web framework |
| Async Runtime | [Tokio](https://tokio.rs/) | Industry-standard async I/O for Rust |
| Serialization | [Serde](https://serde.rs/) | Zero-copy JSON parsing |
| HTTP Client | [Reqwest](https://github.com/seanmonstar/reqwest) | Connection pooling for daemon + search APIs |
| Logging | [Tracing](https://github.com/tokio-rs/tracing) | Structured, async-aware logging |

---

## ⚡ Performance

Since the bridge is a thin translation layer, overhead is minimal:

| Metric | Value |
| ------ | ----- |
| Bridge startup time | **< 5ms** |
| Request routing overhead | **< 1ms** |
| Shell command (`!`) response | **~10ms** |
| Memory footprint | **~3 MB** |
| Binary size | **~5 MB** (static, release build) |

> The bottleneck is always the LLM provider, never the bridge.

---

## 🗺️ Roadmap

- [x] Authentication middleware (Bearer token)
- [x] `/health` status endpoint
- [x] Support for `/v1/models` endpoint
- [x] Configuration file support (TOML)
- [x] Dockerfile for containerized deployment
- [x] Inbound rate limiting
- [x] Configurable search loop limit
- [x] CLI args for search API keys
- [x] Module refactoring (opencode.rs split into 5 modules)
- [x] Test coverage improvements (60 unit + 17 integration tests)
- [ ] Request/response logging & analytics dashboard
- [ ] Multi-model routing (different models for different tasks)

---

## 🤝 Contributing

Contributions are welcome! Whether it's a bug fix, new feature, or documentation improvement.

```bash
# Fork & clone
git clone https://github.com/<your-username>/opencode2claude.git
cd opencode2claude

# Build & test
cargo build
cargo test

# Run in dev mode
RUST_LOG=debug cargo run
```

---

## 📄 License

This project is licensed under the **MIT License** — see the [LICENSE](LICENSE) file for details.

---

<div align="center">

**If this project saved you money or time, consider giving it a ⭐**

Made with 🦀 and ❤️

</div>
