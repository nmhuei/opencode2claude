# API Key Management V2

## Purpose

The API workspace now treats every client credential as an independent resource instead of an item in the global `auth_tokens` array. Each key has a stable ID, lifecycle state, model policy, token limits, traffic limits, permissions, and runtime usage counters.

Managed changes are hot-reloaded. Port, proxy, and other global configuration changes may still require a process restart, but API-key creation, updates, disable/enable, rotation, and revocation do not.

## Storage model

The main TOML configuration remains the source for global bridge settings and legacy `auth_tokens` values.

Managed credentials are stored in a sidecar file beside the active TOML file:

```text
opencode2api.toml
opencode2api.api-keys.json
```

The sidecar stores:

- Stable key ID
- Name, description, environment, timestamps, and status
- Public fingerprint
- SHA-256 digest of the complete high-entropy secret
- Per-key policy
- Suppressed legacy-key hashes used to make legacy revocation persistent

It never stores the raw managed secret. A generated or rotated secret is returned once and is then unavailable to the server and dashboard.

Managed secret format:

```text
sk-oc2-<stable-key-id>.<random-secret>
```

Legacy `auth_tokens` entries are imported into the in-memory registry on startup. They continue to authenticate without migration. The compatibility revoke endpoint also removes selected legacy entries from TOML while preserving comments.

## Request pipeline

```text
Incoming request
  -> extract Bearer / x-api-key credential
  -> hash and constant-time compare against registry
  -> validate status and expiration
  -> validate endpoint permission
  -> per-key concurrent/RPM/daily admission
  -> attach AuthenticatedClient to request extensions
  -> enforce model, token, reasoning, tool, search, shell, and streaming policy
  -> route upstream using stable key ID, never the raw secret
  -> hold concurrency lease until the response body or stream is finished
  -> update runtime usage counters
```

When no legacy or managed key is configured, client authentication remains disabled for backward compatibility. Creating the first managed key enables authentication immediately.

## Key policy

Each managed key supports:

### Model policy

- Default model
- Allowed-model list
- Whether the client may override the model
- Maximum output tokens
- Over-limit behavior: reject or clamp

### Reasoning policy

- Inherit request
- Force disabled
- Force enabled
- Reasoning effort
- Maximum reasoning-token budget

### Traffic policy

- Maximum concurrent requests
- Requests per minute
- Daily request quota

Zero or absent values mean unlimited.

### Permissions

- Anthropic Messages endpoint
- OpenAI Chat Completions endpoint
- Model listing
- Token counting
- Streaming
- Tool calls
- Web search tools
- Shell commands

Shell permission defaults to disabled for newly managed keys. Imported legacy keys preserve the behavior of the global shell policy.

## Management routes

```text
GET    /api/dashboard/control/api-keys
POST   /api/dashboard/control/api-keys
POST   /api/dashboard/control/api-keys/verify
GET    /api/dashboard/control/api-keys/:id
PATCH  /api/dashboard/control/api-keys/:id
DELETE /api/dashboard/control/api-keys/:id
POST   /api/dashboard/control/api-keys/:id/rotate
```

Compatibility routes remain available:

```text
POST /api/dashboard/control/api-keys/revoke
POST /api/dashboard/control/client-config
```

All management mutations require an authenticated dashboard session or explicit dashboard token. Cookie-authenticated mutations additionally require the existing double-submit CSRF token.

## Dashboard interaction model

The API page uses a master table and a right-side settings drawer.

The main workspace provides:

- Create API key
- Check API key locally
- Total, active, disabled, and expiring-soon counters
- Search and filtering by status, environment, and model
- Key name, fingerprint, configuration, limits, usage, and lifecycle status

The create workflow contains:

- Identity and environment
- Expiration
- Client presets
- Model and reasoning configuration
- Traffic limits
- Endpoint and feature permissions
- One-time secret display

The settings drawer contains:

- General lifecycle controls
- Model allowlist and model-override policy
- Output and reasoning limits
- Traffic limits
- Permissions
- Runtime usage
- Placeholder or current-session client-config generation
- Rotation and revocation actions

Desktop uses a side drawer. Mobile uses a full-screen drawer and modal layout.

## Lifecycle semantics

### Create

A key is persisted and activated immediately. The secret is shown once.

### Update

Policy and metadata changes are atomically persisted and replace the in-memory policy immediately.

### Disable

Authentication with that key returns a permission error immediately. The key may be enabled later.

### Rotate

The existing digest is replaced atomically. The old secret becomes invalid immediately and the replacement is shown once.

### Revoke

The key becomes permanently revoked. Revoked records are immutable and excluded from the normal inventory. A matching revoked secret receives a forbidden response rather than being treated as an unknown credential.

## Verification

The browser verification script is:

```text
scripts/manual_verify_api_key_management_redesign.py
```

It verifies:

- Empty workspace
- Managed key creation
- Hash-only persistence
- Immediate authentication without restart
- Local key checking
- Drawer policy rendering
- Permission hot reload
- Disable/enable hot reload
- Placeholder config safety
- Immediate rotation
- Immediate revocation
- Desktop rendering
- Mobile page-width containment
- No console, page, or request failures

Artifacts are written to:

```text
artifacts/redesign/api-key-management-v2/
```
