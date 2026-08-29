# CLI reference

OpenCode2API uses hierarchical commands and supports `--json`, `--quiet`, and `--color auto|always|never` as global output options.

## Command tree

```text
opencode2api
├── server
│   ├── start
│   ├── stop
│   ├── status
│   ├── restart
│   ├── logs
│   └── config
├── proxy
│   ├── ps
│   ├── restart
│   ├── purge
│   └── logs
├── dashboard
│   ├── start
│   └── status
├── env
├── api-key
│   └── generate
├── doctor
├── completion <shell>
├── update
└── init
```

Use `opencode2api <command> --help` as the executable source of truth.

## Global output

- `--json` returns machine-readable output for commands that expose a schema.
- `--quiet` suppresses presentation detail and retains compact success/error output.
- `--color auto|always|never` controls ANSI output.

## Server lifecycle

### Start

```bash
opencode2api server start [OPTIONS]
```

Important options:

- `-f, --foreground` — stay attached to the current terminal;
- `-p, --port PORT` and `--host HOST` — override bind address;
- `-c, --config PATH` — select TOML file;
- `-m, --model MODEL` — override model;
- `--shell-policy disabled|allowlist|unrestricted` — override delegation policy;
- provider credential/URL overrides for Tavily, Exa, Serper, and SearXNG;
- `--no-proxy` — resolve to direct mode and do not touch Docker proxy containers;
- `--max-body-size BYTES` — override incoming request limit, with `0` meaning unlimited.

Background start writes process identity metadata to the runtime directory. Duplicate start is idempotent when the existing managed process is healthy.

### Stop

```bash
opencode2api server stop [--port PORT] [--host HOST] [--purge]
```

Stop signals only a process whose executable and start marker match the PID record. `--purge` additionally removes managed primary proxy resources; protected standbys remain untouched. Direct-mode stop never invokes Docker.

### Status, restart, logs, config

```bash
opencode2api server status
opencode2api server restart
opencode2api server logs
opencode2api server config
```

Status distinguishes managed running, unmanaged running, and stopped states. Config output is safe/redacted. Restart performs bounded stop/start. Logs read the managed daemon log path.

## Proxy commands

```bash
opencode2api proxy ps
opencode2api proxy restart [--dry-run]
opencode2api proxy purge [--yes] [--dry-run]
opencode2api proxy logs
```

Restart and purge target managed primary nodes only. Protected warm standbys and actively leased nodes are rejected before container-runtime access. Use `--dry-run` in automation to inspect the exact action plan.

## Dashboard

```bash
opencode2api dashboard start
opencode2api dashboard status
```

These commands report or open the dashboard URL; they do not create a second HTTP service.

## Environment and diagnostics

```bash
opencode2api env
opencode2api doctor
```

`env` emits the Claude Code integration variables derived from resolved configuration. `doctor` evaluates configuration, port, runtime, Docker/proxy requirements, and other dependencies appropriate to the selected egress mode. Docker is not treated as required in direct mode.


## API keys

```bash
opencode2api api-key generate
opencode2api api-key generate --count 3 --quiet
opencode2api api-key generate --save --config opencode2api.toml
opencode2api api-key generate --save --replace --config opencode2api.toml
```

Keys use cryptographically secure random bytes and default to a 256-bit value prefixed with `sk-oc2-`. `--save` atomically appends to `auth_tokens`, preserves existing TOML comments, writes the config with owner-only mode `0600`, and reports that a bridge restart is required. `--replace` discards existing bridge client keys.

## Completion

```bash
opencode2api completion bash
opencode2api completion zsh
opencode2api completion fish
opencode2api completion powershell
opencode2api completion elvish
```

## Initialize configuration

```bash
opencode2api init --output opencode2api.toml
opencode2api init --output opencode2api.toml --force
```

Without `--force`, an existing file is not overwritten.

## Update

```bash
opencode2api update --check
opencode2api update
opencode2api update --force
```

Update requires a companion SHA-256 checksum, smoke-tests the candidate, replaces atomically, and rolls back when post-install smoke fails.

## Legacy aliases

Hidden compatibility aliases remain for `serve`, `start`, `status`, `stop`, `restart`, and `logs`. New scripts should use the hierarchical command form.

## Exit codes

| Code | Meaning |
|---:|---|
| `0` | Command completed successfully; managed `server status` reports a running tracked instance. |
| `1` | Operational/validation failure, a negative diagnostic result, or `server status` reports stopped. |
| `2` | Invalid CLI syntax or missing required argument as reported by Clap. |
| `3` | `server status` found a responding but unmanaged listener. |
| `4` | A lifecycle mutation was refused because the listener is unmanaged and was not explicitly adopted. |

`doctor` returns nonzero when its report contains failures. `server stop` remains idempotent and returns `0` when nothing is running. Machine-readable output does not change exit-code semantics.

## Automation examples

```bash
opencode2api --json server status | jq
opencode2api --json proxy restart --dry-run | jq
opencode2api --json doctor > doctor.json
opencode2api completion bash > ~/.local/share/bash-completion/completions/opencode2api
```

The deterministic command contract is exercised by `tests/cli_e2e.sh`.
