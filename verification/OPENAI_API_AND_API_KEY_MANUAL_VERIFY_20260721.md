# OpenAI API and API-key manual verification — 2026-07-21

## Implemented

- `POST /v1/chat/completions` OpenAI-compatible inbound route.
- Transparent OpenAI JSON and SSE response forwarding.
- Shared model pinning, request-size limit, rate limiter, retry/model fallback, direct/proxy egress, and bridge auth.
- OpenAI-shaped validation and authentication errors.
- DeepSeek V4 compatibility normalization:
  - thinking defaults to disabled for ordinary OpenAI requests;
  - reasoning effort enables thinking;
  - reasoning mode removes sampling and forced-tool controls that conflict with the provider.
- `opencode2api api-key generate` command.
- `--save`, `--replace`, `--config`, `--count`, `--bytes`, and `--prefix` options.
- Atomic TOML persistence preserving existing comments and tokens.
- Sensitive config writes use mode `0600` on Unix.

## Manual results

### API-key CLI

- Prefix: `sk-oc2-` — PASS
- Default entropy: 32 random bytes / 256 bits — PASS
- Default key length: 71 characters — PASS
- Save into `auth_tokens` — PASS
- File mode after save: `0600` — PASS
- Restart-required flag — PASS

### Authenticated temporary bridge

| Case | Result |
|---|---|
| Invalid Bearer key | HTTP 401, `error.code=invalid_api_key` |
| OpenAI non-streaming | `OPENAI_SYNC_OK` |
| OpenAI SSE | `OPENAI_STREAM_OK`, `[DONE]` received |
| Function tool call | `get_magic_number` emitted |
| Tool-result continuation | `OPENAI_TOOL_OK` |

### Stable bridge after release rebuild/restart

- PID changed to the newly built `opencode2api-serve` process.
- `/health` returned version `0.5.0`.
- Live `/v1/chat/completions` returned `LIVE_OPENAI_OK` using model `deepseek-v4-flash-free`.

## Quality gates

- `cargo fmt --check` — PASS
- `git diff --check` — PASS
- `cargo clippy --all-targets --all-features -- -D warnings` — PASS
- Full tests — 421 PASS, 1 environment-dependent WARP test ignored by design
- Release binaries rebuilt — PASS

## Evidence

- `/tmp/oc2-openai-manual/key-summary.json`
- `/tmp/oc2-openai-manual/openai-summary.json`
- `tmp/check_release.sh`
- `tmp/check_openai_endpoint.sh`
- `tmp/live_openai_smoke.py`
