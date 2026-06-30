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

**Claude Code** → `opencode2claude` → **opencode.ai/zen/v1/chat/completions** → **Any LLM**

```
opencode2claude start  # Start the bridge daemon (background)
opencode2claude status # Check if it's running
claude                 # Works with any model
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

The [quick install script](#-quick-install) is the recommended method — it auto-detects your OS and architecture. Verified on clean Linux (no Rust toolchain required).

**Alternatives:**

- **cargo** (if you have Rust): `cargo install opencode2claude`
- **Docker**: `docker pull ghcr.io/nmhuei/opencode2claude:latest`

No dependencies needed. The binary is **~5MB** and starts instantly.

---

## 👀 Usage

```bash
# Start the bridge daemon (background)
opencode2claude start

# Check status
opencode2claude status

# Use a specific model (via env or CLI flag)
export OPENCODE_MODEL="openai/gpt-4o"
opencode2claude start

# Stop the bridge
opencode2claude stop

# Manage proxy pool
opencode2claude proxy status
opencode2claude proxy restart   # Recreate primary proxies (40001-40003 only)
opencode2claude proxy purge     # Remove + recreate primary proxies
opencode2claude proxy logs      # View proxy container logs

# Traditional start.sh (still works for convenience)
source start.sh

# CLI flags (override all config)
opencode2claude serve --port 4000 --model "google/gemini-2.5-pro"
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
| `OPENCODE_MODEL` | (none, pass-through) | Target model (upstream decides when unset) |
| `BRIDGE_SHELL_POLICY` | `disabled` | `disabled` \| `allowlist` \| `unrestricted` |
| `BRIDGE_AUTH_TOKEN` | (none) | Comma-separated Bearer tokens |
| `BRIDGE_RATE_LIMIT` | (none) | Max concurrent requests (unset = unlimited) |
| `BRIDGE_MAX_SEARCH_LOOPS` | `5` | Search interception retries |
| `TAVILY_API_KEY` | (none) | Web search API keys... |
| `BRIDGE_PRIMARY_PROXIES` | (none) | Primary proxy URLs (socks5://...) |
| `BRIDGE_WARM_STANDBY_PROXIES` | (none) | Warm-standby proxy URLs (protected) |

Full list: see [CLAUDE.md](CLAUDE.md) or `opencode2claude --help`

---

## 🔄 Multi-Agent & Proxy Routing (Avoid Rate Limits)

When running multiple concurrent Claude Code agents, they can hit the free-tier rate limits quickly if they share a single IP. 

To prevent this, `opencode2claude` uses a **two-tier proxy pool**:

1. **Primary Managed Pool** (ports 40001–40003) — normal routing targets
2. **Warm-Standby Protected Pool** (ports 40004–40005) — failover only, protected from CLI modification

### How It Works

- The bridge hashes each agent's API key via **Rendezvous hashing** to a deterministic primary proxy
- Each agent always maps to the same primary proxy (sticky assignment)
- If the selected primary is unhealthy/cooldown/dead, traffic **fails over to WarmStandby**
- **Only affected agents remap** — healthy primaries don't lose their agents
- WarmStandby proxies are never used for normal traffic

### Automated Setup with Docker

If Docker is running on your host, `start.sh` will **automatically** spawn isolated Cloudflare WARP proxy containers:

```bash
# Standard: 3 primary + 2 warm-standby proxies on ports 40001–40005
source start.sh
```

To manage proxy containers via CLI:

```bash
opencode2claude proxy status      # List with roles (primary vs protected)
opencode2claude proxy restart      # Recreate primary proxies only
opencode2claude proxy purge        # Remove + recreate primary proxies
```

To clean up all containers and stop the bridge:

```bash
opencode2claude stop
```

### Manual Proxy Pool Configuration

If you prefer to configure your own SOCKS5/HTTP proxies (e.g. Tor or private proxies):
```bash
export BRIDGE_PRIMARY_PROXIES="socks5://127.0.0.1:40001,socks5://127.0.0.1:40002,socks5://127.0.0.1:40003"
export BRIDGE_WARM_STANDBY_PROXIES="socks5://127.0.0.1:40004,socks5://127.0.0.1:40005"
opencode2claude start
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