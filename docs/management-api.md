# Management API and dashboard

The management surface is separate from the Anthropic-compatible `/v1/*` API. REST endpoints are versioned under `/api/v1`; browser dashboard endpoints remain under `/api/dashboard` for compatibility.

## Authentication

Set a dedicated token:

```bash
export REST_API_TOKEN='replace-with-a-long-random-token'
```

Requests use:

```http
Authorization: Bearer <token>
```

When `REST_API_TOKEN` is unset, REST authentication falls back to `DASHBOARD_ADMIN_TOKEN`. When neither is configured, the API returns `401`.

Browser login sets an HttpOnly session cookie. Cookie-authenticated mutation requests must also present the dashboard CSRF cookie value in the expected CSRF header. Explicit dashboard-token or REST bearer authentication is not subjected to browser CSRF validation.

## Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/status` | Typed runtime status and redacted proxy summary. |
| `GET` | `/api/v1/proxies` | Typed node snapshots including role, health, circuit, lifecycle, identity freshness, and active leases. |
| `GET` | `/api/v1/config` | Safe resolved configuration with credentials removed. |
| `POST` | `/api/v1/config/preview` | Recursive merge, key validation, semantic validation, and changed-key preview. |
| `POST` | `/api/v1/config/apply` | Atomic write, post-write verification, and rollback on failure. |
| `POST` | `/api/v1/proxies/:port/restart` | Restart a configured managed primary node. Protected or leased nodes are rejected before runtime access. |
| `GET` | `/api/v1/metrics` | Typed authenticated operational counter snapshot. |
| `GET` | `/api/v1/audit` | Latest 100 bounded secret-safe management mutation events. |
| `GET` | `/api/v1/openapi.json` | OpenAPI 3.1 document assembled from the same DTO schema registry used by handlers. |

## Configuration workflow

Preview before apply:

```bash
curl -sS \
  -H "Authorization: Bearer $REST_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"toml":"rate_limit = 20\n"}' \
  http://127.0.0.1:4000/api/v1/config/preview
```

Apply:

```bash
curl -sS \
  -H "Authorization: Bearer $REST_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"toml":"rate_limit = 20\n"}' \
  http://127.0.0.1:4000/api/v1/config/apply
```

The service rejects unknown top-level keys, invalid zero values, invalid URLs, inconsistent retry bounds, and unsafe egress combinations. Existing configuration is backed up in memory before replacement; failed post-write verification restores the prior file.

## Proxy lifecycle policy

Only managed primary nodes may be restarted. The service rejects:

- protected warm-standby nodes;
- unknown ports;
- nodes with active request leases;
- invalid or unconfigured proxy IDs.

The same policy is shared by REST and dashboard operations.

## OpenAPI

Fetch the runtime document:

```bash
curl -sS \
  -H "Authorization: Bearer $REST_API_TOKEN" \
  http://127.0.0.1:4000/api/v1/openapi.json > openapi.json
```

Schema tests ensure every public management DTO is registered and referenced. The document intentionally excludes secrets and raw credential fields.

## Dashboard compatibility routes

The embedded dashboard uses:

- `/api/dashboard/login`, `/logout`, and `/auth/status`;
- `/api/dashboard/status`, `/proxies`, `/config`, and `/diagnostics`;
- `/api/dashboard/config/save`;
- `/api/dashboard/proxy/:port/restart`;
- `/api/dashboard/events`;
- `/api/dashboard/test/stream`.

Dashboard event and test streams are authenticated and bounded.
