# CLI Reference

`opencode2claude` provides a subcommand-based CLI for managing the bridge lifecycle and proxy pool.

## Global Flags

```
opencode2claude [COMMAND] [OPTIONS]
```

- `serve` (default): Start the API bridge server
- `start`: Start the bridge as a background daemon (via supervisor)
- `status`: Show bridge status
- `stop`: Stop the bridge daemon
- `restart`: Restart the bridge daemon
- `logs`: View bridge logs
- `env`: Display environment information
- `proxy`: Manage proxy pool containers

---

## `serve` — Start the API Bridge Server

Runs the bridge in the foreground. Use `start` instead for background daemon mode.

```
opencode2claude serve [OPTIONS]
```

| Flag | Env | Description |
|------|-----|-------------|
| `-p, --port` | `BRIDGE_PORT` | Listen port (default: `4000`) |
| `--host` | `BRIDGE_HOST` | Bind address (default: `127.0.0.1`) |
| `-m, --model` | `OPENCODE_MODEL` | Model override |
| `-c, --config` | — | Custom TOML config path |
| `--shell-policy` | `BRIDGE_SHELL_POLICY` | `disabled` \| `allowlist` \| `unrestricted` |
| `--tavily-api-key` | `TAVILY_API_KEY` | Tavily search API key |
| `--exa-api-key` | `EXA_API_KEY` | Exa search API key |
| `--serper-api-key` | `SERPER_API_KEY` | Serper API key |
| `--searxng-url` | `SEARXNG_URL` | SearXNG instance URL |
| `--searxng-api-key` | `SEARXNG_API_KEY` | SearXNG API key |

---

## `start` — Start Background Daemon

```
opencode2claude start [OPTIONS]
```

Starts `serve` as a background child process, writes PID to `.runtime/`.

| Flag | Description |
|------|-------------|
| `-p, --port` | Override bridge port for the daemon |
| `--host` | Override bind address for the daemon |

---

## `status` — Show Bridge Status

```
opencode2claude status [OPTIONS]
```

| Output | Meaning |
|--------|---------|
| `Bridge: Running (PID: 12345, port: 4000)` | Bridge is active |
| `Bridge: Stopped` | Bridge is not running |

---

## `stop` — Stop the Bridge

```
opencode2claude stop [OPTIONS]
```

Sends SIGTERM to the bridge process, waits briefly, then SIGKILL if needed. Cleans up PID file.

---

## `restart` — Restart the Bridge

```
opencode2claude restart
```

Stops the daemon then starts it again.

---

## `logs` — View Bridge Logs

```
opencode2claude logs
```

Displays recent bridge daemon logs from `.runtime/opencode2claude.log`.

---

## `env` — Display Environment

```
opencode2claude env
```

Shows the current bridge configuration derived from all config sources (CLI > Env > TOML > Defaults).

---

## `proxy` — Manage Proxy Pool Containers

### `proxy status` / `proxy ps`

List all proxy containers with their role and health:

```
$ opencode2claude proxy status
Primary managed proxies:
  40001  healthy  opencode-warp-1
  40002  healthy  opencode-warp-2
  40003  healthy  opencode-warp-3

Warm-standby protected proxies:
  40004  healthy  opencode-warp-4  protected
  40005  healthy  opencode-warp-5  protected

Protected warm-standby proxies (40004-40005) are never stopped, restarted,
purged, or recreated by opencode2claude.
```

### `proxy restart`

Recreates primary managed proxy containers (40001–40003):

```
$ opencode2claude proxy restart
Restarting primary managed proxies:
  40001... OK
  40002... OK
  40003... OK

Protected warm-standby proxies skipped: 40004, 40005 (always protected)
```

### `proxy purge`

Removes and recreates all primary proxy containers:

```
$ opencode2claude proxy purge
Purging primary managed proxies:
  40001... removed
  40002... removed
  40003... removed
  40001 recreate... OK
  40002 recreate... OK
  40003 recreate... OK

Protected warm-standby proxies skipped: 40004, 40005 (always protected)
```

### `proxy logs`

Display last 50 log lines from each primary proxy container:

```
$ opencode2claude proxy logs
=== proxy 40001 (opencode-warp-1) ===
...
```

---

## Configuration Priority

```
CLI args  >  Environment variables  >  TOML file  >  Hardcoded defaults
```

All `serve` flags have a corresponding environment variable (see table above).
TOML file defaults to `opencode2claude.toml` in the current directory.
