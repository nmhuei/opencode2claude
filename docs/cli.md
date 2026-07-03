# CLI Reference

`opencode2claude` provides a subcommand-based CLI for managing the bridge lifecycle and proxy pool.

## Command Tree

```
opencode2claude [--json | --quiet] [--color <MODE>] <COMMAND> [OPTIONS]

Commands:
  server      Manage the bridge server (start, stop, status, etc.)
  proxy       Manage WARP proxy pools
  env         Display environment information
  doctor      Diagnose common issues
  completion  Generate shell completion scripts
  update      Self-update to the latest release
  init        Generate a default config file
```

## Global Flags

| Flag | Description |
|------|-------------|
| `--json` | Output in JSON format (machine-readable) |
| `--quiet` | Minimal output (errors/success only) |
| `--color auto\|always\|never` | Color output (default: auto) |

`--json` and `--quiet` are mutually exclusive.

## `server` — Manage the Bridge Server

### `server start`

Start the bridge server. By default starts as a background daemon.
Use `-f` or `--foreground` to run in the current terminal.

```
opencode2claude server start [OPTIONS]
```

| Flag | Env | Description |
|------|-----|-------------|
| `-f, --foreground` | — | Run in foreground (don't daemonize) |
| `-p, --port` | `BRIDGE_PORT` | Override bridge port |
| `--host` | `BRIDGE_HOST` | Override bind address |
| `-c, --config` | — | Path to custom TOML config file |
| `-m, --model` | `OPENCODE_MODEL` | Override model |
| `--shell-policy` | `BRIDGE_SHELL_POLICY` | `disabled` \| `allowlist` \| `unrestricted` |
| `--tavily-api-key` | `TAVILY_API_KEY` | Tavily search API key override |
| `--exa-api-key` | `EXA_API_KEY` | Exa search API key override |
| `--serper-api-key` | `SERPER_API_KEY` | Serper search API key override |
| `--searxng-url` | `SEARXNG_URL` | SearXNG instance URL override |
| `--searxng-api-key` | `SEARXNG_API_KEY` | SearXNG API key override |
| `--tls-cert` | — | Path to TLS certificate (PEM, requires `--tls-key`) |
| `--tls-key` | — | Path to TLS private key (PEM, requires `--tls-cert`) |

### `server stop`

Stop the bridge daemon. Sends SIGTERM, waits briefly, then SIGKILL if needed.
Cleans up PID file.

```
opencode2claude server stop [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-p, --port` | Override bridge port for the daemon |
| `--host` | Override bind address for the daemon |

### `server status`

Show bridge status (running or stopped).

```
opencode2claude server status [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-p, --port` | Override bridge port for the daemon |
| `--host` | Override bind address for the daemon |

**Output modes:**

- Human (default): Dashboard with uptime, PID, model, auth, proxy pool
- `--json`: Structured JSON (`status`, `pid`, `uptime`, `message`)
- `--quiet`: Compact one-line output (`running pid=12345 port=4000` or `stopped`)

### `server restart`

Restart the bridge daemon (stop then start).

```
opencode2claude server restart
```

### `server logs`

View bridge daemon logs (last 100 lines).

```
opencode2claude server logs
```

- Human (default): Colorized log output
- `--json`: Structured JSON with line numbers
- `--quiet`: Raw log lines without color

### `server config`

Show current server configuration.

```
opencode2claude server config
```

- Human (default): Table of config keys and values
- `--json`: Structured JSON

---

## `proxy` — Manage Proxy Pool

### `proxy ps`

List proxy pool status with role and health.

```
opencode2claude proxy ps
```

- Human (default): Table view
- `--json`: Structured JSON per container
- `--quiet`: Compact one-line summary (`primary=3/3 standby=2/2`)

### `proxy restart`

Recreate primary managed proxy containers (40001–40003).

```
opencode2claude proxy restart
```

Warm-standby proxies (40004, 40005) are never modified.

### `proxy purge`

Remove and recreate all primary proxy containers.

```
opencode2claude proxy purge [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-y, --yes` | Skip confirmation prompt |

### `proxy logs`

View proxy container logs (last 50 lines).

```
opencode2claude proxy logs
```

---

## `env` — Display Environment Information

Shows environment variables needed by Claude Code.

```
opencode2claude env
```

- Human (default): Formatted env output
- `--json`: Structured JSON

---

## `doctor` — Diagnose Common Issues

Runs diagnostics on the bridge and its dependencies.

```
opencode2claude doctor
```

Exit code: `0` if no failures, `1` if failures detected.

- Human (default): Colored report
- `--json`: Structured JSON with summary
- `--quiet`: Compact summary (`warnings=2 failures=0`)

---

## `completion` — Generate Shell Completion Scripts

Generate shell completion scripts for bash, zsh, fish, powershell, or elvish.

```
opencode2claude completion <SHELL>
```

Example:
```bash
opencode2claude completion bash > /etc/bash_completion.d/opencode2claude
```

---

## `update` — Self-Update

Check for and apply the latest release.

```
opencode2claude update [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--check` | Check for updates without applying |
| `--force` | Force reinstall even if up-to-date |

---

## `init` — Generate Config File

Generate a default `opencode2claude.toml` config file.

```
opencode2claude init [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-o, --output` | Output path (default: `./opencode2claude.toml`) |
| `-f, --force` | Overwrite existing file without prompting |

---

## Output Modes

Every command supports three output modes via global flags:

### Human (default)

Readable, colored output with tables and spinners when appropriate.

### JSON (`--json`)

Machine-parseable JSON only — no ANSI, no spinners, no decorative output.

Error responses follow a consistent schema:
```json
{
  "ok": false,
  "error": {
    "code": "already_running",
    "message": "Bridge is already running (PID: 12345)",
    "hint": "Run `opencode2claude server stop` first."
  }
}
```

### Quiet (`--quiet`)

Compact, script-friendly one-line output:

| Command | Running | Stopped |
|---------|---------|---------|
| `server status` | `running pid=12345 port=4000` | `stopped` |
| `server stop` | — | `stopped` |
| `server restart` | `running pid=12345 port=4000` | `stopped` |
| `proxy ps` | `primary=3/3 standby=2/2` | — |
| `doctor` | `warnings=2 failures=0` | — |

---

## Backward-Compatible Aliases

The following legacy flat commands remain available for backward compatibility
but are hidden from `--help`:

| Legacy | Equivalent v2 |
|--------|---------------|
| `serve [OPTIONS]` | `server start -f [OPTIONS]` |
| `start [OPTIONS]` | `server start [OPTIONS]` |
| `status [OPTIONS]` | `server status [OPTIONS]` |
| `stop [OPTIONS]` | `server stop [OPTIONS]` |
| `restart` | `server restart` |
| `logs` | `server logs` |
| `proxy status` | `proxy ps` |

Legacy commands emit a deprecation hint and redirect to the equivalent v2 command.
They may still appear in generated shell completions for compatibility.

---

## Configuration Priority

```
CLI args  >  Environment variables  >  TOML file  >  Hardcoded defaults
```

All `server start` flags have a corresponding environment variable (see above).
TOML file defaults to `opencode2claude.toml` in the current directory.
