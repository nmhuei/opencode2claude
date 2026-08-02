# External Agent API Manual Verification

**Date:** 2026-07-21
**Bridge:** `http://127.0.0.1:4000`
**Version:** `0.5.0`
**Pinned model:** `opencode/deepseek-v4-flash-free`

## Conclusion

The bridge is currently usable as a **local Anthropic Messages API-compatible endpoint** for external agents and backend automation. It is not yet a dual-protocol/public API gateway: inbound OpenAI Chat Completions, browser CORS, remote binding, and production authentication are not enabled in the active configuration.

## Manual verification matrix

| Capability | Result | Evidence |
|---|---:|---|
| Health endpoint | PASS | `GET /health` returned HTTP 200 and version 0.5.0 |
| Model discovery | PASS | `GET /v1/models` returned `opencode/deepseek-v4-flash-free` |
| Token counting | PASS | `POST /v1/messages/count_tokens` returned `input_tokens: 13` |
| Generic HTTP client, non-streaming | PASS | HTTP 200, exact result `EXTERNAL_AGENT_API_OK` |
| Generic HTTP client without API key | PASS | HTTP 200, exact result `NO_AUTH_LOCAL_OK` |
| SSE streaming | PASS | `text/event-stream`, complete `message_start` → deltas → `message_stop`, exact text `EXTERNAL_STREAM_OK` |
| Tool call | PASS | First turn returned `stop_reason=tool_use` and `get_magic_number` |
| Tool result continuation | PASS | Second turn consumed tool result and returned `TOOL_LOOP_OK` |
| Anthropic error shape | PASS | Empty messages returned HTTP 400 with `invalid_request_error` |
| Reusable Python client | PASS | Standard-library client returned a valid Anthropic message response |
| Inbound OpenAI `/v1/chat/completions` | NOT IMPLEMENTED | Probe returned HTTP 404 |
| Browser CORS preflight | NOT ENABLED | `OPTIONS /v1/messages` returned HTTP 405; no CORS headers |
| Remote access | NOT ENABLED | Listener is bound to `127.0.0.1:4000` |
| Runtime authentication | DISABLED | Effective config reports `auth_enabled=false` |

## Supported external-agent contract

Use either form depending on how the client constructs resource paths:

- Base root: `http://127.0.0.1:4000`, resource `/v1/messages`
- Base with version: `http://127.0.0.1:4000/v1`, resource `/messages`

Clients must avoid producing `/v1/v1/messages`.

## Current suitable uses

- Local coding or workflow agents that support a custom Anthropic base URL
- Backend services and bots using HTTP JSON
- Streaming terminal or server applications using SSE
- Agent runtimes implementing tool-use loops
- CI/local automation where the bridge and caller run on the same host

## Current limitations

- OpenAI-only clients cannot connect directly until an inbound `/v1/chat/completions` adapter is added.
- Browser-only apps need CORS support or a same-origin backend proxy.
- Other machines cannot access the active listener because it is loopback-only.
- Public/private-network deployment requires strong bearer/API-key authentication and preferably TLS through a reverse proxy.
- The official Python `anthropic` package was not installed on this host, so verification used raw standards-based HTTP rather than that SDK.

## Evidence files

All request/response artifacts are under:

```text
artifacts/external-agent-manual-verify/
```

Key files:

```text
sync.json
no_auth_sync.json
stream_verify.json
advanced_summary.json
tool_first.json
tool_second.json
openai_probe.headers
cors_options.headers
invalid_request.body
example_client.py
```
