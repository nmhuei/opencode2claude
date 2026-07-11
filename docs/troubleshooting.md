# Troubleshooting and incident playbooks

Start with:

```bash
opencode2api doctor
opencode2api server status
curl -fsS http://127.0.0.1:4000/health/live
curl -fsS http://127.0.0.1:4000/health/ready
```

Use `--json` for machine-readable CLI output.

## Server will not start

Common causes:

- port already in use;
- invalid TOML or unsupported future schema version;
- public bind without strong client/dashboard authentication;
- proxy mode with no configured proxies;
- direct fallback enabled in proxy mode;
- private SearXNG configured without explicit opt-in;
- runtime directory not writable.

Check the configured port:

```bash
opencode2api --json server config
```

Use a different port:

```bash
opencode2api server start --port 4010 --no-proxy
```

## Liveness is healthy but readiness fails

This is expected when the process is running but cannot safely serve traffic. Inspect authenticated diagnostics. Typical causes are:

- no eligible proxy route;
- failed critical worker;
- stale or insufficient verified exit identities;
- strict unique-exit policy not met;
- runtime dependency unavailable in the configured mode.

Do not route production traffic based on `/health` or `/health/live` alone.

## Proxy mode fails all requests

Verify the local SOCKS ports:

```bash
for port in 40001 40002 40003; do
  nc -z 127.0.0.1 "$port" || echo "missing $port"
done
```

Inspect node state:

```bash
opencode2api --json proxy ps
```

Preview lifecycle actions without changing containers:

```bash
opencode2api --json proxy restart --dry-run
opencode2api --json proxy purge --dry-run
```

Protected standby ports `40004-40005` cannot be restarted or purged by the bridge.

## Duplicate exit identities

Multiple WARP nodes may resolve to the same public exit. This is not counted as independent capacity. Run Tier C or inspect authenticated proxy diagnostics. To require independent capacity:

```toml
require_verified_exit_ip = true
minimum_unique_exit_ips = 2
```

Readiness remains false until fresh identity probes satisfy the policy.

## Upstream 429 or provider errors

The retry policy separates rate limits, provider client errors, provider server errors, and transport failures. Check authenticated metrics for the relevant counter. Configure explicit compatible fallback models rather than relying on implicit fallback for reasoning streams.

Do not treat a provider `400` or `500` as proof that a proxy is unhealthy. The bridge does not penalize transport for ordinary provider failures.

## Streaming ends early

Check for:

- reverse-proxy response buffering;
- reverse-proxy idle timeout;
- upstream malformed SSE;
- configured `max_sse_line_bytes` too small;
- client disconnect;
- worker or server shutdown.

Protocol errors are emitted as bounded SSE error events followed by a terminal `message_stop` when possible.

## Dashboard mutation returns CSRF error

Log in again and ensure the browser sends the CSRF header corresponding to the CSRF cookie. API automation should use `Authorization: Bearer $REST_API_TOKEN` rather than dashboard cookies.

## Stop refuses to kill a PID

The supervisor only signals a process when executable identity and start marker match the PID file. A stale or legacy PID file may be reported as unmanaged and will not grant permission to terminate an unrelated process. Remove stale runtime state only after confirming the process is not the bridge.

## Update or install fails checksum verification

Do not bypass the checksum. Confirm that the binary and companion `.sha256` belong to the same release and platform. Verify the release provenance before retrying. A failed self-update leaves or restores the previous binary.

## Configuration apply rolls back

The management API performs a post-write parse and validation. If it fails, the previous content is restored. Use `/api/v1/config/preview` to inspect changed keys and validation errors before applying.

## Collecting an incident bundle

Collect only redacted data:

```bash
opencode2api --json doctor > doctor.json
opencode2api --json server status > status.json
opencode2api --json server config > config-safe.json
opencode2api --json proxy ps > proxies.json
```

Add authenticated metrics and diagnostics only after reviewing them for environment-specific sensitivity. Never attach `.env`, raw tokens, private keys, or unredacted upstream payloads.
