# Security Policy

## Supported Versions

The following table describes which versions of opencode2api receive active security support.

| Version | Supported          |
|---------|--------------------|
| 0.4.x   | :white_check_mark: |
| < 0.4   | :x:                |

Only the latest minor release line receives security patches. Users are strongly encouraged to update to the latest version.

## Reporting a Vulnerability

If you discover a security vulnerability in opencode2api, please report it privately by sending a **direct message to the repository owner via GitHub**.

**Do not** file a public GitHub issue, discuss the vulnerability in public forums, or disclose it on social media before it has been triaged and resolved.

Your report should include:

- A clear description of the vulnerability
- Steps to reproduce (proof-of-concept preferred)
- The impact of the vulnerability (what an attacker could achieve)
- Any suggested remediation, if known

You will receive an acknowledgment of your report within 48 hours.

## Security Response Process

1. **Acknowledgment** — The maintainer acknowledges receipt within 48 hours and begins triage.
2. **Triage** — The report is assessed for severity and impact. A CVE may be assigned if applicable.
3. **Fix Development** — A fix is developed and reviewed:
   - **Critical** severity: patch released within 7 days.
   - **High** severity: patch released within 14 days.
   - **Medium/Low** severity: patch included in the next regular release.
4. **Release** — A patched version is published to GitHub Releases and crates.io.
5. **Disclosure** — A coordinated public disclosure is made after the fix is available.

## Disclosure Policy

We follow a **coordinated disclosure** process:

- Vulnerabilities are disclosed privately to the maintainer first.
- A fix is developed and released before any public announcement.
- The reporter is credited (with consent) in the release notes and advisory.
- Public disclosure occurs after users have had a reasonable window to update (typically 7-30 days, depending on severity).

## Security Features

### API Authentication (`BRIDGE_AUTH_TOKEN`)

Bearer token authentication is enforced on all API endpoints (`/v1/messages`, `/v1/models`, dashboard API routes) except `/health`. When `BRIDGE_AUTH_TOKEN` is configured, every request must include an `Authorization: Bearer <token>` header with a valid token.

- Multiple tokens can be supplied as a comma-separated list.
- If unset, authentication is disabled (local-only use is the default, see Configuration section).
- Invalid tokens receive a structured JSON `401 Unauthorized` error response.
- Logging of auth failures includes the request path but **never** includes the token value.

### Dashboard Admin Token (`DASHBOARD_ADMIN_TOKEN`)

The web dashboard is protected by a separate admin token. This token:

- Is read from the `DASHBOARD_ADMIN_TOKEN` environment variable.
- Must be at least 8 characters long when binding to a non-loopback address.
- Is stored in an **HttpOnly session cookie** after login (the cookie's value is the token itself, not an indirection — treat this as a bearer-equivalent secret).
- When unset, the dashboard enters **fail-closed mode** — all admin routes are inaccessible.

### Shell Execution Policy

The `!` shell command feature is governed by a three-tier policy:

| Policy | Behavior |
|--------|----------|
| `disabled` (default) | All shell commands rejected. |
| `allowlist` | Only commands whose base name is in the configured allowlist are permitted. Shell metacharacters (`;`, `&`, `\|`, `` ` ``, `$`, `>`, `<`, `\n`) are rejected even if the base command is allowed, preventing chaining and injection. |
| `unrestricted` | All shell commands allowed. Cannot be combined with public (non-loopback) binding — the server will refuse to start. |

The default shell policy is `disabled` as a safety measure.

### Security Hardened Defaults

- **Bind address**: defaults to `127.0.0.1` (localhost-only). A warning is emitted when binding to `0.0.0.0`.
- **Configuration validation** (`validate_security()`): before the HTTP server starts, the configuration is validated against these rules:
  - Non-loopback bind requires `DASHBOARD_ADMIN_TOKEN` to be set (minimum 8 characters).
  - Non-loopback bind requires `BRIDGE_AUTH_TOKEN` to be configured.
  - Non-loopback bind with unrestricted shell policy is unconditionally rejected.
- **Unknown shell policy values** default to `disabled` — never to `unrestricted`.

### Dashboard HTTP Security Headers

The web dashboard sets the following security headers on all responses:

| Header | Value |
|--------|-------|
| `Content-Security-Policy` | `default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline' ...; img-src 'self' data: https://raw.githubusercontent.com; connect-src 'self' ws: wss:; frame-ancestors 'none'` |
| `X-Frame-Options` | `DENY` |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `no-referrer` |

### Rate Limiting

Optional concurrency limiting via `BRIDGE_RATE_LIMIT` environment variable. When configured, a `tokio::Semaphore` limits the number of concurrent requests being processed. This provides a basic defense against resource exhaustion.

### Request Body Limits

The server enforces a maximum request body size (default 1 MB, configurable via `BRIDGE_MAX_BODY_SIZE`). Requests exceeding the limit are rejected with a `413 Payload Too Large` response.

### Proxy Pool Resilience

The 2-tier proxy pool provides transport-layer redundancy with automatic failure detection and recovery:

- **Primary pool** (ports 40001-40003): normal traffic, restartable via CLI.
- **Warm-standby pool** (ports 40004-40005): failover only, never restarted by the CLI.
- After `FAILURE_THRESHOLD` (2) consecutive transport failures, a proxy enters cooldown.
- After `RECOVERY_SUCCESS_COUNT` (2) consecutive successes, a proxy auto-recovers.
- Rate-limit cooldown is separate from transport failure — HTTP-level errors do not mark transport failure.

### Structured Error Responses

All errors return a consistent JSON envelope that **does not leak internal paths, stack traces, or database details**:

```json
{"type": "error", "error": {"type": "authentication_error", "message": "..."}}
```

- `400` — Invalid request
- `401` — Unauthorized (missing or invalid auth)
- `403` — Forbidden (shell command blocked)
- `502` — Upstream API error
- `500` — Internal server error

## Security-Related Configuration

| Variable | Default | Purpose |
|----------|---------|---------|
| `BRIDGE_AUTH_TOKEN` | unset | Comma-separated Bearer tokens for API authentication. Required when binding to non-loopback addresses. |
| `DASHBOARD_ADMIN_TOKEN` | unset | Admin token for dashboard login. Must be 8+ characters for non-loopback binding. When unset, dashboard goes into fail-closed mode. |
| `BRIDGE_SHELL_POLICY` | `disabled` | Shell execution policy: `disabled`, `allowlist`, or `unrestricted`. |
| `BRIDGE_SHELL_ALLOWLIST` | `git,ls,pwd,cat,...` | Comma-separated allowed commands when policy is `allowlist`. |
| `BRIDGE_RATE_LIMIT` | unset | Maximum concurrent requests. |
| `BRIDGE_MAX_BODY_SIZE` | `1048576` (1 MB) | Maximum request body size in bytes. |
| `BRIDGE_HOST` | `127.0.0.1` | Bind address. Use `0.0.0.0` for external access (requires auth). |
| `BRIDGE_PRIMARY_PROXIES` | `socks5://127.0.0.1:40001-40003` | Primary SOCKS5 proxy URLs for multi-agent support. |
| `BRIDGE_WARM_STANDBY_PROXIES` | `socks5://127.0.0.1:40004-40005` | Warm-standby proxy URLs for failover. |

## Known Security Considerations

- The dashboard session cookie stores the `DASHBOARD_ADMIN_TOKEN` value directly (no session indirection). This means the cookie value is equivalent to the admin password — protect it accordingly. The cookie is HttpOnly but transmitted over plain HTTP unless TLS is configured separately.
- Proxy traffic (SOCKS5) between the bridge and WARP containers is unencrypted. All proxy containers bind to `127.0.0.1` and are not exposed externally.
- The default configuration binds to `127.0.0.1` with auth disabled. This is safe for local-only use but must not be exposed to a network without enabling authentication.
