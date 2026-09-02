# Provider Capacity Scheduler Design

## Status

Proposed and approved for a single local OpenCode2API daemon. This document
extends the provider-runtime design; it does not replace alias, fallback,
credential storage, or the 1M-context contract.

## Goal

Prevent one upstream API credential, account, or provider quota from being
overloaded when several Claude Code or OpenAI-compatible requests arrive at
once. The gateway must atomically distribute work across independently
available credentials, respect configured request and token budgets, retain
safe provider fallback, and report why a request was delayed, routed, or
rejected.

For three independent credentials with `max_in_flight = 1`, three concurrent
requests must receive three distinct reservations whenever each credential has
capacity. A request must never be routed to a credential merely because it was
visible as free before another concurrent request reserved it.

## Scope

### In scope

- A process-local capacity scheduler for the existing single daemon.
- Credential pools that separate key distribution from provider/model fallback.
- Per-credential concurrency caps, request-per-minute (RPM) budgets, and
  optional token-per-minute (TPM) budgets.
- Explicit quota scopes for credentials that share an upstream account or IP
  quota.
- Atomic admission, weighted round-robin selection, cooldown awareness, and
  fail-fast bounded backpressure.
- `Retry-After` and OpenAI-style quota-header feedback when providers expose
  it.
- Secret-safe CLI, management API, diagnostics, metrics, and deterministic
  concurrency tests.

### Out of scope

- Redis, cloud synchronization, or capacity sharing among multiple daemon
  processes. A later distributed coordinator will implement the same admission
  interface.
- OAuth login, subscription scraping, or attempts to evade an account-, IP-,
  or provider-level limit by rotating keys or egress.
- A provider-specific tokenizer dependency. Token admission uses the gateway's
  documented heuristic and reconciles from upstream usage where available.
- Retrying a response after a streaming body has emitted content.
- Dynamic latency-based concurrency control in the first release. Long LLM
  streams make full-response latency an unreliable overload signal; it is a
  later opt-in feature based on time-to-first-byte and enough samples.

## Design Principles

1. **A route is semantic; a pool is operational.** An alias candidate chooses
   a provider and model with a context contract. A credential pool chooses the
   credential that can safely carry that request.
2. **Admission is atomic.** Selecting a key and consuming its temporary
   capacity happens under one scheduler lock and returns a lease. It is not a
   read-then-send race.
3. **Quota scope is explicit.** Multiple keys for one upstream account must
   use the same `quota_scope`; they share its RPM and TPM budget. Credentials
   default to an individual scope only when the operator does not declare a
   shared upstream quota.
4. **Protect interactive work first.** When a pool cannot admit a request, the
   router tries a valid fallback provider; if nothing can admit it, it returns
   a retryable overload response promptly instead of holding an unbounded
   in-memory queue.
5. **Do not silently weaken a route.** Strict 1M aliases still route only to
   verified 1M candidates. Capacity pressure cannot downgrade context or model
   capabilities.
6. **Secrets and raw prompts never become scheduler diagnostics.** Public
   state names a provider, credential ID, pool, and redacted quota scope only.

## Terms

| Term | Meaning |
| --- | --- |
| alias candidate | One provider + provider-local model fallback target owned by an alias. |
| credential pool | Named set of credentials for one provider, with a selection policy. |
| pool member | One credential and its independent capacity limits. |
| quota scope | A non-secret ID representing an upstream quota shared by one or more credentials. |
| admission | Atomic decision to reserve temporary capacity for one upstream attempt. |
| dispatch lease | RAII value that owns the reservation until the response completes, errors, or is dropped. |
| cooldown | Temporary in-memory exclusion after a classified upstream failure. |

## Operator Model

```text
Claude Code client model
          |
          v
alias: sonnet[1m] / free-1m
          |
          +-- priority 0: B.AI / deepseek-1m / pool bai-1m
          |                    |
          |                    +-- key-1 / quota account-a
          |                    +-- key-2 / quota account-b
          |                    +-- key-3 / quota account-c
          |
          +-- priority 10: OpenCode / deepseek-v4-flash-free / anonymous pool
```

The first layer decides semantic fallback. The second layer decides capacity
and never exposes an API-key value to routing, logs, JSON output, or metrics.

## Configuration

Schema version remains `3`. The new tables are optional and additive, so an
existing v3 candidate that directly names `credential` continues to work as an
implicit single-member pool. A candidate may set exactly one of `credential`
or `credential_pool`; anonymous providers set neither.

```toml
[credential_pools.bai-1m]
provider = "bai"
strategy = "weighted_round_robin" # round_robin | least_loaded | weighted_round_robin

[[credential_pools.bai-1m.members]]
credential = "bai-key-1"
quota_scope = "bai-account-a"
max_in_flight = 1
requests_per_minute = 20
tokens_per_minute = 200000
weight = 1

[[credential_pools.bai-1m.members]]
credential = "bai-key-2"
quota_scope = "bai-account-b"
max_in_flight = 1
requests_per_minute = 20
tokens_per_minute = 200000
weight = 1

[aliases.free-1m]
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.free-1m.candidates]]
provider = "bai"
model = "deepseek-1m"
credential_pool = "bai-1m"
priority = 0
```

### Field contract

| Field | Required | Rules |
| --- | --- | --- |
| `credential_pools.<id>.provider` | yes | Must name an existing provider. Every member credential must belong to it. |
| `strategy` | no | Defaults to `round_robin`; accepted values are `round_robin`, `least_loaded`, and `weighted_round_robin`. |
| `members[].credential` | yes | Must name an existing credential and may belong to only one explicit pool. |
| `members[].quota_scope` | no | Defaults to `credential:<credential-id>`; use the same explicit value for keys sharing one upstream quota. |
| `members[].max_in_flight` | no | Defaults to `1`; must be in `1..=1024`. |
| `members[].requests_per_minute` | no | `1..=10_000_000`; omitted means no local RPM token bucket. |
| `members[].tokens_per_minute` | no | `1..=10_000_000_000`; omitted means no local TPM token bucket. |
| `members[].weight` | no | Defaults to `1`; valid only for `weighted_round_robin`, in `1..=1000`. |
| candidate `credential_pool` | conditional | Mutually exclusive with candidate `credential`; its provider must equal the candidate provider. |

When two members declare the same `quota_scope`, RPM and TPM values must be
identical. This prevents one shared account quota from accidentally receiving
two contradictory local budgets. `max_in_flight` remains member-specific,
while the scope additionally enforces the sum of active reservations against
the smallest configured member cap unless an explicit future scope cap is
introduced.

The main configuration never stores a secret value. `credential` continues to
resolve through `env:`, `file:`, or `managed:` sources exactly as it does now.

## Scheduler Architecture

```text
RoutePlanner
  -> semantic targets sorted by alias priority
  -> CapacityScheduler::admit(targets, demand, now)
       -> remove disabled / context-ineligible / cooldown targets
       -> choose pool member with a successful atomic reservation
       -> return DispatchLease(provider, model, credential ID)
  -> ProviderAdapter prepares one HTTP request
  -> executor holds DispatchLease for the complete response lifecycle
  -> completion observes result, usage, headers, then releases/reconciles
```

### State ownership

`ProviderRuntimeHandle` owns an immutable runtime snapshot containing the
compiled providers, credentials, models, aliases, credential pools, and that
snapshot's `Arc<CapacityScheduler>`. `AppState` continues to own the runtime
handle, but does not own scheduler or cooldown state that can be replaced
underneath an active request.

`CapacityScheduler` owns a `Mutex<SchedulerState>`, including the existing
`RouteState` cooldown policy/state. The mutex protects selection cursors,
token buckets, cooldown reads and writes used for admission, current
in-flight counts, and outstanding reservation IDs. It is held only while
refilling budgets and issuing, completing, or rejecting a lease; it is never
held across DNS, HTTP, streaming, or secret I/O.

The existing global `BRIDGE_RATE_LIMIT` semaphore remains an outer bridge-wide
guard. It does not replace per-pool admission: a global limit of three is
allowed to dispatch all three requests to one key without this scheduler.

### Admission demand

Every request produces a `CapacityDemand` before selection:

```text
request_cost = 1
input_cost   = estimate_provider_request_tokens(messages)
output_cost  = requested max_tokens, otherwise target model max_output_tokens, otherwise 0
token_cost   = ceil((input_cost + output_cost) * 1.25)
```

`estimate_provider_request_tokens` reuses the existing content-aware gateway
estimator and is extended to walk OpenAI-compatible message JSON, tool
arguments, and tool results. The `1.25` safety multiplier is a fixed v1
constant, reported in diagnostics but not configurable per request. A provider
may correct this estimate from response usage or quota headers. TPM is
therefore admission protection, not a claim of exact provider billing.

If a request's token cost exceeds a configured scope's entire TPM capacity, it
is rejected before sending with a clear capacity error; it is not repeatedly
retried across equivalent keys.

### Candidate selection

1. Preserve alias priority groups and strict context checks from `RoutePlanner`.
2. Within the lowest available priority group, inspect each candidate's pool.
3. Exclude members whose credential, provider, model, or quota scope is in
   cooldown; an unavailable secret also counts as a credential failure.
4. Start from the pool's rotation cursor. For `round_robin`, try each member
   cyclically. For `weighted_round_robin`, expand the deterministic rotation
   by weight. For `least_loaded`, choose the member with the smallest
   `in_flight / max_in_flight`, then break equal scores using the cursor.
5. Attempt to reserve member and quota-scope capacity atomically. A failed
   reservation advances to the next member; a successful reservation advances
   the cursor and returns its lease.
6. If no member in the current candidate can admit work, try the next eligible
   alias candidate immediately. This is capacity fallback, not failure retry.
7. If no candidate can admit work, return an overload error with the earliest
   known retry time. The v1 scheduler has no waiting queue, so it cannot grow
   memory unbounded or add hidden latency to interactive CLI requests.

The cursor advances only after a successful reservation. Consequently, a pool
with three independent idle members and `max_in_flight = 1` distributes three
simultaneous admissions across all three members.

### Dispatch lease lifecycle

`DispatchLease` contains an opaque reservation ID, selected provider/model,
redacted credential ID, and token reservation. It has exactly one terminal
transition:

- Success: release in-flight capacity, reconcile observed usage, clear the
  selected target's transient failure state, and record safe telemetry.
- Classified upstream failure before stream commit: release the lease, record
  cooldown state, and allow execution to request another admission.
- Transport failure, client disconnect, cancellation, or dropped response:
  release in-flight capacity and retain the reserved input charge; do not
  credit an unknown upstream consumption.
- Streaming response: move the lease into the response body wrapper and
  release it only on end-of-stream or drop. Fallback is permitted only before
  the first client-visible event.

Drop is a mandatory safety net. Releasing a lease twice is ignored and emits a
debug assertion in tests; an unknown lease ID cannot decrement capacity.

## Failure, Cooldown, and Feedback Policy

`RouteState` remains the failure-policy type but is stored inside
`SchedulerState`, so failure recording and a later admission share one atomic
state transition. The executor records failure before attempting a new
candidate, so a concurrent request observes the exclusion as soon as its own
admission lock is acquired.

| Condition | Scheduler action |
| --- | --- |
| 429 or quota body | Release lease, apply bounded `Retry-After` or rate-limit cooldown to credential and quota scope, then admit another eligible target. |
| 401/403 | Quarantine only that credential unless the provider adapter marks the response account-wide. |
| 402 | Quarantine provider and all pools for its provider briefly; continue provider fallback. |
| 404 | Cool down `(provider, model)`; do not poison other models or credentials. |
| 5xx or transport | Short provider cooldown; do not change key RPM/TPM budget. |
| 400/422 client request | Release lease and return the request error without fallback. |
| response success | Clear transient target cooldown and reconcile observed usage/header feedback. |

For OpenAI-compatible APIs, the scheduler parses these optional response
headers case-insensitively: `x-ratelimit-limit-requests`,
`x-ratelimit-remaining-requests`, `x-ratelimit-reset-requests`,
`x-ratelimit-limit-tokens`, `x-ratelimit-remaining-tokens`, and
`x-ratelimit-reset-tokens`. Valid headers tighten the matching quota scope;
missing or malformed headers are ignored without changing configured budgets.
Provider-specific header mapping is deliberately deferred until an adapter
needs it.

## Response Contract and Observability

When all candidates are capacity-limited, the gateway returns a normalized
retryable overload response:

```json
{
  "error": {
    "type": "rate_limit_error",
    "code": "provider_capacity_exhausted",
    "message": "All eligible provider credentials are at capacity; retry after 12 seconds."
  }
}
```

The response includes a bounded `Retry-After` header when the earliest refill
or cooldown is known. It never reveals a key source, secret, raw upstream
body, provider internal account ID, or prompt.

New safe runtime fields:

```text
credential_pools: total, active_members, cooling_members, saturated_members
quota_scopes: active, cooling, rpm_limited, tpm_limited
admission: admitted, rejected, fallback_due_to_capacity, queue_depth=0
```

`route explain <alias>` gains a capacity section showing the configured pool,
strategy, eligible member count, active reservation count, and limiting reason
without revealing credential material. `route simulate` gains a deterministic
`--concurrent N` mode and may simulate a 429 from one pool member. `health`
shows capacity state rather than probing or mutating a cooldown.

Structured logs contain `provider`, `model`, `pool`, redacted `credential`,
`quota_scope`, `admission_reason`, `in_flight`, and `retry_after_ms`. They do
not include a serialized request or authorization header. Metrics add counters
for admission outcomes and gauges for per-pool in-flight/saturation with a
bounded label set based on configured IDs.

## Configuration and Reload Semantics

The registry validates pool membership and quota constraints before an atomic
TOML write or daemon reload. A failed validation changes neither file nor live
runtime. `POST /api/v1/provider-runtime/reload` constructs a new scheduler
snapshot only after registry validation succeeds.

Reload builds a complete new runtime snapshot, including a new scheduler,
before atomically replacing the handle. Active leases retain the old snapshot
and its scheduler until their response finishes; new requests use the new
snapshot. The old state is released once its final lease drops. This prevents
a reload from leaking or double-releasing a capacity reservation.

Configuration mutations expose `restart_required` as today unless the daemon
reload endpoint is explicitly called.

## CLI Contract

The canonical command surface remains shallow:

```text
opencode2api pool list|show|set|remove
opencode2api route explain ALIAS
opencode2api route simulate ALIAS --concurrent 3
opencode2api health [PROVIDER]
```

`pool set` accepts provider, strategy, and member limit arguments
but never secret values; users add secret references with existing
`credential set`. `pool remove` fails when an alias still
references that pool. JSON output uses the existing versioned envelope.

## Compatibility and Migration

- Existing schema-v3 direct `credential` candidates compile into implicit
  one-member pools with `max_in_flight = 1`, an individual quota scope, no
  local RPM/TPM cap, and `round_robin` strategy.
- Existing keyless candidates compile into an anonymous single-member pool
  with the same concurrency default.
- Schema-v1/v2 and legacy singleton retry configuration retain their existing
  behavior; legacy `upstream_api_keys` keeps its atomic round-robin path until
  an explicit config migration creates a v3 credential pool.
- `config migrate --write` may materialize implicit pools only when requested;
  normal reads and writes do not churn a user's TOML.

## Security and Compliance

The scheduler is an availability and cost-control mechanism, not a means to
circumvent upstream controls. Operators must declare shared quota scopes for
keys issued under the same account and must comply with provider terms,
including IP/account throttles. Egress routing is not changed in response to a
provider 429.

Managed secret storage remains owner-only; scheduler snapshots retain only
credential IDs and source metadata. Hashes used for internal cooldown keys are
not emitted in user-visible output.

## Acceptance Criteria

1. Three simultaneous requests to a three-member independent pool with one
   permit each receive three different credentials before any HTTP response is
   required.
2. Three credentials sharing one quota scope obey one RPM/TPM budget and do
   not create three independent quota budgets.
3. A 429 causes future admissions to skip the affected credential/scope before
   a new outbound request is sent; another eligible pool member or provider
   may serve the original request before stream commit.
4. A strict 1M alias never chooses an undersized, unverified, disabled, or
   capacity-ineligible candidate merely to reduce load.
5. Cancellation and an incomplete stream release the in-flight lease exactly
   once. No capacity remains stuck after a client disconnect.
6. A malformed pool, a cross-provider member, duplicate member, contradictory
   shared scope budget, or a candidate with both `credential` and
   `credential_pool` fails validation atomically.
7. CLI, REST runtime state, logs, metrics, and test fixtures never contain an
   API secret or Authorization value.
8. Existing provider-management, legacy retry, streaming, Claude Code 1M, and
   fallback test contracts stay green.

## Verification Matrix

| Layer | Required evidence |
| --- | --- |
| registry/config | Schema parsing, cross-reference validation, legacy implicit-pool compatibility, atomic store rollback. |
| scheduler unit | Cursor fairness, weighted order, least-loaded tie-break, atomically reserved permits, RPM/TPM refill, shared scope, and earliest retry calculation with Tokio paused time. |
| execution integration | Three concurrent requests at a barrier, 429 reroute, `Retry-After`, 402/401/404/5xx behavior, stream-drop release, no post-commit replay. |
| CLI/REST | Redacted pool display, invalid mutation rejection, route explain/simulate state, reload with in-flight lease. |
| regression | `cargo test --locked`, clippy with warnings denied, docs/secret scans, and `tests/claude_code_e2e.py --provider-alias free-1m`. |

## Deferred Follow-up

After the static scheduler has telemetry from real workloads, a separate
design may add an opt-in adaptive per-scope concurrency limit. It must use
time-to-first-byte plus error signals, have stable lower/upper bounds, and be
disabled by default. Multi-daemon deployments may replace the local admission
state with atomic Redis-backed buckets and leases while preserving the
`CapacityScheduler::admit` and `DispatchLease` contracts.
