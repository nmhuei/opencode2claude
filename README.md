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
[Configuration](#%EF%B8%8F-configuration) •
[Contributing](#-contributing)

</div>

---

## ❓ Why OpenCode2Claude?

**Claude Code** is an incredible AI coding agent — but it's locked to Anthropic's API and pricing. **OpenCode** supports dozens of LLM providers (including free tiers). This bridge connects them seamlessly.

|        Without this bridge        |             With this bridge             |
| :-------------------------------: | :--------------------------------------: |
| Claude Code → Anthropic API only | Claude Code →**Any LLM provider** |
|         💸 Pay per token         |         🆓 Free models available         |
|              1 model              |              🌐 50+ models              |

### Key Benefits

- 🆓 **Use free models** — Route Claude Code through free-tier models like `deepseek-v4-flash-free`
- ⚡ **Near-zero latency** — Rust + Axum + Tokio delivers hardware-level I/O performance
- 🔌 **Drop-in replacement** — Just set 2 environment variables, no code changes needed
- 📡 **Real-time streaming** — Full SSE (Server-Sent Events) support matching the Anthropic protocol
- 🖥️ **Shell passthrough** — Execute local commands instantly with `!` prefix (0.01s response)
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

| Incoming (Anthropic API)             | Outgoing (OpenCode CLI)            |
| ------------------------------------ | ---------------------------------- |
| `POST /v1/messages`                | `opencode run --attach <daemon>` |
| SSE streaming events                 | Realtime stdout/stderr streaming   |
| `{"role": "user", "content": ...}` | Extracted prompt text              |

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

---

## 🛠️ Configuration

All configuration is done through environment variables:

| Variable           | Default                             | Description                    |
| ------------------ | ----------------------------------- | ------------------------------ |
| `BRIDGE_PORT`    | `4000`                            | Port the API bridge listens on |
| `OPENCODE_PORT`  | `4096`                            | Port of the OpenCode daemon    |
| `OPENCODE_MODEL` | `opencode/deepseek-v4-flash-free` | Target LLM model identifier    |

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

### Manual Setup (Advanced)

If you prefer to start each component individually:

```bash
# 1. Build
cargo build --release

# 2. Start OpenCode daemon
opencode serve --port 4096 --hostname 127.0.0.1

# 3. Start the bridge
./target/release/opencode2claude

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
│   └── main.rs          # Core bridge — Axum router, SSE streaming, process management
├── Cargo.toml           # Rust dependencies (axum, tokio, serde, reqwest, tracing)
├── start.sh             # One-command setup: compile → daemon → bridge → env export
├── stop.sh              # Graceful shutdown of all background processes
├── .gitignore
└── README.md
```

### Tech Stack

| Component     | Technology                                     | Why                                              |
| ------------- | ---------------------------------------------- | ------------------------------------------------ |
| Web Framework | [Axum](https://github.com/tokio-rs/axum) 0.7      | Type-safe, ergonomic, fastest Rust web framework |
| Async Runtime | [Tokio](https://tokio.rs/)                        | Industry-standard async I/O for Rust             |
| Serialization | [Serde](https://serde.rs/)                        | Zero-copy JSON parsing                           |
| HTTP Client   | [Reqwest](https://github.com/seanmonstar/reqwest) | Daemon health checks                             |
| Logging       | [Tracing](https://github.com/tokio-rs/tracing)    | Structured, async-aware logging                  |

---

## ⚡ Performance

Since the bridge is a thin translation layer, overhead is minimal:

| Metric                         | Value                                   |
| ------------------------------ | --------------------------------------- |
| Bridge startup time            | **< 5ms**                         |
| Request routing overhead       | **< 1ms**                         |
| Shell command (`!`) response | **~10ms**                         |
| Memory footprint               | **~3 MB**                         |
| Binary size                    | **~5 MB** (static, release build) |

> The bottleneck is always the LLM provider, never the bridge.

---

## 🗺️ Roadmap

- [x] Authentication middleware (Bearer token)
- [x] `/health` status endpoint
- [x] Support for `/v1/models` endpoint
- [x] Configuration file support (TOML)
- [x] Dockerfile for containerized deployment
- [ ] Rate limiting
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
