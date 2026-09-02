# Provider management

OpenCode2API is the provider manager and local HTTP gateway. It does not spawn
OpenCode, Kilo Code, Claude Code, or another provider CLI. Provider records are
HTTP endpoints; adapters only translate protocol differences.

## Ownership model

```text
Claude Code alias -> ordered candidates -> provider + model + direct credential/pool -> scheduler -> adapter -> HTTP API
```

| Record | Owns |
| --- | --- |
| provider | endpoint, protocol, kind, non-secret headers, enabled state |
| credential | provider-scoped secret reference and auth scheme |
| model | provider-local wire ID and context/capability metadata |
| alias | stable client model name, required context, fallback order |
| credential pool | named credentials, quota scope, fair selection, and local capacity limits |
| runtime | immutable compiled snapshot plus scheduler-owned in-memory capacity/cooldowns |

Model IDs are keyed by `(provider, model)`, so two providers may safely expose
the same model name. A generic OpenAI Chat Completions API requires no Rust
change. The registry also records protocol explicitly; unsupported Responses
or Anthropic Messages records fail closed instead of receiving a mismatched
Chat Completions payload.

## Schema v3

The provider registry uses named tables. Configuration stores references only;
managed secret bytes live in `provider-secrets.json` beside the TOML file with
owner-only permissions.

```toml
schema_version = 3

[router]
active_alias = "free-1m"

[providers.bai]
name = "B.AI"
kind = "bai"
base_url = "https://api.b.ai/v1"
protocol = "openai_chat_completions"
enabled = true

[credentials.bai-main]
provider = "bai"
source = "env:BAI_API_KEY" # file:/path or managed[:id] are also valid
auth_scheme = "x-api-key"

[models.bai."deepseek-1m"]
wire_model = "deepseek-1m"
context_window = 1000000
max_output_tokens = 128000
verified_context = true
free = true

[aliases.free-1m]
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.free-1m.candidates]]
provider = "bai"
model = "deepseek-1m"
credential_pool = "free-1m"
priority = 10

[credential_pools.free-1m]
provider = "bai"
strategy = "round_robin"

[[credential_pools.free-1m.members]]
credential = "bai-main"
quota_scope = "bai-account-main"
max_in_flight = 1
requests_per_minute = 20
tokens_per_minute = 120000
weight = 1
```

Array-style schema v2 and legacy singleton configuration remain readable.
`config migrate --write` is the only command that rewrites provider records to
v3, and it creates a timestamped adjacent backup first. Provider mutations
preserve unrelated bridge settings in the same TOML file.

## CLI

```bash
# Add any OpenAI-compatible endpoint without source changes.
opencode2api provider add moonshot https://api.example/v1 \
  --kind openai-compatible --protocol openai_chat_completions
opencode2api credential set moonshot primary --env MOONSHOT_API_KEY
opencode2api model discover moonshot

# Or register metadata explicitly.
opencode2api model verify moonshot model-id

# Stable Claude-facing identity with independent fallback providers.
opencode2api alias set free-1m --client-model 'sonnet[1m]' \
  --context-window 1000000 \
  --candidate bai:deepseek-1m:bai-main \
  --candidate kilo:free-model:kilo-free
opencode2api alias use free-1m
opencode2api route explain free-1m
opencode2api route simulate free-1m --concurrent 3

# A pool is configured independently from aliases. Empty RPM/TPM disables
# that local bucket; no secret value is accepted by this command.
opencode2api pool set free-1m --provider bai --strategy round_robin \
  --member bai-main,bai-account-main,1,20,120000,1
opencode2api pool show free-1m
opencode2api pool list
opencode2api pool remove free-1m
```

Canonical management commands are `provider list|show|add|remove|enable|disable|test`,
`credential list|set|remove|test`, `model list|show|discover|verify`,
`alias list|show|set|remove|use`, `route explain|test|simulate`,
`pool list|show|set|remove`, `health [PROVIDER]`, and
`config path|show|validate|migrate`.

All management commands support `--json`, `--quiet`, and `--color`. JSON never
contains secret values. `--config PATH` selects the file being read or
mutated; successful writes report `restart_required: true`. An authenticated
daemon can reload the validated file with
`POST /api/v1/provider-runtime/reload`.

Legacy `provider opencode`, `provider api`, `provider models`, and `upstream`
remain compatibility commands for one release cycle.

## Claude Code 1M contract

The alias is the client identity; each candidate retains its own upstream wire
model. A strict 1M alias is exposed as `sonnet[1m]` and exports:

```text
CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000
CLAUDE_CODE_AUTO_COMPACT_WINDOW=800000
CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=80
CLAUDE_CODE_DISABLE_1M_CONTEXT=0
```

Every alias derives `floor(context_window * 80 / 100)` for auto-compaction.
Fallback candidates cannot change this threshold. Strict aliases reject
unknown, unverified, or undersized context metadata, so a model name containing
`1m` is not proof of a 1M context window.

`model discover` records metadata as unverified. `model verify` performs a live
`GET /models` request, matches the exact model ID, requires a numeric context
window, checks every strict alias that references it, and only then marks the
record verified.

Verify the installed client contract with:

```bash
python3 tests/claude_code_e2e.py --provider-alias free-1m
```

## Fallback and 429 policy

The planner filters disabled providers and insufficient-context candidates.
The runtime then visits candidates by priority, admits one healthy pool member
atomically, and applies transient health state:

| Failure | Action |
| --- | --- |
| 401/403 | quarantine that credential, then try another credential/provider |
| 429 | honor bounded `Retry-After`, rotate credential, then advance provider |
| 404 | cool down that provider/model until refresh |
| 5xx/transport | short provider cooldown, then advance candidate |
| 402 | quarantine that provider briefly, then try another candidate |
| 4xx client error | return immediately; the request itself is likely invalid |

Round-robin advances only after a successful admission. Least-loaded compares
`in_flight / max_in_flight` exactly, and weighted round-robin uses a bounded
deterministic cursor. Members that share a `quota_scope` share one RPM/TPM and
in-flight budget, so multiple keys cannot bypass an account-level limit. Local
capacity exhaustion is fail-fast and returns a sanitized HTTP 429; a later
alias candidate is tried when it has capacity. Capacity state is process-local
to one daemon generation, so multi-node coordination requires a distributed
quota backend in a future version.

Cooldowns are in memory only. Streaming requests may switch candidates only
before the first content event; after content is committed, the stream closes
without replaying the request.

Admission estimates `(input + requested output) * 1.25` from the serialized
provider request, including nested tool arguments. Successful JSON responses
report OpenAI `usage` fields to the lease observer; common
`x-ratelimit-remaining-*` headers are parsed defensively and can cool a member
or its shared quota scope
when the provider reports zero remaining capacity. These observations refine
health only; configured local RPM/TPM buckets remain authoritative.

## Debugging truth

Relative `file:PATH` secret references are resolved beside the selected TOML
file; absolute paths remain absolute. Use `config show --file` for the redacted TOML document,
`config show --effective` for process precedence, `health` for a direct/API
probe, and `GET /api/v1/provider-runtime` for the daemon snapshot actually
serving requests. The runtime response exposes providers, aliases, counts, and
cooldown counts, capacity summaries, pool strategy/member counts, and aggregate
admission/rejection counters; never authorization headers, secret bytes,
prompts, quota secrets, or raw upstream bodies. `health` prefers this live
daemon view and falls back to direct provider probes when the daemon is not
reachable.
