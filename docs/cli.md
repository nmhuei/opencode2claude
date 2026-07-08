# CLI Reference

`opencode2api` provides a unified, hierarchical subcommand-based CLI (v2) for managing the bridge lifecycle, proxy pools, and system diagnostics.

## Global Flags

Global flags can be placed anywhere in the command line invocation:

*   `--json`: Output in JSON format (machine-readable, hides human-only dashboards/tables/spinners).
*   `--quiet`: Minimal output (displays compact status line summaries or errors only).
*   `--color <auto|always|never>`: Control terminal color styling (default: `auto`).

---

## Command Tree

```text
opencode2api
├── server
│   ├── start [-f] [-p PORT] [--host HOST] [-c CONFIG] [-m MODEL] [--shell-policy POLICY]
│   ├── stop [-p PORT] [--host HOST]
│   ├── status [-p PORT] [--host HOST]
│   ├── restart
│   ├── logs
│   └── config
├── proxy
│   ├── ps (status)
│   ├── restart
│   ├── purge [-y]
│   └── logs
├── env
├── doctor
├── completion <SHELL>
├── update [--check] [--force]
└── init [-o OUTPUT] [-f]
```

---

## 1. `server` Commands

Manage the bridge server process lifecycle.

### `server start`
Start the bridge server process. By default, this spawns the bridge as a detached background daemon and writes its PID to `~/.opencode2api/opencode2api.pid.json`.

*   **Flags:**
    *   `-f, --foreground`: Run in the current terminal foreground (do not daemonize).
    *   `-p, --port <PORT>`: Port for the API bridge (default: `4000`, env: `BRIDGE_PORT`).
    *   `--host <HOST>`: Bind host address (default: `127.0.0.1`, env: `BRIDGE_HOST`).
    *   `-c, --config <PATH>`: Custom TOML configuration path.
    *   `-m, --model <MODEL>`: Upstream model override (e.g., `opencode/deepseek-v4-flash-free`).
    *   `--shell-policy <disabled|allowlist|unrestricted>`: Enable local shell execution from prompts starting with `!`.
    *   `--tavily-api-key <KEY>`: Tavily web search API key override.
    *   `--exa-api-key <KEY>`: Exa web search API key override.
    *   `--serper-api-key <KEY>`: Serper.dev web search API key override.
    *   `--searxng-url <URL>`: SearXNG web search URL override.
    *   `--searxng-api-key <KEY>`: SearXNG web search API key override.

### `server stop`
Stop the running bridge daemon cleanly. Reads the PID file to terminate the exact process and frees the port.

*   **Flags:**
    *   `-p, --port <PORT>`: Target port of the daemon to stop (default: `4000`).
    *   `--host <HOST>`: Target host of the daemon to stop (default: `127.0.0.1`).

### `server status`
Show the detailed runtime dashboard (PID, port, model, auth, uptime, and proxy pool table).

*   **Flags:**
    *   `-p, --port <PORT>`: Target port of the daemon to query (default: `4000`).
    *   `--host <HOST>`: Target host of the daemon to query (default: `127.0.0.1`).
*   **Quiet Output:** Outputs a single compact status string: `running`, `stopped`, or `error`.

### `server restart`
Restart the bridge daemon process.

### `server logs`
View recent bridge daemon logs (tailing `~/.opencode2api/opencode2api.log`).

### `server config`
Display the current resolved configuration schema derived from CLI overrides, environment variables, TOML, and hardcoded defaults.

---

## 2. `proxy` Commands

Manage proxy pool Docker containers for multi-agent Cloudflare WARP egress.

### `proxy ps`
List SOCKS5 proxy containers (role, port, status, name).

*   **Quiet Output:** Outputs a single-line summary: `primary=X/Y standby=A/B`.

### `proxy restart`
Recreate primary managed proxy containers (ports `40001`–`40003`) to rotate egress IPs. Warm-standby proxies are skipped/protected.

### `proxy purge`
Fully remove and recreate all primary proxy containers to wipe Docker caches and enforce fresh container state.

*   **Flags:**
    *   `-y, --yes`: Skip verification prompt.

### `proxy logs`
Tail recent logs from active SOCKS5 proxy containers.

---

## 3. General Utilities

### `env`
Display resolved environment configuration details for Claude Code integrations.

### `doctor`
Diagnose common issues with the bridge and its requirements (Docker daemon, port availability, container health, config formats, security auth state, and upstream API reachability).

*   **Quiet Output:** Outputs aggregate summary results: `warnings=X failures=Y`. Fails with exit code 1 if failures > 0.

### `completion <SHELL>`
Generate shell autocomplete scripts. Supported values: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

### `update`
Self-update the running binary atomically by checking GitHub releases.

*   **Flags:**
    *   `--check`: Inspect if an update is available without downloading.
    *   `--force`: Reinstall even if the local version is already up-to-date.

### `init`
Generate a fully commented default TOML configuration template (`opencode2api.toml`).

*   **Flags:**
    *   `-o, --output <PATH>`: Output path (default: `opencode2api.toml`).
    *   `-f, --force`: Overwrite existing configuration file without prompting.

---

## Backward-Compatible Aliases (Hidden)

The legacy flat commands remain supported for backward-compatibility but will log a deprecation warning:

*   `serve` $\rightarrow$ Alias for `server start -f`
*   `start` $\rightarrow$ Alias for `server start`
*   `status` $\rightarrow$ Alias for `server status`
*   `stop` $\rightarrow$ Alias for `server stop`
*   `restart` $\rightarrow$ Alias for `server restart`
*   `logs` $\rightarrow$ Alias for `server logs`
