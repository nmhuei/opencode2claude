# Provider template and management boundary

`opencode2api` owns provider configuration and request routing. It never
launches OpenCode or Kilo Code. Add a normal OpenAI-compatible API by creating
records; Rust changes are needed only for a genuinely different wire protocol.

## Common schema-v3 template

```toml
schema_version = 3

[router]
active_alias = "coding-1m"

[providers.provider-id]
name = "Human Provider Name"
kind = "openai-compatible"       # opencode, kilo, bai, or generic
protocol = "openai_chat_completions"
base_url = "https://api.example.com/v1"
enabled = true

[credentials.provider-main]
provider = "provider-id"
source = "env:PROVIDER_API_KEY"  # file:/path or managed[:id]
auth_scheme = "bearer"            # bearer, x-api-key, or none

[models.provider-id."model-id"]
wire_model = "model-id"
context_window = 1000000
max_output_tokens = 128000
supports_thinking = true
verified_context = true
free = true

[aliases.coding-1m]
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.coding-1m.candidates]]
provider = "provider-id"
model = "model-id"
credential = "provider-main"
priority = 0
```

The relationship is:

```text
client alias -> ordered candidates -> provider + model + credential -> adapter -> HTTP endpoint
```

`auto_compact_window` is derived centrally as
`floor(context_window * 80 / 100)`. It is not configurable per provider or
candidate. A 1M alias therefore keeps a 1,000,000-token Claude Code context
and compacts at 800,000 tokens.

## Add an API without source changes

```bash
opencode2api provider add my-api https://api.example.com/v1 \
  --kind openai-compatible --protocol openai_chat_completions
opencode2api credential set my-api main --env MY_API_KEY
opencode2api model discover my-api
opencode2api model verify my-api my-model
opencode2api alias set coding-1m --client-model 'sonnet[1m]' \
  --context-window 1000000 --candidate my-api:my-model:main
opencode2api alias use coding-1m
```

`model discover` reads the provider's `/models` endpoint and stores provider-
local metadata. Discovery metadata is unverified until `model verify`, which
performs a second live catalog check and validates strict-alias requirements.
A strict alias cannot use unknown or unverified context metadata. Relative
`file:PATH` secrets are resolved relative to the selected TOML file.

## Secret storage

TOML contains only `env:NAME`, `file:/path`, or `managed[:id]` references.
Managed values are stored in `provider-secrets.json` beside the selected TOML
file with mode `0600`. `credential list`, config views, JSON, logs, and errors
show status or source only, never the value. Provider credentials are separate
from bridge client keys in `auth_tokens`.

## CLI ownership

| Command | Owns |
| --- | --- |
| `provider add/remove/list/show/enable/disable/test` | endpoint lifecycle |
| `credential set/list/remove/test` | key reference and availability |
| `model add/remove/list/show/discover/verify` | local wire metadata |
| `alias set/show/list/remove/use` | Claude identity and fallback order |
| `route explain/test/simulate` | offline/debug routing decisions |
| `health [PROVIDER]` | daemon runtime health, with direct fallback |
| `config path/show/validate/migrate` | scope, validation, and schema migration |

`--config PATH` selects the file scope. Mutations are validated and atomically
written through `ProviderConfigStore`; unrelated bridge settings are preserved.
Use `POST /api/v1/provider-runtime/reload` after a write when the daemon is
running, or restart it if reload is unavailable.

## Protocol boundary

The current execution path supports OpenAI Chat Completions with Bearer,
`x-api-key`, or no authentication. `openai_responses` and
`anthropic_messages` are reserved protocol values and fail closed until a
matching request/response adapter is added; they are never silently sent as a
Chat Completions payload. Adding another endpoint that uses the generic
protocol remains data-only.
