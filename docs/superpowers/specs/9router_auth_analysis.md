# 9router Auth Service Analysis

Source: `/home/light/GitHub/9router/src/sse/services/auth.js`

## `getProviderCredentials(provider, excludeConnectionIds, model, options)`

Selects an upstream provider account (connection) for a request. Serialized by a
module-level promise mutex so concurrent selections cannot race.

Flow:

1. **Normalization** — `excludeConnectionIds` (a single ID, a Set, or null) is normalized
   into a `Set`; `options.preferredConnectionId` can pin selection to a specific account.
2. **Alias resolution** — provider alias is resolved to a canonical provider ID
   (e.g. `kc` → `kilocode`).
3. **No-auth free providers** — if the provider is in `FREE_PROVIDERS` with `noAuth`, a
   virtual connection is returned without touching the DB: `{ id: "noauth", connectionName: "Public",
   accessToken: "public" }`, but with a real proxy config (strategy-aware proxy pool pick via
   `pickProxyPoolId` / `resolveConnectionProxyConfig`).
4. **Load connections** — active connections for the provider come from localDb
   (`getProviderConnections`); returns `null` when there are none.
5. **Filtering** — a connection is excluded if:
   - its ID is in the exclude set (retry-with-next-account exclusion),
   - `isModelLockActive(c, model)` says it has an active per-model lock (`modelLock_*`),
   - (Antigravity only) the lazy quota cache shows per-model quota exhausted with a future `resetAt`.
6. **All-accounts-unavailable handling** — if nothing survives filtering:
   - Computes the earliest lock expiry (persistent model locks + Antigravity quota-cache resets)
     and returns `{ allRateLimited: true, retryAfter, retryAfterHuman, lastError, lastErrorCode }`
     when any expiry exists.
   - Otherwise returns `null` (all accounts unavailable, no known reset time).
7. **Selection strategy** — per-provider override beats global setting, default `fill-first`:
   - **Pinned**: if `preferredConnectionId` matches an available connection, use it (strategy skipped).
   - **round-robin**: sticky — stay with the most-recently-used account until
     `consecutiveUseCount` reaches `stickyRoundRobinLimit` (default 3), then switch to the
     least-recently-used account. `lastUsedAt`/`consecutiveUseCount` updates are awaited for persistence.
   - **fill-first** (default): take the first available connection (already priority-sorted in localDb).
8. **Proxy resolution** — `resolveConnectionProxyConfig` resolves per-connection proxy settings
   and they are merged into `providerSpecificData`.
9. **Return shape** — a credential object with `apiKey`, `accessToken`, `refreshToken`,
   `idToken`, `expiresAt`/`expiresIn`, `projectId`, `connectionName`, `connectionId`,
   `copilotToken`, resolved proxy fields in `providerSpecificData`, current
   `testStatus`/`lastError`, plus `_connection` (the full DB record) so that
   `clearAccountError` can read `modelLock_*` keys later.

## `markAccountUnavailable(connectionId, status, errorText, provider, model, resetsAtMs)`

Marks a failed account+model as unavailable by writing a `modelLock_${model}` (or account-wide
`modelLock___all`) lock into the DB, and decides whether the caller should fall back to
another account.

Cooldown / lock logic (in precedence order):

1. **noauth / missing ID** — no-op, returns `{ shouldFallback: false, cooldownMs: 0 }`.
2. **GitHub 402 monthly exhaustion** — if the provider is GitHub with HTTP 402 and the
   "monthly usage limit" message, lock is account-wide until the 1st of the next UTC month
   (`githubMonthlyResetMs`); backoff level reset to 0.
3. **Provider-supplied `resetsAtMs`** — exact reset time from upstream (e.g. codex
   `usage_limit_reached`, Antigravity quota API) wins over backoff. Antigravity's exact
   per-model `resetAt` is never truncated; other providers cap it at
   `MAX_RATE_LIMIT_COOLDOWN_MS`. Backoff level reset to 0.
4. **Fallback default** — `checkFallbackError(status, errorText, backoffLevel)` computes
   `shouldFallback`, the cooldown, and the next backoff level. If it says no fallback,
   returns `{ shouldFallback: false, cooldownMs: 0 }`.

Write step (on fallback):

- `updateProviderConnection(connectionId, { ...lockUpdate, testStatus: "unavailable",
  lastError, errorCode: status, lastErrorAt, backoffLevel })` — the lock key comes from
  `buildModelLockUpdate` (per-`model` lock; GitHub case passes `model = null` so the lock is
  account-wide).
- Emits a warning log with the account name, lock key, cooldown seconds, and status; also
  `console.error`s `❌ provider [status]: reason`.

Returns `{ shouldFallback: true, cooldownMs }`.

Key design point: **all upstream errors (429, 401, 5xx, …) lock per model, not per account**,
so a failure on one model does not block other models on the same account; the GitHub 402 case
is the exception (account-wide until month reset). Success clears locks via `clearAccountError`
in the same file.
