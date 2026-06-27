<div align="center">

# 🌉 OpenCode2Claude

### Use Claude Code with any LLM — for free.

[![Crates.io](https://img.shields.io/crates/v/opencode2claude?style=flat-square&logo=rust)](https://crates.io/crates/opencode2claude)
[![CI](https://img.shields.io/github/actions/workflow/status/nmhuei/opencode2claude/ci.yml?style=flat-square&branch=main)](https://github.com/nmhuei/opencode2claude/actions)
[![Docker](https://img.shields.io/badge/docker-ghcr.io-blue?style=flat-square&logo=docker)](https://github.com/nmhuei/opencode2claude/pkgs/container/opencode2claude)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/nmhuei/opencode2claude?style=flat-square&logo=github)](https://github.com/nmhuei/opencode2claude/releases)

### ⚡ Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/nmhuei/opencode2claude/main/install.sh | sh
```

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

The [quick install script](#-quick-install) is the recommended method — it auto-detects your OS and architecture.

**Alternatives:**

- **cargo** (if you have Rust): `cargo install opencode2claude`
- **Docker**: `docker pull ghcr.io/nmhuei/opencode2claude:latest`

No dependencies needed. The binary is **~5MB** and starts instantly.

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
| `BRIDGE_PROXIES` | (none) | Comma-separated list of SOCKS5/HTTP proxies for multi-agent independent IP mapping |

Full list: see [CLAUDE.md](CLAUDE.md) or `opencode2claude --help`

---

## 🔄 Multi-Agent & Proxy Routing (Avoid Rate Limits)

When running multiple concurrent Claude Code agents, they can hit the free-tier rate limits quickly if they share a single IP. 

To prevent this, `opencode2claude` supports a pool of SOCKS5 or HTTP proxies. 

1. **Independent IP Mapping**: The bridge automatically hashes each agent's API key (provided by Claude Code) to route it through a specific proxy in the pool. This ensures that different agents use different proxies/IPs.
2. **Automatic Cooldown & Failover**: If a proxy hits a `429 Too Many Requests` or network error, the bridge automatically marks it as rate-limited, puts it on cooldown, and routes the request through the next available proxy in the pool.

To configure a proxy pool:
```bash
export BRIDGE_PROXIES="socks5://127.0.0.1:40001,socks5://127.0.0.1:40002,socks5://127.0.0.1:40003"
source start.sh
```

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