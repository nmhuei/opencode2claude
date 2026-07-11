# Security model

OpenCode2API handles API credentials, management credentials, upstream content, configurable network destinations, local process state, and optional Docker/WARP lifecycle. The default policy favors loopback binding, disabled shell delegation, bounded input/output, and fail-closed proxy routing.

## Trust boundaries

1. **Client API** — untrusted HTTP input under `/v1/*`.
2. **Management API and dashboard** — privileged status, configuration, and proxy actions.
3. **Upstream provider** — untrusted JSON/SSE and error bodies.
4. **Search providers** — untrusted JSON/HTML and configurable destinations.
5. **Local infrastructure** — PID files, config files, Docker CLI, WARP CLI, and release binary replacement.

## Public binding

Binding to a non-loopback address is rejected unless:

- client API authentication is configured;
- a dashboard token of at least 12 characters is configured;
- shell policy is not unrestricted.

TLS termination is not built into the bridge. Public deployments must place it behind a trusted TLS reverse proxy and retain the bridge authentication requirements.

## Authentication separation

- `BRIDGE_AUTH_TOKEN` protects `/v1/*`.
- `REST_API_TOKEN` protects `/api/v1/*`.
- `DASHBOARD_ADMIN_TOKEN` protects dashboard sessions and acts as REST fallback only when a dedicated REST token is absent.
- Public health endpoints deliberately expose minimal state and no topology.

Token comparison avoids an early length shortcut. Secret wrapper `Debug` and `Display` output is redacted.

## CSRF

Dashboard browser sessions use an HttpOnly authentication cookie and double-submit CSRF protection for mutations. Requests authenticated with an explicit REST bearer or dashboard token header are not browser-cookie requests and are evaluated by the corresponding token policy.

## Shell delegation

The default policy is `disabled`. `allowlist` validates the base command and rejects shell metacharacters such as command separators, substitutions, pipes, redirects, and newlines. `unrestricted` is rejected on non-loopback binding. The bridge returns client-side tool-use instructions; it does not execute arbitrary request content in HTTP handlers.

## Egress confidentiality

Proxy mode is fail closed. If the configured pool is empty or has no eligible node, the request fails rather than using direct host egress. Failed nodes are excluded on retry. Protected standby nodes cannot be changed by restart, stop, purge, or reconciliation.

Strict exit verification can require fresh public identities and a minimum number of unique exits. Duplicate exits are excluded from independent capacity. Identity endpoints are configurable and should be controlled because they receive requests from every proxy.

## SSRF and search

Public provider endpoints must use HTTP or HTTPS and cannot contain embedded credentials. Private or loopback SearXNG targets are rejected unless `allow_private_searxng` is explicitly enabled. Enabling it expands the bridge's network reach and should only be used in a segmented network.

Search response bytes, duration, result count, and snippet length are bounded. HTML is parsed into typed results before model-context formatting.

## Resource limits

The following limits are independently enforced:

- incoming request body;
- concurrent requests;
- SSE channel capacity and send timeout;
- upstream SSE line bytes;
- non-streaming upstream response bytes;
- DSML streaming buffer;
- search response bytes and timeout;
- search iteration count;
- retry and proxy-restart attempts;
- worker and server shutdown deadlines.

Malformed upstream events are ignored or converted to a bounded error sequence. Upstream bodies are not returned verbatim to clients.

## Files and process ownership

Sensitive writes use atomic replacement and owner-only permissions where supported. PID state records executable identity and a process start marker. Stop refuses to signal a process whose identity does not match, preventing PID-reuse termination.

Only infrastructure adapters may execute OS commands. The source boundary checker fails when direct process execution appears elsewhere.

## Install and update

Install and self-update require a companion SHA-256 checksum. Candidate binaries must pass `--version` before installation. Self-update preserves the previous binary, performs atomic replacement, smoke-tests the installed candidate, and rolls back automatically on failure.

GitHub release artifacts include per-file checksums, an aggregate checksum file, SBOM data, and GitHub build provenance. Container releases use registry provenance/SBOM and keyless signing.

## Security gates

Tier B requires:

```bash
shellcheck ...
cargo audit
cargo deny check
python3 scripts/check_secrets.py --self-test
python3 scripts/check_secrets.py
cargo test --test parser_fuzz_smoke
```

The repository secret scanner checks tracked files plus untracked non-ignored files and validates itself against synthetic positive and negative fixtures. `cargo deny` permits MPL-2.0 only for the documented `option-ext` transitive dependency and records unavoidable duplicate dependency trees explicitly.

## Incident response

For suspected credential exposure:

1. rotate client, dashboard, REST, search, and upstream credentials;
2. stop public ingress;
3. collect authenticated diagnostics and redacted logs;
4. verify config and runtime file permissions;
5. inspect release checksum/provenance before reinstalling;
6. run Tier A and Tier B from a clean checkout.
