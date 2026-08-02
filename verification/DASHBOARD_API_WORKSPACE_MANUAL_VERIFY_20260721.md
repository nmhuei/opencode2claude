# Dashboard API Workspace Manual Verification — 2026-07-21

## Status

**PASS**

## Implemented UI changes

- Renamed `Access` to `API`.
- Legacy `#access` hash redirects to `#api`.
- Increased dashboard typography and contrast.
- Added self-contained inline SVG icons for navigation, page headings, activity metrics, refresh, copy, and download actions.
- Added live uptime with one-second updates and backend resynchronization.
- Added fingerprint-only saved API-key inventory.
- Added API-key generation, append, replace, and revoke workflows.
- Refuses to revoke the final saved client key.
- Added client configuration generator for:
  - `.env`
  - Claude Code `settings.json`
  - OpenAI Python
  - Anthropic Python
  - cURL shell script
- Placeholder key is the default. Active/latest secret export requires an explicit selection and confirmation.

## Backend contracts

```text
GET  /api/dashboard/control/api-keys
POST /api/dashboard/control/api-keys
POST /api/dashboard/control/api-keys/revoke
POST /api/dashboard/control/client-config
```

Saved keys are never returned in full by the inventory endpoint. TOML edits are atomic and preserve unrelated settings and comments.

## Client config conventions

Generated configurations use:

```text
OPENAI_API_KEY
OPENAI_BASE_URL=http://127.0.0.1:4000/v1
ANTHROPIC_API_KEY
ANTHROPIC_BASE_URL=http://127.0.0.1:4000
```

Claude Code output uses a `settings.json` document with an `env` object and the Claude Code settings JSON schema.

## Manual browser verification

Script:

```text
scripts/manual_verify_api_workspace.py
```

Artifact:

```text
artifacts/redesign/api-workspace/summary.json
```

Verified:

- 7 sidebar icons and 4 activity metric icons render from embedded SVG.
- No external icon/font/CDN request.
- Uptime advanced from `0m 03s` to `0m 06s` during a 3.2-second observation.
- Saved key inventory contained fingerprints only.
- Ephemeral key generation worked.
- Placeholder Claude Code configuration contained no active/generated secret.
- Downloaded Claude Code JSON parsed successfully.
- Explicit latest-key export required confirmation and displayed a secret warning.
- Browser revoke removed the selected key and preserved the TOML comment.
- Desktop geometry: `1440 / 1440`, no horizontal overflow.
- Mobile geometry: `390 / 390`, no horizontal overflow.
- No console errors, page errors, or failed requests.

Screenshots:

```text
artifacts/redesign/api-workspace/desktop-api.png
artifacts/redesign/api-workspace/mobile-api.png
```
