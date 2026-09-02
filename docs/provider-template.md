# Provider template and management boundary

`opencode2api` is the provider manager and gateway. OpenCode, Kilo Code, B.AI,
or any other upstream is only a configured HTTP provider; this project does
not spawn their CLIs.

## Common provider template

The schema-v2 template is intentionally split into five records:

```toml
schema_version = 2

[[providers]]
id = "provider-id"
name = "Human Provider Name"
kind = "openai-compatible"       # opencode, kilo, bai, or generic
protocol = "openai_chat_completions"
base_url = "https://api.example.com/v1"
enabled = true

[[credentials]]
id = "provider-main"
provider_id = "provider-id"
env = "PROVIDER_API_KEY"          # or file / managed
auth_scheme = "bearer"             # bearer, x-api-key, or none

[[models]]
provider_id = "provider-id"
model_id = "model-id"
wire_model_id = "model-id"         # optional; defaults to model_id
context_window = 1000000
max_output_tokens = 128000
verified_context = true
supports_thinking = true
free = true

[[aliases]]
id = "coding-1m"
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.candidates]]
provider_id = "provider-id"
model_id = "model-id"
credential_id = "provider-main"
priority = 0
```

The relationship is:

```text
client alias -> ordered candidates -> provider + model + credential -> adapter -> HTTP endpoint
```

The model's auto-compact threshold is calculated centrally as
`floor(context_window * 80 / 100)`. It is not a provider setting, so a 1M
model keeps `CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000` and gets
`CLAUDE_CODE_AUTO_COMPACT_WINDOW=800000`.

## Adding a new API

For an OpenAI-compatible API, no Rust change is required:

```bash
opencode2api provider add my-api https://api.example.com/v1 \
  --kind openai-compatible --protocol openai_chat_completions
opencode2api provider credential set my-api main --env MY_API_KEY
opencode2api provider model add my-api my-model \
  --context-window 128000 --max-output-tokens 32768
opencode2api provider alias set coding \
  --client-model claude-sonnet-5 \
  --context-window 128000 \
  --candidate my-api:my-model:main
opencode2api provider activate coding
```

For a provider that uses a new wire protocol, authentication handshake, or
response format, one adapter is required. The provider's endpoint, model
metadata, credentials, aliases, and fallback order still remain data-driven;
adding another provider using that adapter does not require another code
change.

## CLI ownership

The canonical management surface is the `opencode2api provider` namespace:

| Command | Owns |
| --- | --- |
| `provider add/remove/list` | endpoint and protocol identity |
| `provider credential set/list/remove` | provider-scoped key references |
| `provider model add/remove` | wire model and capability metadata |
| `provider alias set/show/list` | Claude-facing alias and fallback candidates |
| `provider activate` | active route used by the launcher |
| `provider models` | live/cached upstream model discovery |
| `provider health/status` | operational state |

Claude Code is only the client. Running `opencode2api` without a subcommand
launches Claude Code with the generated `ANTHROPIC_*` and model-context
environment. It does not manage provider records.

## 9router comparison

The current 9router architecture separates provider connections, compatible
provider nodes, aliases, custom models, combos, API keys, and usage persistence.
It also provides dashboard CRUD, model combos, multi-account rotation, quota
tracking, cooldowns, token refresh, and request logging. These are useful
reference capabilities from [its architecture document](https://github.com/decolua/9router/blob/master/docs/ARCHITECTURE.md)
and [provider connection repository](https://github.com/decolua/9router/blob/master/src/lib/db/repos/connectionsRepo.js).

This project deliberately maps the stable core first:

| 9router concept | `opencode2api` equivalent | Status |
| --- | --- | --- |
| provider node/connection | `Provider` + `Credential` | implemented |
| custom model | `ModelInfo` | implemented |
| model alias/combo | `ModelAlias` with ordered candidates | implemented |
| client API key | `auth_tokens` | separate by design |
| atomic runtime reload | `ProviderRuntimeHandle` | implemented |
| health/cooldown/quota history | `HealthManager` and routing hooks | partial |
| dashboard CRUD | CLI and management API | partial |

The important boundary is that provider identity is data, while protocol
translation is code. This keeps adding another OpenAI-compatible endpoint a
configuration operation instead of a source-code operation.
