# 9router Provider Architecture Analysis

**Scope:** Provider Registry, Executor Dispatcher, and Error Normalization in `/home/light/GitHub/9router/open-sse/`.
**Sources inspected:** `providers/schema.js`, `providers/index.js`, `providers/shared.js`, `providers/registry/index.js`, `providers/registry/{anthropic,claude,cline,deepseek,gemini,opencode}.js`, `executors/{base,default,index}.js`, `executors/{codex,gemini-cli,zed,commandcode,grok-cli,antigravity}.js` (error/retry overrides), `handlers/chatCore.js`, `utils/error.js`, `config/errorConfig.js`, `config/runtimeConfig.js`.

> **Naming note:** there is no `providers/registry/google.js`. The Google LLM provider is `gemini.js`
> (`id: "gemini"`); `google-pse.js` and `google-tts.js` are media-only (search / TTS) providers.

---

## 1. High-Level Data Flow

```
client request (chatCore.handleChat)
  │
  ├─ translate body (sourceFormat → targetFormat)
  ├─ resolve credentials (OAuth / API key / noAuth) + proxyOptions
  │
  ├─► getExecutor(provider)                  executors/index.js  (dispatcher)
  │      ├─ specialized executor?  → static instance from `executors` map
  │      └─ else                   → cached DefaultExecutor(provider)
  │
  ├─► executor.execute({model, body, stream, credentials, signal, log, proxyOptions})
  │      executors/base.js
  │      ├─ buildUrl()          per-URL fallback loop (urlIndex)
  │      ├─ transformRequest()  provider-specific body rewrite
  │      ├─ buildHeaders()      auth descriptors + header hooks
  │      ├─ proxyAwareFetch()   + connect-timeout AbortController
  │      ├─ tryRetry()          status-keyed retry table (429/502/503/504)
  │      └─ shouldRetry()       status-based fallback to next base URL
  │
  ├─ 401/403 → executor.refreshCredentials() → re-execute (max 3, rotating RT)
  │
  ├─ !response.ok → parseUpstreamError(response, executor)   utils/error.js
  │      ├─ executor.parseError() override (provider-specific fields)
  │      └─ generic JSON extraction (error.message | message | error | raw text)
  │
  └─ createErrorResult(statusCode, message, resetsAtMs)      → client-facing
         OpenAI-shaped error body (ERROR_TYPES) + account-cooldown metadata
         (errorConfig ERROR_RULES: text/status → cooldown or exponential backoff)
```

Key invariant: **the executor returns the raw upstream `Response`, not a parsed result.**
Error interpretation is deferred to a second pass (`parseUpstreamError`) that re-enters the
executor through its `parseError` hook. This keeps transport (retry/fallback) and error
semantics (message extraction, cooldown hints) cleanly separated.

---

## 2. Provider Registry

### 2.1 Layered structure

| Layer | File | Role |
|---|---|---|
| Entry files | `providers/registry/{id}.js` | One declarative default-exported object per provider (~120 files) |
| Aggregator | `providers/registry/index.js` | **Auto-generated** static import list + default-exported array. Deliberate omissions are commented out (e.g. `trae` — no tool calling, `devin-cli` — spawns local shell agent, `windsurf`) |
| Compiler | `providers/index.js` | Folds the array into four runtime tables: `PROVIDERS`, `PROVIDER_MODELS`, `PROVIDER_OAUTH`, `PROVIDER_MEDIA` |
| Schema | `providers/schema.js` | Documents the `RegistryEntry` typedef; `PROVIDER_DEFAULTS`, `ENDPOINT_DEFAULTS`, `resolveProvider()` |
| Shared constants | `providers/shared.js` | `ANTHROPIC_API_VERSION`, `CLAUDE_API_HEADERS`, `CLAUDE_CLI_SPOOF_HEADERS`, `selectAnthropicBeta()`, compat base URLs, OAuth client ids |

`providers/index.js` build rules:

- `entry.transport` → `PROVIDERS[id]`, after `buildTransport()`:
  - `format` defaults to `"openai"` (from `PROVIDER_DEFAULTS.format`);
  - `oauth.clientId/clientSecret/tokenUrl` are **injected into transport** (`OAUTH_INJECT_FIELDS`)
    so executors reading `this.config.{clientId,clientSecret,tokenUrl}` keep working with a
    single declaration site.
- `entry.transports` (multi-endpoint) is attached verbatim onto `PROVIDERS[id].transports`.
- `entry.models` → `PROVIDER_MODELS[alias || id]` through `normalizeModel()`; a missing
  `models` key means "no model list", `models: []` means "explicitly empty".
- Top-level media fields (`serviceKinds`, `ttsConfig`, `sttConfig`, `embeddingConfig`,
  `imageConfig`, `searchViaChat`, `modelsFetcher`, …) plus legacy `entry.media` are merged
  into `PROVIDER_MEDIA[id]`.
- TTS tables (`buildTtsProviderModels()`) are appended afterwards, keyed by special names.

### 2.2 The RegistryEntry contract

Only `id` + `category` are strictly required (`schema.js` typedef). Observed fields:

| Field | Purpose | Seen in |
|---|---|---|
| `id`, `priority` | Unique id (kebab-case) + ordering (10=claude … 110=deepseek) | all |
| `alias`, `aliases`, `uiAlias` | Lookup tokens / UI badge text (`cl`, `oc`, `ds`, `cc`) | cline, deepseek, claude |
| `category` | UI grouping: `apikey` / `oauth` / `freeTier` / `free` | all |
| `authType`, `authModes`, `hasOAuth`, `noAuth`, `hasFree` | Credential-mode hints | gemini, opencode |
| `display` | `{name, icon, color, textIcon, website, notice{apiKeyUrl,signupUrl}, deprecated, deprecationNotice}` | claude (`deprecated: true, "RISK_NOTICE"`) |
| `transport` | Runtime HTTP config (see §2.3) | all |
| `transports[]` | **Multi-endpoint**: per-`format` transport, chosen at runtime by client `sourceFormat` to skip translation | deepseek (openai + claude endpoints) |
| `oauth` | Flow config: `clientId`, `authorizeUrl`, `tokenUrl`, `scopes`, `codeChallengeMethod`, `refresh{encoding,scope}`, `refreshLeadMs` | claude, cline |
| `models[]` | `{id, name, upstreamModelId?, targetFormat?, kind?, params?, dimensions?}` | deepseek (`upstreamModelId` aliasing), opencode (per-model `targetFormat`), gemini (`kind: embedding/image/stt/tts`) |
| `features` | `{usage, usageApikey}` | deepseek, claude |
| `serviceKinds` | Non-LLM capabilities | anthropic `['llm','imageToText']`, gemini (7 kinds) |
| `modelsFetcher` | Dynamic model list `{url, type}` | opencode |
| `passthroughModels` | Forward client model id untouched | opencode |
| `quirks` | Per-provider behavior flags (`cloakToolsOnOAuth`, `dropClientMetadata`) | claude |
| `reasoningInject` | `{scope:'all'}` — surface upstream `reasoning_content` | deepseek |
| `validateUrl` | API-key validation endpoint | deepseek |
| `usage` | `{oauthUrl, orgUrl, settingsUrl}` quota endpoints | claude |

### 2.3 `transport` anatomy

Observed shape (superset; everything except `baseUrl` is optional):

- `baseUrl` — full endpoint URL (deepseek style) OR base path (gemini: `.../models`, model path built per-request)
- `format` — `openai` | `claude` | `gemini`; drives endpoint defaults + translator target
- `headers` — static headers merged over `Content-Type: application/json`
- `auth` — one of:
  - `{ combined:true, header, scheme, hooks[] }` — cline: one header for both credential types
  - `{ apiKey:{header,scheme}, oauth:{header,scheme} }` — claude/gemini: split by credential type
- `urlSuffix` — e.g. `?beta=true` (claude)
- `quirks` — behavioral flags consumed by executors
- `retry` / `timeoutMs` / `forceStream` / `passthroughModels` — defaults declared in `PROVIDER_DEFAULTS`
- `tokenUrl` / `refreshUrl` / `clientId` / `clientSecret` — token refresh (`clientId*` injected from `oauth`)
- `validateUrl` / `usage` / `reasoningInject` — provider extras

`schema.js` also defines `ENDPOINT_DEFAULTS` per format (`openai`, `claude`, `gemini` — the
gemini one templating `/{model}:streamGenerateContent`) and `resolveProvider()`, a deep-merge of
an entry over `PROVIDER_DEFAULTS`. **However, `resolveProvider()` is not wired into the runtime** —
the file itself admits: "runtime (index.js buildTransport) only re-applies `format`; the rest
documents the contract and feeds the (currently unwired) resolveProvider()". Of the shared
defaults, only `format:"openai"` is actually enforced at build time.

### 2.4 Case studies

**`anthropic.js`** — the plainest API-key entry: `format:"claude"`, static
`anthropic-version` + `Anthropic-Beta` headers pinned to an older beta set (notably *without*
`claude-code-20250219`, unlike the OAuth `claude` entry), no `auth` block (falls back to the
executor's `XAPIKEY` descriptor with `anthropicVersion`), `serviceKinds:['llm','imageToText']`.

**`claude.js`** (Claude Code OAuth, `alias:'cc'`) — the richest claude-format entry:

- Full Claude CLI fingerprint spoof (`X-Stainless-*`, `User-Agent: claude-cli/2.1.92`) inlined
  in `transport.headers`, duplicating `CLAUDE_CLI_SPOOF_HEADERS` from `shared.js`;
- `urlSuffix:'?beta=true'`, split auth (`x-api-key` raw / `Authorization: Bearer`);
- `quirks.cloakToolsOnOAuth` — hides tool definitions on OAuth credentials;
- complete OAuth block: `S256` PKCE, `refresh:{encoding:'json'}`, `refreshLeadMs: 14400000` (4 h), 3 scopes;
- `display.deprecated: true` with `RISK_NOTICE`;
- `usage` endpoints for quota display.

**`cline.js`** — OAuth provider behind an OpenAI-compatible gateway:
`transport.auth = { combined:true, header:'Authorization', scheme:'bearer', hooks:['clineHeaders'] }`.
The `hooks` array is the registry's extension point for non-auth header quirks: the executor's
`HEADER_HOOKS.clineHeaders` injects Cline-specific headers derived from the token. Token +
refresh URLs are declared twice (in `transport` for the executor and in `oauth` for the flow).

**`deepseek.js`** — the multi-endpoint showcase. `transports[]` declares two parallel
transports: `format:'openai'` → `/chat/completions` with `Authorization: Bearer`, and
`format:'claude'` → `/anthropic/v1/messages` with `x-api-key` + `CLAUDE_API_HEADERS`. At runtime
the client's `sourceFormat` selects the matching transport (surfaced to the executor as
`credentials.runtimeTransport`), so native clients skip translation entirely. Also demonstrates
`upstreamModelId` (three catalog SKUs mapping to `deepseek-v4-pro`) and
`reasoningInject:{scope:'all'}`.

**`gemini.js`** (the Google entry) — `category:'freeTier'`, `format:'gemini'` with
`baseUrl:.../v1beta/models`; the executor appends `/{model}:streamGenerateContent?alt=sse` or
`:generateContent`. Split auth: API key → `x-goog-api-key` (raw), OAuth → `Authorization: Bearer`.
Public Google CLI OAuth client credentials are inlined in `transport` (and also exported from
`shared.js` as `GOOGLE_OAUTH_CLIENT` — a duplication point). The models list is heterogeneous
(`kind: embedding|image|stt|tts`) and the entry carries a full media stack (7 `serviceKinds`,
`tts/stt/embedding/imageConfig`, `searchViaChat` with free-tier copy).

**`opencode.js`** — the no-auth/free provider: `noAuth:true` in both entry and transport,
`passthroughModels:true`, dynamic `modelsFetcher` (`opencode-free` type), and a single static
model with a **per-model** `targetFormat:'openai-responses'` — an inline comment notes the
format is declared per-model rather than per-provider because only that one model is served by
`/zen/v1/responses`.

### 2.5 Registry observations

1. **Two-to-three declaration dialects coexist.** `auth.combined` (cline/deepseek-openai) vs
   `auth.{apiKey,oauth}` (claude/gemini) vs *no* `auth` (anthropic, opencode) — three shapes the
   executor must interpret, plus the `hooks` overlay. The `auth` part of the `schema.js` typedef
   (`{header, scheme, source}`) is stale relative to what real entries use.
2. **`resolveProvider()` / `PROVIDER_DEFAULTS` are aspirational.** Only `format` is enforced by
   `buildTransport`; the `retry`/`timeoutMs`/`forceStream` defaults never reach `PROVIDERS`
   (the executor re-merges retry itself from `DEFAULT_RETRY_CONFIG`).
3. **Duplication hotspots:** claude's CLI fingerprint headers (entry vs `shared.js`), Gemini
   OAuth client id/secret (entry vs `shared.js`), cline's token/refresh URLs (`transport` vs
   `oauth`). `OAUTH_INJECT_FIELDS` fixes this only for `clientId/clientSecret/tokenUrl`.
4. **Executor map outlives registry entries.** `executors/index.js` still instantiates
   `devin-cli`, `trae`, `windsurf` executors while their registry entries are commented out of
   the aggregator — those executors are reachable only if stored credentials reference those ids.

---

## 3. Executor Dispatcher

### 3.1 Dispatch table (`executors/index.js`)

A module-level `executors` map holds **pre-instantiated singletons** for ~26 specialized
executors (antigravity, azure, gemini-cli, github, iflow, qoder, kiro, kimchi, codex, cursor,
vertex (+`vertex-partner`), opencode, grok-web, grok-cli, perplexity-web, ollama-local,
commandcode, xiaomi-tokenplan, mimo-free, codebuddy-cn/-intl, trae, zed, windsurf, devin-cli)
plus **alias keys** pointing at the same instances (`cu` → cursor, `gcli`/`gb` → grok-cli,
`mmf` → mimo-free).

```js
export function getExecutor(provider) {
  if (executors[provider]) return executors[provider];
  if (!defaultCache.has(provider)) defaultCache.set(provider, new DefaultExecutor(provider));
  return defaultCache.get(provider);
}
export function hasSpecializedExecutor(provider) { return !!executors[provider]; }
```

Design consequences:

- **Open/closed by default:** any new registry id without a specialized executor works
  immediately through `DefaultExecutor` (config-driven from `PROVIDERS[provider]`), with a
  per-provider `defaultCache` (unbounded Map — one instance per distinct provider id).
- **Specialized executors that ignore their provider id** (constructed with no args, e.g.
  `new AntigravityExecutor()`) are pinned to one registry id; `VertexExecutor` demonstrates the
  parameterized pattern: `new VertexExecutor('vertex')` / `new VertexExecutor('vertex-partner')`.
- `DefaultExecutor` falls back to `PROVIDERS.openai` when `PROVIDERS[provider]` is missing —
  dynamic `openai-compatible-*` / `anthropic-compatible-*` providers intentionally have no
  registry entry and are handled via URL-building branches keyed on the id prefix.
- Dispatch happens once per chat request in `handlers/chatCore.js` (`const executor = getExecutor(provider)`);
  the same instance is reused for the credential-refresh retry and is passed into
  `parseUpstreamError` for the error-parsing hook. Other call sites: `embeddingsCore`,
  `imageGenerationCore`, `imageProviders/antigravity`.

### 3.2 The executor contract (`executors/base.js`, `BaseExecutor`)

Constructor: `(provider, config)` where `config` is the provider transport from `PROVIDERS`.
Overridable hooks and their default behavior:

| Hook | Default | Overridden by |
|---|---|---|
| `getBaseUrls()` | `config.baseUrls` or `[config.baseUrl]` | multi-URL providers |
| `getFallbackCount()` | base-URL count (min 1) | — |
| `buildUrl(model, stream, urlIndex, credentials)` | base-URL fallback; `openai-compatible-*` → `{baseUrl}/chat/completions` or `/responses`; `anthropic-compatible-*` → `{baseUrl}/messages` | `DefaultExecutor` (runtimeTransport, gemini format, urlSuffix, `{accountId}` templating), codex, cursor, zed, … |
| `buildHeaders(credentials, stream, url, model)` | Content-Type + config headers; Bearer auth (accessToken or apiKey); `x-api-key` + `anthropic-version` for `anthropic-compatible-*`; `Accept: text/event-stream` when streaming | `DefaultExecutor` (descriptor-driven), grok-cli |
| `transformRequest(model, body, stream, credentials)` | identity | nearly every specialized executor |
| `shouldRetry(status, urlIndex)` | retry next base URL only on **429** | — |
| `refreshCredentials(credentials, log, proxyOptions)` | `null` (no refresh) | `DefaultExecutor` + most OAuth executors |
| `needsRefresh(credentials)` | `shouldRefreshCredentials(provider, credentials)` (oauthCredentialManager) | — |
| `parseError(response, bodyText)` | `{ status, message: bodyText || 'HTTP <status>' }` | codex, gemini-cli, zed, commandcode, grok-cli (see §4.3) |
| `computeRetryDelay(response, attempt, delayMs)` | absent | antigravity (Retry-After); returning `false` vetoes the retry, a number overrides the delay |

### 3.3 The `execute()` loop

`execute({model, body, stream, credentials, signal, log, proxyOptions})` runs a fallback loop
over `urlIndex ∈ [0, fallbackCount)`:

1. `retryConfig = { ...DEFAULT_RETRY_CONFIG, ...this.config.retry }` — status-keyed:
   `429: attempts 0`, `502: 3×3s`, `503: 3×2s`, `504: 2×3s`; `resolveRetryEntry()` normalizes
   numbers / objects / null entries.
2. Per-URL attempt bookkeeping in `retryAttemptsByUrl` (each URL gets its own budget).
3. **Connect timeout:** an internal `AbortController` (default `FETCH_CONNECT_TIMEOUT_MS = 60s`)
   merged with the client signal via `AbortSignal.any`. A connect-timeout abort is *converted*
   into a retryable network error (mapped to the `502` retry key); a client abort is rethrown as-is.
4. On response:
   - `tryRetry(urlIndex, response.status, …, response)` — retry same URL if the status key is
     configured and budget remains (honors `computeRetryDelay` for dynamic backoff);
   - else `this.shouldRetry(...)` — move to the next base URL on 429;
   - else return `{ response, url, headers, transformedBody }` (optionally with
     `responseFormat` from wrappers like cursor).
5. On thrown fetch error: map to `HTTP_STATUS.BAD_GATEWAY` retry key; if no budget/URLs remain,
   rethrow. After exhausting all URLs: `throw lastError || new Error('All N URLs failed…')`.

Note the asymmetry: **streaming bodies are never consumed inside `execute()`** — errors and
success both surface as `Response` objects; SSE parsing belongs to the handlers.

### 3.4 `DefaultExecutor` — the config-driven workhorse

The largest part of the system's per-provider logic lives here as *data*, not code:

- **`AUTH_DESCRIPTORS`** — derived at module load from `PROVIDERS[id].auth`, so registry entries
  declaratively define auth. `applyAuth()` implements the two dialects: `combined` always sets
  the header (legacy quirk: even `"Bearer undefined"` for `noAuth`), while split descriptors set
  only the matching branch. `anthropicVersion: true` on a descriptor injects
  `anthropic-version: 2023-06-01` if absent.
- **`resolveAuthDescriptor()` fallback chain:** `anthropic-compatible-*` → split x-api-key/Bearer
  + anthropicVersion; `format === 'claude'` → `XAPIKEY`; otherwise `BEARER`.
- **`HEADER_HOOKS`** — small named overlays invoked before auth so they can't clobber the token:
  `kimiHeaders` (stable device_id from `providerSpecificData.deviceId`), `clineHeaders`,
  `kilocodeOrg` (`X-Kilocode-OrganizationID`). Referenced by name from registry `auth.hooks`.
- **`buildUrl()` priority order:** `credentials.runtimeTransport` (deepseek multi-endpoint) →
  `openai-compatible-*` (api type responses vs chat/completions) → `anthropic-compatible-*` →
  `format === 'gemini'` (`{model}:streamGenerateContent?alt=sse` / `:generateContent`) →
  `config.urlSuffix` → `{accountId}` template substitution (throws when missing) → plain baseUrl.
- **`transformRequest()`** — `applyJsonSchemaFallback` (for `openai-compatible-*`: rewrite
  `response_format: json_schema` into a system-prompt + `json_object`), quirk
  `dropClientMetadata`, `stripUnsupportedParams(provider, model, body)`, then
  `injectReasoningContent` (deepseek `reasoningInject`).
- **claude-specific header massaging in `buildHeaders()`** — `Anthropic-Beta` recomputed per
  model via `selectAnthropicBeta(model)` (heavy-agent betas only for opus/sonnet); for
  `anthropic-compatible-*` third-party gateways it strips the Claude Code identity
  (`X-App`, browser-access header, `claude-code-20250219` beta flag) and sends *both*
  `x-api-key` and `Authorization: Bearer` for gateway compatibility.
- **OAuth refresh:** `REFRESH_GRANTS` derived from `PROVIDER_OAUTH[id].refresh` (encoding
  json/form, optional scope) powers `refreshFromGrant()` for claude/codex/gemini; bespoke
  refreshers cover iflow (Basic-auth form), kiro (JSON), cline/clinepass (JSON with
  `workos:` prefix normalization + ISO `expiresAt` → relative `expiresIn`), kimi
  (form + `X-Msh-*` device headers), kilocode (no refresh — device-code flow, returns null).
  All refreshers return `{accessToken, refreshToken, expiresIn}` or null; failures are caught
  and logged, never thrown.

### 3.5 Dispatcher observations

1. **Singletons with per-request credentials.** Executors are stateless singletons; all
   request state flows through the `execute()` argument bag — good for memory, but it means
   executors must never cache credentials on `this` (codex's `_isCompact` is a rare, benign
   per-request field set in `transformRequest`).
2. **Alias keys are hand-maintained** in the dispatcher map (`cu`, `gcli`, `gb`, `mmf`) — the
   registry's own `alias`/`aliases` fields are *not* consulted for executor dispatch; alias
   resolution to canonical provider ids must happen upstream (credential/service layer).
3. **Two-tier extension model:** add a provider = add a registry entry (zero code, Default
   path); add a provider *with custom transport semantics* = add a registry entry + specialized
   executor + dispatcher-map line. The specialized executors are large (kiro 54 KB, cursor 38 KB,
   devin-cli 34 KB) because they own SSE re-framing, not just HTTP.
4. **Retry policy is split across three layers** — `DEFAULT_RETRY_CONFIG` (global),
   `config.retry` (registry transport), and `shouldRetry`/`computeRetryDelay` (code). There is
   no single place to answer "how many times will this provider be retried, with what delays?".

---

## 4. Error Normalization

Error handling is a three-stage pipeline, each stage converting raw upstream reality into a
progressively more standardized shape.

### 4.1 Stage 1 — in-executor parsing (`parseError` overrides)

Base default (`base.js:96`): `{ status: response.status, message: bodyText || 'HTTP <n>' }` —
i.e. **raw body text as message, no JSON awareness**. Specialized executors override it to
extract provider-specific semantics while keeping the same return contract
(`{status, message, resetsAtMs?, retryAfter?, code?}`):

| Executor | What it extracts |
|---|---|
| `codex` | 429 `error.type === 'usage_limit_reached'` → `resetsAtMs` from `resets_at` (epoch sec) or `resets_in_seconds`; enables precise account cooldown instead of a guessed window |
| `gemini-cli` | 429 Google API body → `google.rpc.RetryInfo.retryDelay` surfaced as `retryAfter` |
| `zed` | `code === 'trial_blocked'` → rewrites into a human-readable billing-block message |
| `commandcode` | tolerates `error.code` / `error.statusCode` variants and re-bases status on them |
| `grok-cli` | 402 `personal-team-blocked:spending-limit` → structured `{status:402, code}` so downstream can route to fallback |
| `antigravity` | contributes the `computeRetryDelay` hook (Retry-After aware veto/override) instead of parseError |

All overrides degrade gracefully: on parse failure they fall through to `super.parseError` or
the generic path.

### 4.2 Stage 2 — `utils/error.js` (`parseUpstreamError`)

Called by `chatCore.js` when `!providerResponse.ok`:

```js
const { statusCode, message, resetsAtMs } = await parseUpstreamError(providerResponse, executor);
```

Resolution order:

1. Read body text (swallowing read errors).
2. If the executor has `parseError`, use it inside a try/catch; on success normalize to
   `{statusCode: parsed.status ?? response.status, message: parsed.message ?? default, resetsAtMs}`.
3. Otherwise generic JSON extraction: `json.error?.message || json.message || json.error || bodyText`
   (note: `json.error` as an object gets stringified).
4. Final fallbacks: `DEFAULT_ERROR_MESSAGES[status]` → `'Upstream error: <status>'`.

The same module owns the **client-facing shapes**:

- `buildErrorBody(status, message)` — OpenAI envelope `{error:{message, type, code}}` with
  type/code from `ERROR_TYPES[status]` (`400 invalid_request_error/bad_request`,
  `401 authentication_error/invalid_api_key`, `402 billing_error`, `403 permission_error`,
  `404 model_not_found`, `406 model_not_supported`, `429 rate_limit_error`,
  `5xx server_error/*`), defaulting `>=500` → `server_error` else `invalid_request_error`.
- `errorResponse()` (non-streaming `Response`) and `writeStreamError()` (writes the same
  envelope as an SSE `data:` frame) — one shape regardless of streaming mode.
- `createErrorResult(status, message, resetsAtMs)` — the chatCore contract:
  `{success:false, status, error, resetsAtMs, response}`; `resetsAtMs` carries the codex-style
  precise cooldown into the account-cooldown layer.
- `unavailableResponse(status, message, retryAfter, retryAfterHuman)` — all-accounts-limited
  response with a numeric `Retry-After` header.
- `formatProviderError(error, provider, model, status)` — `[<code>]: <message>` plus
  `error.cause.code/message` (UND_ERR_SOCKET, ECONNRESET, ETIMEDOUT) for diagnosability.

### 4.3 Stage 3 — classification & cooldown (`config/errorConfig.js`)

- `ERROR_RULES` — ordered rules, text substring matches first (case-insensitive), then status:
  - text → cooldown: `no credentials`, `improperly formed request` (2 min); `request not allowed` (5 s);
  - text → exponential backoff: `rate limit`, `too many requests`, `quota exceeded`, `capacity`, `overloaded`;
  - status → cooldown: 401/402/403/404 (2 min); status 429 → backoff.
- `BACKOFF_CONFIG` `{base: 2000, max: 5 min, maxLevel: 15}`, `TRANSIENT_COOLDOWN_MS = 30 s`,
  `MAX_RATE_LIMIT_COOLDOWN_MS = 30 min` (caps codex's 5–6 h `resets_at`).
- `COOLDOWN_MS` kept as a backward-compat re-export for `index.js`.

### 4.4 The 401/403 refresh-retry path (chatCore)

Before error classification, a non-ok 401/403 triggers (once):

1. `refreshWithRetry(() => executor.refreshCredentials(credentials, log), 3, log)` — with
   **rotating-refresh-token mutation**: when a refresher returns a new RT, `credentials` is
   mutated in place so attempts 2/3 don't reuse the consumed token (xAI/grok-cli invalidate the
   old RT on every refresh).
2. On success: `Object.assign(credentials, newCredentials)`, optional `onCredentialsRefreshed`
   persistence hook, then a single `executor.execute(...)` retry (only adopted if `response.ok`).
3. All failures degrade to normal error classification.

### 4.5 Error-normalization observations

1. **`parseError` is dual-purpose and slightly confused.** Its two consumers need different
   things: `parseUpstreamError` wants `{status, message, resetsAtMs}`, while `base.execute()`'s
   internal retry loop keys off raw `response.status` and never calls `parseError`. Providers
   whose error *body* reports a different status than the HTTP status (commandcode) can
   surface a `statusCode` that diverges from the actual response.
2. **Silent catch-all everywhere.** Every parse override and both branches of
   `parseUpstreamError` swallow exceptions by design (resilience), but this also hides
   malformed upstream error payloads — debugging relies entirely on `dbg('FETCH')` lines.
3. **Two message-quality tiers.** Specialized executors produce rich messages (zed's trial
   explanation, codex's quota message); everything on the Default path can be a raw JSON blob
   or even raw HTML body text passed straight through `formatProviderError` to the client.
4. **`json.error || bodyText` edge case:** when `error` is an object without `.message`, the
   client receives stringified JSON rather than a curated message.
5. **Cooldown knowledge lives client-side of the executor.** `resetsAtMs`/`retryAfter` from
   `parseError` flow into `createErrorResult`, but text-rule classification
   (`ERROR_RULES`) re-derives semantics from message substrings — a provider rewording an
   error message silently changes cooldown behavior.
6. **No unified error type.** Three different result shapes coexist:
   executor-internal `{status,message}`, `parseUpstreamError`'s `{statusCode,message,resetsAtMs}`,
   and `createErrorResult`'s `{success,status,error,resetsAtMs,response}` — all mapped by
   convention, not by a shared type or validation.

---

## 5. Summary Assessment

**Strengths**

- Registry-as-data keeps ~120 providers describable without code; the Default path makes
  plain OpenAI/claude/gemini-format providers nearly free to add.
- The dispatcher's specialized-override + Default-fallback design gives a clean on-ramp from
  config-only providers to fully custom SSE-handling executors.
- Retry/fallback/timeout in one audited loop (`BaseExecutor.execute`), with per-URL budgets,
  connect-timeout conversion, and an escape hatch for dynamic delay policies.
- Error parsing is pluggable per provider but funnels into a single OpenAI-compatible envelope,
  so clients only ever see one error shape.

**Risks / suggested follow-ups**

1. Wire (or delete) `resolveProvider()` / update `schema.js`'s stale `auth` typedef so the
   documented contract matches the three dialects actually in use.
2. Consolidate duplicated fingerprints (claude CLI headers, Google OAuth client) via the
   existing `shared.js` constants; extend `OAUTH_INJECT_FIELDS` to `refreshUrl`.
3. Give `parseError` a formal result type and let the execute loop consult it, so status
   divergence (commandcode) and provider-defined cooldowns are handled in one place.
4. Consider a structured error classification keyed on typed fields (status + parsed code)
   rather than message-substring matching in `ERROR_RULES`.
5. Prune dispatcher entries whose registry entries are hidden (`trae`, `windsurf`, `devin-cli`)
   or document that they are intentionally kept for legacy stored credentials.
