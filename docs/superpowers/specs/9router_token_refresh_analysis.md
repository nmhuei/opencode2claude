# 9router Token Refresh & Lifecycle Analysis

**Sources inspected:** `src/sse/services/tokenRefresh.js`, `open-sse/services/tokenRefresh.js`, `open-sse/services/tokenRefresh/providers.js`, `open-sse/services/tokenRefresh/dedup.js`, `open-sse/services/oauthCredentialManager.js`, `src/sse/services/backgroundTokenRefresh.js`.

---

## 1. High-Level Architecture

Token refresh in 9router is a two-tier system:
1. **On-Demand Request-Time Refresh (`checkAndRefreshToken`)**:
   - Called right before dispatching an upstream request in `chat.js`.
   - Inspects `expiresAt` of the selected connection against `getRefreshLeadMs(provider)`.
   - If nearing expiration (`now >= expiresAt - leadMs`), triggers proactive refresh synchronously before issuing the request.
2. **Background Periodic Refresh (`backgroundTokenRefresh.js`)**:
   - Runs every 5 minutes (`setInterval`).
   - Scans active connections in SQLite with a wider lead time (default 30 minutes: `BACKGROUND_REFRESH_LEAD_MS`).
   - Refreshes tokens in the background so request paths almost never experience refresh latency.

---

## 2. Expiration Detection & Lead Time

### 2.1 Timestamp Normalization
- Handled in `oauthCredentialManager.js`:
  - Detects Unix epoch seconds vs milliseconds (`expiresAt < 1e12` is treated as seconds and multiplied by 1000).
  - Also parses ISO-8601 string dates.
- Default buffer: `TOKEN_EXPIRY_BUFFER_MS = 5 * 60 * 1000` (5 minutes).

### 2.2 Per-Provider Lead Times (`REFRESH_LEAD_MS`)
Configured in `appConstants.js` and registry entries:
- **Default**: 5 minutes
- **Claude**: 4 hours (`14_400_000 ms`) because Claude access tokens have long lifespans and strict rate limits on the refresh endpoint.
- **iFlow**: 24 hours (`86_400_000 ms`)
- **Codex**: 5 days (`432_000_000 ms`) with `maxRefreshAgeMs = 8 days` (force refresh if untouched for 8 days).
- **Antigravity / Gemini CLI**: 5 minutes.

---

## 3. Deduplication & Concurrency Control

### 3.1 In-Flight Deduplication (`dedup.js`)
When multiple concurrent requests hit the gateway with the same expiring token:
- Keyed by `provider:oldRefreshToken` (or `provider:oldAccessToken`).
- If a refresh is already in flight for that key, subsequent callers await the same Promise instead of firing duplicate refresh requests to upstream OAuth servers.
- Results are cached with a short TTL (10s) to absorb micro-bursts.

### 3.2 Lock Guard (`withCredentialRefreshLock`)
- Per-connection mutex ensures only one refresh process writes new credentials to the SQLite DB (`providerConnections` table) at any given time.

---

## 4. Per-Provider Refresh Implementations (`providers.js`)

| Provider | Mechanism | Notes |
|---|---|---|
| **Google / Antigravity** | `POST https://oauth2.googleapis.com/token` | Standard `grant_type=refresh_token`, returns new `access_token` and `expires_in`. |
| **Claude** | `POST https://api.anthropic.com/v1/oauth/token` | JSON body with `client_id` only, returns new tokens and optional rotation. |
| **Codex** | Custom OAuth token refresh | JSON payload, handles quota expiration & error categorization. |
| **GitHub Copilot** | Two-tier token flow | 1. Refresh GitHub OAuth token if needed. 2. Exchange GitHub token for short-lived Copilot session token (`https://api.github.com/copilot_internal/v2/token`). |
| **Cline** | `POST https://api.cline.bot/v1/auth/refresh` | Exchanges refreshToken for fresh accessToken. |
| **DeepSeek / OpenAI** | Static API Keys | No refresh needed (checked by `isApiKeyOnly`). |
| **Vertex AI** | Service Account JWT | Self-minted RS256 JWT, signed locally using private key, exchanged for Google OAuth token. |

---

## 5. Unrecoverable Errors & Account Disabling

When upstream refresh returns:
- `invalid_grant`, `token_revoked`, `account_disabled`, or HTTP 400/401 with unrecoverable error payload:
  - Account is marked `testStatus: "failed"`, `isActive: 0`.
  - Notification emitted, and gateway never attempts to refresh that token again until re-authenticated by user.
