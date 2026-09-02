# Provider Management and Claude Code 1M Alias Design

## Goal

Turn `opencode2api` into a local multi-provider gateway that keeps Claude Code on one stable client-facing model alias while routing each request to a configured upstream provider/model. The alias must make Claude Code expose a 1,000,000-token context window through `/context`, and every fallback candidate must satisfy the same context and capability contract.

## Scope

In scope:

- HTTP gateway providers such as Kilo Gateway, OpenCode Zen, B.AI, and generic OpenAI-compatible endpoints.
- Provider-scoped credentials stored by reference, never embedded in model configuration.
- Live model catalog discovery and explicit model capability metadata.
- Stable aliases such as `oc2api/free-1m` mapped to a Claude Code-compatible `[1m]` model identity.
- Strict same-context fallback for long conversations.
- OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages adapters.
- CLI and management API access to provider, credential, catalog, alias, health, and runtime state.
- Backward compatibility with the current singleton upstream configuration during migration.

Out of scope:

- Spawning or controlling the OpenCode or Kilo CLI processes.
- Implementing an agent runtime supervisor.
- Treating an upstream free model list as permanent; free availability is refreshed from provider catalogs.

## Client-facing 1M contract

Claude Code receives a compatibility model identity, for example:

```text
sonnet[1m]
```

The local launcher sets:

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:4000
ANTHROPIC_AUTH_TOKEN=<local-gateway-token>
ANTHROPIC_MODEL=sonnet[1m]
CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000
CLAUDE_CODE_AUTO_COMPACT_WINDOW=800000
CLAUDE_CODE_DISABLE_1M_CONTEXT=0
```

The compatibility identity is separate from the upstream model ID. Claude Code-facing responses use the compatibility identity; internal history records both the requested identity and the actual upstream target.

The implementation must run a black-box verification against the installed Claude Code version. If a version does not recognize the configured `[1m]` identity, the launcher must fail with a clear compatibility error rather than claim that `/context` is 1M.

## Domain model

```rust
pub type ProviderId = String;
pub type CredentialId = String;
pub type AliasId = String;

pub enum ProviderKind {
    KiloGateway,
    OpenCodeZen,
    Bai,
    OpenAiCompatible,
}

pub enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

pub struct Provider {
    pub id: ProviderId,
    pub display_name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub protocols: Vec<Protocol>,
    pub auth: AuthConfig,
    pub extra_headers: std::collections::HashMap<String, String>,
    pub enabled: bool,
}

pub struct Credential {
    pub id: CredentialId,
    pub provider_id: ProviderId,
    pub source: SecretSource,
    pub auth_scheme: AuthScheme,
    pub enabled: bool,
}

pub struct ModelInfo {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub wire_model_id: String,
    pub protocols: Vec<Protocol>,
    pub context_window: u64,
    pub max_output_tokens: Option<u64>,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub supports_streaming: bool,
    pub is_free: bool,
}

pub struct ModelAlias {
    pub id: AliasId,
    pub client_model: String,
    pub context_window: u64,
    pub auto_compact_window: u64,
    pub strict_context: bool,
    pub candidates: Vec<ModelCandidate>,
}

pub struct ModelCandidate {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub credential_id: Option<CredentialId>,
    pub priority: u32,
}

pub struct AttemptTarget {
    pub provider_id: ProviderId,
    pub upstream_model_id: String,
    pub credential_id: Option<CredentialId>,
    pub protocol: Protocol,
}
```

The model alias is a route policy, not an upstream model. A candidate is eligible only when its live or explicitly configured `context_window` meets the alias requirement.

## Credential storage

The main TOML file stores only credential references:

```toml
[credentials.bai_main]
provider = "bai"
source = "keychain"
key_name = "bai_main"

[credentials.kilo_anonymous]
provider = "kilo"
source = "anonymous"
```

Resolution order:

1. Environment variable reference.
2. OS keychain implementation when available.
3. Atomic secure file fallback at `$XDG_DATA_HOME/opencode2api/credentials.json` with mode `0600`.

The existing `api_key` module remains responsible for client authentication tokens. Upstream provider credentials use a separate provider credential store so the two concepts cannot be mixed.

## Configuration

```toml
schema_version = 2

[router]
active_alias = "free-1m"

[providers.kilo]
display_name = "Kilo Gateway"
kind = "kilo_gateway"
base_url = "https://api.kilo.ai/api/gateway"
enabled = true

[providers.opencode]
display_name = "OpenCode Zen"
kind = "opencode_zen"
base_url = "https://opencode.ai/zen/v1"
enabled = true

[providers.bai]
display_name = "B.AI"
kind = "bai"
base_url = "https://api.b.ai/v1"
enabled = true

[aliases.free-1m]
client_model = "sonnet[1m]"
context_window = 1000000
auto_compact_window = 800000
strict_context = true

[[aliases.free-1m.candidates]]
provider = "bai"
model = "deepseek-1m"
credential = "bai_main"
priority = 10

[[aliases.free-1m.candidates]]
provider = "bai"
model = "glm-5.3-flash-1m"
credential = "bai_main"
priority = 20

[[aliases.free-1m.candidates]]
provider = "kilo"
model = "kilo-auto/free"
credential = "kilo_anonymous"
priority = 30
```

The exact upstream model IDs must come from the provider's `/models` response or an explicit operator override. A name containing `1m` is not sufficient evidence of a 1M context window.

## Request and fallback logic

```text
incoming Claude Messages request
  -> read client model alias
  -> resolve ModelAlias
  -> estimate input tokens and required output budget
  -> filter candidates by context and protocol capabilities
  -> skip disabled/unhealthy providers
  -> select candidate by priority and credential rotation
  -> adapt request and auth for provider
  -> send upstream request
  -> normalize response back to Anthropic format
```

For an alias with `strict_context = true`:

```text
candidate.context_window < 1_000_000 -> ineligible
no eligible candidate              -> explicit compatibility error
```

For streaming, fallback is allowed only before the first content event. After content is emitted, the response cannot be safely replayed.

Error policy is provider-aware:

- `401`/`403`: quarantine credential or provider according to adapter classification.
- `404`/unsupported model: disable that candidate until catalog refresh.
- `429`: apply provider retry-after and try another eligible candidate when policy permits.
- `402`: do not silently switch a paid request to a different billing policy.
- `5xx`/transport failure: try the next eligible candidate.
- context-length error: try only a candidate whose declared context satisfies the request.

## Protocol adapters

```rust
pub trait ProviderAdapter: Send + Sync {
    fn build_endpoint(&self, provider: &Provider, protocol: Protocol) -> anyhow::Result<String>;
    fn apply_auth(&self, request: reqwest::RequestBuilder, credential: Option<&Credential>)
        -> anyhow::Result<reqwest::RequestBuilder>;
    fn encode_request(&self, request: NormalizedRequest, target: &AttemptTarget)
        -> anyhow::Result<UpstreamRequest>;
    fn decode_response(&self, response: UpstreamResponse)
        -> anyhow::Result<NormalizedResponse>;
    fn classify_error(&self, response: &UpstreamResponse) -> RetryDecision;
}
```

Provider-specific headers, endpoint paths, model ID rewriting, and unsupported parameter removal belong in adapters. The retry engine must not assume every provider uses Bearer authentication or `/chat/completions`.

## Runtime state

The active alias and compiled candidate list are held in an immutable runtime snapshot. A configuration update either atomically replaces the snapshot or returns `restart_required`; it must never leave the CLI configuration and daemon configuration disagreeing.

The status response exposes:

```json
{
  "configured_alias": "free-1m",
  "configured_client_model": "sonnet[1m]",
  "runtime_alias": "free-1m",
  "runtime_context_window": 1000000,
  "restart_required": false,
  "active_candidate": {
    "provider": "bai",
    "model": "deepseek-1m"
  }
}
```

## Compatibility migration

Legacy fields are converted into one synthetic provider and one synthetic alias:

```text
upstream_base_url + upstream_api_key + model
  -> provider legacy-upstream
  -> alias legacy-model
  -> one ModelCandidate
```

Existing `provider api`, `provider opencode`, `upstream set`, and `upstream reset` commands remain compatibility aliases until the new provider commands are stable.

## Acceptance criteria

1. Claude Code starts with the configured client compatibility alias.
2. `/context` reports a 1M window for the 1M alias on the supported Claude Code version.
3. A request larger than 200K tokens reaches a verified 1M upstream candidate.
4. A failing 1M candidate falls back only to another compatible 1M candidate.
5. A 128K candidate is never selected for the strict 1M alias.
6. Provider credentials are never sent to a different provider.
7. Client-facing model identity and upstream effective model are both recorded.
8. CLI, dashboard, management API, launcher, and runtime share one provider registry.
9. Legacy configuration continues to work during migration.
10. Two independent fixture servers prove endpoint, auth header, model ID, streaming, and fallback behavior.
