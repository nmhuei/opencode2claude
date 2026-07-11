# OpenCode2API

OpenCode2API is a local Rust bridge that accepts a subset of the Anthropic Messages API and forwards requests to an OpenAI-compatible upstream. It supports synchronous and streaming responses, reasoning blocks, native tool calls, DSML tool calls, bounded web-search interception, model fallback, and an optional managed WARP/SOCKS egress pool.

The implementation is designed to fail closed: public binding requires strong authentication, proxy mode does not silently fall back to the host network, protected warm-standby nodes cannot be modified by normal lifecycle commands, and downloaded updates are checksum-verified before replacement.

## Supported platforms

Release targets are Linux x86_64, Linux ARM64, macOS x86_64, and macOS ARM64. Runtime lifecycle tests run on Linux and macOS CI. Windows is not currently advertised as a supported release target.

## Installation

Using Cargo:

```bash
cargo install opencode2api
```

Using a release binary:

```bash
curl -fsSL https://raw.githubusercontent.com/nmhuei/opencode2api/main/install.sh | sh
```

The install script downloads the binary and its companion `.sha256` file, verifies the checksum, runs a `--version` smoke check, and only then installs it.

## Quick start

The default bind address is `127.0.0.1:4000`. The default proxy topology expects three primary SOCKS proxies on ports `40001-40003` and two protected warm-standby proxies on `40004-40005`.

Start with managed proxy egress:

```bash
opencode2api server start
opencode2api server status
```

Start without Docker/WARP proxy management and use direct host egress:

```bash
opencode2api server start --no-proxy
```

Generate a documented configuration file:

```bash
opencode2api init --output opencode2api.toml
opencode2api server start --config opencode2api.toml
```

Configure Claude Code or another Anthropic-compatible client using the values emitted by:

```bash
opencode2api env
```

## API surface

Anthropic-compatible routes:

| Method | Route | Contract |
|---|---|---|
| `POST` | `/v1/messages` | Sync or SSE Messages response |
| `POST` | `/v1/messages/count_tokens` | Explicit token estimate |
| `GET` | `/v1/models` | Configured/default model metadata |

Public health routes:

| Method | Route | Contract |
|---|---|---|
| `GET` | `/health` | Minimal compatibility health |
| `GET` | `/health/live` | Process/event-loop liveness |
| `GET` | `/health/ready` | Worker and permitted-egress readiness |

Versioned management routes require `Authorization: Bearer <REST_API_TOKEN>` or the configured dashboard token fallback:

| Method | Route | Contract |
|---|---|---|
| `GET` | `/api/v1/status` | Typed bridge status |
| `GET` | `/api/v1/proxies` | Redacted egress-node state |
| `GET` | `/api/v1/config` | Safe resolved configuration |
| `POST` | `/api/v1/config/preview` | Validate and preview config changes |
| `POST` | `/api/v1/config/apply` | Atomically apply with rollback verification |
| `POST` | `/api/v1/proxies/:port/restart` | Restart a managed primary only |
| `GET` | `/api/v1/metrics` | Authenticated operational counters |
| `GET` | `/api/v1/audit` | Recent bounded secret-safe management audit events |
| `GET` | `/api/v1/openapi.json` | OpenAPI 3.1 management contract |

The browser dashboard is served at `/dashboard`. Cookie-authenticated mutations use a double-submit CSRF token.

## CLI

```text
opencode2api server start|stop|status|restart|logs|config
opencode2api proxy ps|restart|purge|logs
opencode2api dashboard start|status
opencode2api env
opencode2api doctor
opencode2api completion <shell>
opencode2api update [--check] [--force]
opencode2api init [--output PATH] [--force]
```

Machine-readable output is available through `--json`; reduced output is available through `--quiet`. Destructive proxy operations support `--dry-run`, and purge requires explicit confirmation.

Shell-prefixed prompts are **delegated** as client `tool_use` output according to the configured shell policy. The HTTP bridge does not execute arbitrary shell commands inside request handlers. Shell delegation is disabled by default.

## Egress model

The default pool contains:

- three managed primary nodes;
- two protected warm-standby nodes;
- stable rendezvous routing for sticky assignment;
- retry exclusion and circuit-breaker state;
- active-request leases that block destructive lifecycle actions;
- optional multi-endpoint exit-identity verification;
- duplicate public-exit suppression;
- fail-closed behavior when proxy mode has no eligible route.

Warm-standby containers are never restarted, stopped, purged, or recreated by normal management operations.

## Web search

Search interception uses this fallback order:

```text
Tavily → Exa → Serper → SearXNG → DuckDuckGo HTML
```

Providers return typed results, and a central formatter bounds result count, snippet length, response bytes, and request duration. Configurable private SearXNG endpoints require an explicit opt-in because they expand SSRF reach.

## Security defaults

- Bind address: loopback only.
- Client API auth: optional on loopback, mandatory for non-loopback binding.
- Management API: fails closed when no management token is configured.
- Shell policy: disabled.
- Proxy mode: no direct fallback.
- Request, sync-response, search-response, and SSE-line sizes: bounded.
- Config/runtime/PID writes: atomic and permission constrained.
- Update/install: SHA-256 verification and executable smoke test.
- Secrets: redacted from safe config, diagnostics, metrics, and public health.

## Verification

Per-commit deterministic checks:

```bash
scripts/tier-a.sh
```

Protected release/security checks:

```bash
scripts/tier-b.sh
```

Real Docker/WARP and bounded soak checks:

```bash
scripts/tier-c.sh
```

Tier C requires local WARP SOCKS proxies on `127.0.0.1:40001-40003` and Internet access.

## Documentation

- [Configuration and precedence](docs/configuration.md)
- [CLI and exit codes](docs/cli.md)
- [Anthropic compatibility](docs/compatibility.md)
- [Management API and OpenAPI](docs/management-api.md)
- [Proxy/WARP architecture](docs/proxy-pool.md)
- [Security model](docs/security.md)
- [Production deployment](docs/deployment.md)
- [Health, readiness, and metrics](docs/observability.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Upgrade and rollback](docs/upgrade-rollback.md)
- [Contributor test tiers](docs/testing.md)
- [Release checklist](docs/release-checklist.md)

## License

MIT. See [LICENSE](LICENSE).
