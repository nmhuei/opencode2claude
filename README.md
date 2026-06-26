<div align="center">

# 🌉 OpenCode2Claude

### Use Claude Code with any LLM — for free.

[![Crates.io](https://img.shields.io/crates/v/opencode2claude?style=flat-square&logo=rust)](https://crates.io/crates/opencode2claude)
[![CI](https://img.shields.io/github/actions/workflow/status/nmhuei/opencode2claude/ci.yml?style=flat-square&branch=main)](https://github.com/nmhuei/opencode2claude/actions)
[![Docker](https://img.shields.io/badge/docker-ghcr.io-blue?style=flat-square&logo=docker)](https://github.com/nmhuei/opencode2claude/pkgs/container/opencode2claude)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)

**Claude Code** → `opencode2claude` → **OpenCode CLI** → **Any LLM**

```
source start.sh   # 5 seconds → ready to use
claude            # works with any model
```

[Install](#-install) • [Usage](#-usage) • [Configuration](#-configuration) • [Benchmark](#-benchmark)

</div>

---

## 💡 Why?

Claude Code is locked to Anthropic's API. This bridge routes it through [OpenCode](https://github.com/opencode-ai/opencode) to access **50+ models** — including free tiers like `deepseek-v4-flash-free`.

| Before | After |
|--------|-------|
| Claude Code → Anthropic only | Claude Code → Any LLM |
| 💸 Pay per token | 🆓 Free models available |

---

## 📦 Install

```bash
# One-liner (recommended)
git clone https://github.com/nmhuei/opencode2claude.git && cd opencode2claude
source start.sh

# Or via cargo
cargo install opencode2claude

# Or via Docker
docker pull ghcr.io/nmhuei/opencode2claude:latest
```

Then just run `claude` in the same terminal.

---

## 👀 Usage

```bash
# Use a specific model
export OPENCODE_MODEL="openai/gpt-4o"
source start.sh

# Enable auth
export BRIDGE_AUTH_TOKEN="sk-your-token"
source start.sh

# Rate limiting
export BRIDGE_RATE_LIMIT=10
source start.sh

# CLI flags (override all config)
./target/release/opencode2claude --port 4000 --model "google/gemini-2.5-pro"
```

Shell commands bypass the LLM entirely — prefix with `!`:
```
You: !git status          # → instant local exec (0.01s)
You: !docker ps           # → direct terminal output
You: What is recursion?   # → routed through LLM as normal
```

---

## ⚙️ Configuration

Priority: **CLI args > Env vars > TOML file > Defaults**

| Variable | Default | Description |
|----------|---------|-------------|
| `BRIDGE_PORT` | `4000` | Bridge listen port |
| `BRIDGE_HOST` | `127.0.0.1` | Bind address |
| `OPENCODE_MODEL` | `deepseek-v4-flash-free` | Target model |
| `BRIDGE_SHELL_POLICY` | `unrestricted` | `disabled` \| `allowlist` \| `unrestricted` |
| `BRIDGE_AUTH_TOKEN` | (none) | Comma-separated Bearer tokens |
| `BRIDGE_RATE_LIMIT` | (none) | Max concurrent requests (unset = unlimited) |
| `BRIDGE_MAX_SEARCH_LOOPS` | `5` | Search interception retries |
| `TAVILY_API_KEY` | (none) | Web search API keys... |

Full list: see [CLAUDE.md](CLAUDE.md) or `opencode2claude --help`

---

## 📊 Benchmark

| Metric | Value |
|--------|-------|
| Bridge startup | **<5ms** |
| Request routing | **<1ms** |
| Shell command (`!`) | **~10ms** |
| Memory | **~3MB** |
| Binary | **~5MB** (static) |

> The bottleneck is always the LLM provider, never the bridge.

---

## 🔧 Tech Stack

**Rust** + **Axum** + **Tokio** + **Reqwest** — ~4k LOC, single binary, zero runtime deps.

## 📄 License

MIT — [LICENSE](LICENSE)
