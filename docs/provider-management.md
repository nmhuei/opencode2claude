# Provider management

OpenCode2API is a local HTTP gateway. It does not start OpenCode, Kilo Code, or
any other provider CLI. It selects an HTTP endpoint, model, and credential for
each request and translates the request through the configured adapter.

## Configuration model

Schema version 2 keeps these identities separate:

| Object | Purpose |
| --- | --- |
| provider | endpoint, protocol, provider kind, non-secret headers |
| credential | provider-scoped reference to an environment variable, file, or managed secret |
| model | provider-local wire model and verified context metadata |
| alias | stable Claude Code model identity and ordered fallback candidates |

The main TOML file contains references only. Managed secret values are stored
in `provider-secrets.json` beside the configured TOML file with owner-only
permissions. Client authentication keys in `auth_tokens` remain a separate
registry and are never used as an upstream credential automatically.

## CLI workflow

```text
opencode2api provider add bai https://api.b.ai/v1 --kind bai
opencode2api provider credential set bai bai-main --env BAI_API_KEY
opencode2api provider credential set bai bai-main --api-key-stdin
opencode2api provider model add bai deepseek-1m \
  --context-window 1000000 --max-output-tokens 128000 --free
opencode2api provider list
opencode2api provider credential list
opencode2api provider alias set free-1m \
  --candidate bai:deepseek-1m:bai-main
opencode2api provider alias show free-1m
opencode2api provider activate free-1m
opencode2api provider health
```

Use `--config PATH` on provider, credential, alias, and activation commands to
operate on a specific configuration. JSON output is safe for automation and
does not contain raw secrets.

`provider opencode`, `provider api`, `provider models`, and `upstream` remain
compatibility commands for the legacy singleton configuration.

## Claude Code 1M contract

An active strict 1M alias is exposed to Claude Code as
`sonnet[1m]`, while `OPENCODE_MODEL` remains the upstream wire model
for compatibility. The launcher exports:

```text
CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000
CLAUDE_CODE_AUTO_COMPACT_WINDOW=1000000
CLAUDE_CODE_DISABLE_1M_CONTEXT=0
```

`/context` must be verified against the installed Claude Code version by
`python3 tests/claude_code_e2e.py --provider-alias free-1m`. A proxy cannot
claim 1M merely because an upstream model name contains `1m`; strict aliases
require verified context metadata.

## Fallback rules

Candidates are sorted by ascending priority. A strict 1M alias rejects unknown
or sub-million models during registry compilation. On a request, the route
planner filters candidates by the required output budget, then the adapter
for each candidate controls its endpoint, authentication scheme, and wire
model. Credential rotation stays within the candidate provider. Streaming
responses must fail before content is emitted to be safely retried.

Free gateways may apply quotas or retain prompts. Operators should inspect the
provider's terms before sending confidential data.
