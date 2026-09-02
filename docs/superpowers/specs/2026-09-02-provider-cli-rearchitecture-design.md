# Provider Runtime and CLI Rearchitecture Design

## Status

Approved design input: the CLI presentation may take visual cues from
`auto_download_ctf_challenge`, but its command vocabulary, hierarchy, domain,
and interactive flows are not reused. This document defines OpenCode2API's own
provider-management control plane.

## Goal

Make OpenCode2API a maintainable local multi-provider gateway. Operators must
be able to add an OpenAI-compatible provider without changing Rust code,
connect credentials without exposing their values, expose stable Claude Code
aliases such as `sonnet[1m]`, understand every routing decision, and recover
from a provider or credential rate limit by selecting an independently
available candidate.

The result remains a local HTTP gateway. It does not spawn, automate, or
manage OpenCode, Kilo Code, Claude Code, or any other upstream CLI.

## Problems Being Solved

The current schema-v2 registry establishes the right core identities but is an
add-on beside the legacy singleton retry loop. Provider commands also combine
argument handling, TOML mutation, secret handling, and rendering in
`src/app/providers.rs`. The legacy retry loop can move among model IDs that
share the same endpoint and quota, so a model fallback is not necessarily a
provider failover. Finally, local `.env` values can make a CLI status view
different from the daemon's loaded runtime configuration.

## Non-Goals

- No full-screen TUI.
- No cloud sync, billing platform, OAuth implementation, or quota prediction.
- No automatic claim that a model has a 1M context window from its name.
- No provider-specific Rust module for a normal OpenAI-compatible endpoint.
- No deletion of legacy commands before a compatible replacement is verified.
- No automatic replay of a streaming request after content was emitted.

## Product Principles

1. **One stable client identity, many upstream targets.** An alias is a
   client-facing route contract; it is never an upstream model name.
2. **Configuration is declarative; credentials are separate.** TOML stores
   references and policy, while a secure local file or environment variable
   holds secret bytes.
3. **Route by independent failure domain.** A 429 first rotates credentials
   within a provider, then chooses the next eligible provider candidate rather
   than presenting a same-provider model switch as guaranteed recovery.
4. **Runtime truth comes from the daemon.** The daemon exposes a safe runtime
   snapshot. CLI commands distinguish file configuration, effective CLI
   configuration, and daemon state.
5. **Human output is readable; automation output is contractual.** Every
   management command supports `human`, `quiet`, and JSON output without ANSI
   or prose in JSON/quiet modes.
6. **Generic first.** A new HTTP API using OpenAI Chat Completions is added by
   configuration. An adapter exists only for an endpoint, authentication, or
   wire-format difference.
7. **Compaction is a route invariant.** Every alias with a known context
   window derives its compaction boundary as `floor(context_window * 0.80)`.
   It is not a provider-specific or per-alias tuning knob, so a fallback never
   changes the operator's compaction policy.

## Operator Model

```text
provider  ── owns endpoint, protocol, headers, enablement
credential ─ owns secret reference and authentication scheme
model     ── owns provider-local wire ID and verified capabilities
alias     ── owns client model identity and ordered candidate policy
candidate ── binds provider + model + credential preference
runtime   ── owns a compiled immutable snapshot and transient health state
```

Provider model IDs are keyed by `(provider_id, model_id)`. Identical strings
from two providers are deliberately distinct models.

## CLI Information Architecture

The maximum command depth is two levels. Command groups use nouns; operations
inside a group use a small, repeated verb set.

```text
opencode2api
├── provider    list | show | add | remove | enable | disable | test
├── credential  list | set | remove | test
├── model       list | show | discover | verify
├── alias       list | show | set | remove | use
├── route       explain | test | simulate
├── health      [PROVIDER] | watch
├── config      path | show | validate | migrate
├── server      start | stop | status | restart | logs
├── proxy       ps | restart | purge | logs
├── dashboard   start | status
├── doctor
├── env
├── shell
├── api-key
├── completion
├── init
└── update
```

The global flags are only flags that are meaningful before command execution:

```text
--config PATH
--json
--quiet
--color auto|always|never
--verbose
```

`--config` selects the file transaction target for management mutations. It
does not secretly mean the daemon has reloaded. A mutation response reports
`restart_required` or `reloaded` explicitly.

### Common workflows

```bash
# Add any OpenAI-compatible API without Rust code.
opencode2api provider add moonshot \
  --base-url https://api.moonshot.example/v1 \
  --protocol chat --auth bearer
opencode2api credential set moonshot primary --env MOONSHOT_API_KEY
opencode2api model discover moonshot

# Create a stable Claude Code alias with independent fallbacks.
opencode2api alias set free-1m --client-model 'sonnet[1m]' \
  --context-window 1000000 --strict-context \
  --candidate bai:deepseek-v4-flash:bai-main:10 \
  --candidate kilo:free-model:kilo-free:20
opencode2api alias use free-1m
opencode2api route explain free-1m

# Debug a rate limit without sending a real request.
opencode2api route simulate free-1m --from bai --status 429
opencode2api health bai
```

### Output contract

Human output uses a two-line brand header, a primary state line, aligned facts
or a borderless table, then one actionable hint. It borrows this presentation
discipline from the referenced CTF project only.

```text
◆ OpenCode2API
  Route explanation · free-1m

  ● eligible candidates

  Priority   Provider   Model                       Credential   Context
  10         bai        deepseek-v4-flash           bai-main     1,000,000
  20         kilo       free-model                  kilo-free    1,000,000

  › Run `opencode2api route test free-1m` to probe candidates.
```

Quiet output prints only the requested primary value. JSON output has a
versioned top-level object, uses stable field names, contains no color or
human hints, and never serializes credential values.

## Code Boundaries

The repository keeps the existing HTTP protocol and proxy subsystems. This
work introduces focused boundaries around provider management and moves only
code that belongs to those boundaries.

```text
src/
  cli/
    mod.rs                 command parser root and shared global arguments
    provider.rs            typed arguments for provider/model/alias commands
    server.rs              typed arguments for lifecycle commands
    output.rs              OutputFormat and serializable CLI envelopes
    render.rs              human-only headers, facts, tables, errors, hints

  command/
    mod.rs                 dispatcher from parsed command to a service
    provider.rs            provider configuration operations
    credential.rs          credential configuration operations
    model.rs               catalog and verification operations
    alias.rs               alias operations
    route.rs               explain/test/simulate operations
    health.rs              daemon and direct health operations
    config.rs              config inspection, validation, migration

  provider/
    domain.rs              IDs, Provider, Credential, Model, Alias, Capability
    registry.rs            compile and validate immutable route definitions
    config.rs              schema-v3 decoding and v1/v2 compatibility readers
    store.rs               atomic TOML read-modify-validate-write transaction
    credentials.rs         environment, file, and managed secret resolution
    catalog.rs             provider-local cached model metadata
    adapter.rs             ProviderAdapter interface and AdapterRegistry
    adapters/              generic OpenAI plus bounded special adapters
    routing.rs             candidate eligibility and route explanation
    resilience.rs          failure classification, cooldowns, selection policy
    runtime.rs             immutable snapshot plus transient route state

  gateway/
    provider_execute.rs    execute a compiled target and return a typed outcome
    provider_stream.rs     pre-first-event streaming retry boundary

  legacy/
    config.rs              singleton config to synthetic route compatibility
    commands.rs            hidden legacy command aliases and deprecation text
```

`src/app/providers.rs` is removed only after the `command/*` services own all
provider mutations. `src/opencode/retry/execute/mod.rs` retains the legacy
path during migration, but its schema-v3 branch delegates to
`gateway/provider_execute.rs`; it no longer owns provider selection policy.

## Schema Version 3

Schema v3 uses named TOML tables, so a provider can be located and changed
without scanning an array. Secrets remain outside the main document.

```toml
schema_version = 3

[router]
active_alias = "free-1m"

[providers.bai]
name = "B.AI"
kind = "openai-compatible"
base_url = "https://api.b.ai/v1"
protocol = "openai_chat_completions"
enabled = true

[credentials.bai-main]
provider = "bai"
source = "managed"
auth_scheme = "bearer"

[models.bai."deepseek-v4-flash"]
wire_model = "deepseek-v4-flash"
context_window = 1000000
max_output_tokens = 384000
verified_context = true
supports_streaming = true
supports_tools = true
free = true

[aliases.free-1m]
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.free-1m.candidates]]
provider = "bai"
model = "deepseek-v4-flash"
credential = "bai-main"
priority = 10

[[aliases.free-1m.candidates]]
provider = "kilo"
model = "free-model"
credential = "kilo-free"
priority = 20
```

`provider-secrets.json` lives beside the configured TOML and is created with
owner-only permissions. It maps managed credential IDs to values and is never
included in `config show`, logs, management DTOs, history, or error output.

`auto_compact_window` is compiled, not stored: every alias derives it as
`floor(context_window * 0.80)`. Therefore a 1M alias compacts at 800,000
tokens, a 200K alias at 160,000 tokens, and fallback candidates cannot change
the threshold. The loader rejects aliases without a positive context window;
legacy configuration receives a synthetic alias using its verified or
conservative configured context window before the same calculation.

The migration policy is conservative:

- v1 legacy singleton fields continue to work as an in-memory synthetic
  provider and alias.
- v2 array-of-table registry documents are read and compiled without loss.
- `config migrate --write` is the only command that rewrites a document to
  v3. It validates the converted registry before atomic replacement and keeps
  an adjacent timestamped backup.
- A future or malformed schema produces a typed error; it never falls back to
  OpenCode defaults.

## Configuration Truth and Reloading

There are three intentionally different views:

| View | Command/API | Meaning |
| --- | --- | --- |
| File | `config show --file` | Redacted document at a selected path. |
| Effective CLI | `config show --effective` | CLI flags, process environment, and file precedence. Every overridden field names its source. |
| Daemon runtime | `server status`, `health`, `GET /api/v1/provider-runtime` | The snapshot currently serving requests. |

The daemon records a fingerprint of its compiled provider snapshot. A config
write tries an authenticated reload endpoint. If reload validation succeeds,
the daemon atomically replaces the snapshot and returns `reloaded: true`.
If the daemon is unavailable or the new snapshot is invalid, the write remains
on disk and returns `restart_required: true` without altering the live
snapshot.

## Provider Adapter Contract

```rust
pub trait ProviderAdapter: Send + Sync {
    fn prepare(
        &self,
        provider: &Provider,
        target: &AttemptTarget,
        request: &NormalizedRequest,
        credential: Option<&SecretString>,
    ) -> Result<ProviderHttpRequest, AdapterError>;

    fn classify_failure(
        &self,
        status: Option<StatusCode>,
        headers: &HeaderMap,
        body: &str,
    ) -> FailureClass;
}
```

The generic adapter supports OpenAI Chat Completions with configurable
`Bearer`, `x-api-key`, or no authentication. B.AI, Kilo, and OpenCode select
that adapter unless their endpoint or payload actually differs. An
Anthropic/Responses adapter is added only when the provider requires that
protocol. The retry engine receives a `FailureClass`, never raw provider-kind
branches.

## Routing and Resilience Policy

Each request gets one immutable `ProviderRuntimeSnapshot`. It contains the
compiled registry and a read-only catalog. A separate synchronized
`RouteState` holds transient provider and credential health.

```text
resolve alias
  → filter disabled, unsupported, insufficient-context, cooling candidates
  → choose by priority
  → choose a non-cooling credential for that provider
  → send through adapter
  → classify result
  → update route state
  → rotate credential or advance candidate as policy requires
```

Failure actions are exact:

| Failure class | Action |
| --- | --- |
| 401/403 | Quarantine the credential. Try another credential, then next provider candidate. |
| 429 | Set credential cooldown from bounded `Retry-After`, then use another credential or the next provider candidate. |
| 404/model unsupported | Mark the model candidate unavailable until catalog refresh, then advance candidate. |
| 5xx/transport/timeout | Mark provider degraded for a bounded backoff and advance candidate. |
| context limit | Advance only to a candidate satisfying the alias/request context requirement. |
| 402 | Return a payment error; no silent billing-policy change. |
| malformed client request | Return the error; do not retry. |

When all candidates are cooling, the error includes the earliest retry time
without naming credential values. Cooldowns are in-memory only. Restarting
the daemon does not invent a quota reset or persist a potentially stale block.

For streaming, the first response content event commits the target. Retry and
candidate advance are allowed only before that event; after it, failures close
the stream with the correct client protocol error.

Strict aliases require verified context metadata for every candidate. Every
alias compacts at `floor(context_window * 0.80)`. A 1M alias additionally
exposes `sonnet[1m]` and exports a 1,000,000-token context setting to Claude
Code.

## Health, Debugging, and Observability

`provider test` probes a configured provider directly. `health` asks the
running daemon for the state it is actually using. Health state is scoped to a
provider or credential and is one of `unknown`, `healthy`, `degraded`,
`cooling_down`, or `unavailable`.

Every upstream attempt emits safe structured fields:

```text
request_id
alias_id
candidate_index
provider_id
model_id
credential_id_hash
attempt_action
failure_class
retry_after_ms
cooldown_until
stream_committed
```

No raw headers, prompt bodies, credential values, or provider response bodies
are included in normal operational logs. A route explanation can be recreated
from these fields and the redacted registry.

## Backward Compatibility

The old `provider api`, `provider opencode`, `provider models`, `upstream`,
and hidden lifecycle aliases remain during one release cycle. They call the
new services, print a deprecation hint in human mode, and retain their old
exit-code behavior. New automation is documented only with the new command
tree.

## Acceptance Criteria

1. A generic OpenAI-compatible provider is added with configuration and no
   new Rust adapter.
2. Credential values never appear in config, JSON output, logs, tests, or
   `Debug` output.
3. `route explain` shows all exclusions and the selected order without a live
   request.
4. A 429 rotates a provider credential then advances to a different eligible
   provider candidate before same-provider backoff retry.
5. A strict 1M alias never routes to missing, unverified, or sub-1M metadata.
6. Streaming fallback occurs before the first content event and never after.
7. `server status` reports the daemon snapshot, not a caller's `.env` view.
8. v1 and v2 configurations remain loadable; explicit v3 migration is atomic
   and reversible via its backup.
9. Human, quiet, and JSON output have regression tests for all new command
   groups.
10. Existing Anthropic Messages and OpenAI Chat compatibility tests remain
    green, together with Claude Code 1M E2E verification.
11. Every alias, including migrated legacy aliases, reports an auto-compaction
    threshold derived from exactly 80% of its context window.
