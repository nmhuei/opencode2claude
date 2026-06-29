# Migration from `start.sh`

## Background

`start.sh` was the original workflow for starting the bridge. It compiled the Rust binary,
started the OpenCode daemon, spun up Docker WARP proxy containers, and launched the bridge,
all in one script. While functional, this approach had several drawbacks:

- **Process tracking** relied on a `.bridge.pids` file with no health monitoring
- **Docker proxy management** was tightly coupled to the startup script
- **No supervision** — crashed processes stayed down
- **Stale cleanup** was unreliable on partial failures

## Current Workflow

The `opencode2claude` binary now includes a built-in supervisor:

### Quick Start

```bash
# Start the bridge (background daemon)
opencode2claude start

# Check status
opencode2claude status

# View logs
opencode2claude logs

# Stop the bridge
opencode2claude stop

# Manage proxy pool
opencode2claude proxy status
```

### Proxy Container Management

Proxy containers (Docker WARP SOCKS5 proxies) are **not** managed by `start.sh`.
Use the `proxy` subcommand instead:

```bash
opencode2claude proxy status    # List containers with roles
opencode2claude proxy restart   # Recreate primary proxies (40001–40003)
opencode2claude proxy purge     # Remove + recreate primary proxies
opencode2claude proxy logs      # View proxy container logs
```

### Warm-Standby Protection

Ports 40004–40005 are **Warm-Standby Protected Proxies**. They are:

- Never stopped, restarted, purged, or recreated by `opencode2claude proxy`
- Routed normal traffic only when the selected primary is unhealthy/cooldown/dead
- Still shown in `proxy status` output with a `protected` label

## `start.sh` Compatibility

Existing `start.sh` still works for users who prefer the all-in-one script approach:

```bash
source start.sh
```

It will:
1. Compile the binary if missing
2. Start the OpenCode daemon if not running
3. Spin up Docker WARP proxy containers on ports 40001–40005
4. Launch the bridge
5. Export environment variables (`ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`)

However, `start.sh` is now **legacy** — all new development focuses on the supervisor CLI.

## Migration Steps

### 1. Install the Binary

```bash
curl -fsSL https://raw.githubusercontent.com/nmhuei/opencode2claude/main/install.sh | sh
```

Or via Cargo:

```bash
cargo install opencode2claude
```

### 2. Verify

```bash
opencode2claude --help
opencode2claude status
```

### 3. Start Using CLI Commands

Replace `source start.sh` with:

```bash
opencode2claude start
export ANTHROPIC_API_KEY="opencode-bridge"
export ANTHROPIC_BASE_URL="http://127.0.0.1:4000/v1"
export OPENCODE_MODEL="opencode/deepseek-v4-flash-free"
```

Or use the `.env` / TOML config file for persistent settings.

### 4. Migrate Proxy Workflow

| Before (`start.sh`) | After (`opencode2claude proxy …`) |
|---------------------|-----------------------------------|
| Implicit Docker setup | Explicit `proxy status` / `proxy restart` |
| No pool visibility | `proxy status` shows roles + health |
| All containers equal | Primary (40001–40003) vs WarmStandby (40004–40005) distinction |
| Hard-coded port ranges | Dynamic but verified |

## What Changed

| Aspect | `start.sh` | `opencode2claude` CLI |
|--------|------------|----------------------|
| Process model | Background with `.bridge.pids` | Supervisor with PID in `.runtime/` |
| Restart on crash | Manual | Re-spawn on `start` / `restart` |
| Proxy management | Side effect of `start.sh` | Dedicated `proxy` subcommand |
| Status | No CLI status | `opencode2claude status` + `proxy status` |
| WarmStandby protection | None | Enforced by CLI code (`is_protected_proxy_port`) |

## Deprecation

- **Port 40010** — Removed. Do not use.
- **`start.sh`** — Still functional but considered legacy. New features target the supervisor CLI.
