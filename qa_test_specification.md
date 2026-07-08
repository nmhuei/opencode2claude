# OpenCode2API — QA Test Specification

> **Document version:** 1.0  
> **Target:** opencode2api v0.4.x (dashboard, bridge API, auth separation)  
> **Test file:** `tests/fast.rs` (74 test cases, 1561 lines)  
> **Branch:** `qa/pentest-validation`

---

## 1. Overview

This specification defines **74 automated test cases** (TC-001 through TC-074 plus auxiliary AUTH-STATUS and LOGOUT variants) for the OpenCode2API bridge application. The tests cover five functional domains:

| Domain | Scope | Count |
|--------|-------|-------|
| **Dashboard UI & Routing** | Landing page, SPA, static assets, cache headers, security headers | 8 |
| **Dashboard Auth** | Login/logout, token validation, session cookies, fail-closed behavior | 18 |
| **Dashboard Config** | Config read/write, TOML validation, secret masking, persistence | 9 |
| **Dashboard Diagnostics & Proxy** | Diagnostics endpoint, proxy restart, daemon status, node roles | 16 |
| **Bridge API** | Messages (sync/streaming), models, health, request validation, body limits | 13 |
| **Auth Separation** | Cross-realm auth isolation (bridge token vs dashboard token), anonymous rejection | 10 |

### Key Tested Properties

- **Auth separation** — Dashboard admin token (`X-Dashboard-Token`) and bridge bearer token (`Authorization: Bearer`) authenticate separate API realms and must NOT cross-authenticate.
- **Fail-closed** — When `DASHBOARD_ADMIN_TOKEN` is unset, all dashboard endpoints reject requests (including the legacy default `123456`).
- **Security headers** — CSP, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store` on sensitive pages.
- **Input validation** — Missing fields, malformed JSON, empty arrays, oversized payloads (1 MB limit), invalid TOML.
- **Public binding guards** — Security validation aborts startup when binding `0.0.0.0` without a configured (or with a weak) admin token.
- **Error safety** — No stack traces or panic details in error responses.

---

## 2. Test Environment

### Prerequisites

- Rust toolchain (edition 2021)
- No running opencode2api instance on port conflicts (tests bind random ports)

### Execution

```bash
# Run all 74 fast test cases (no release build required)
cargo test --test fast

# Run a single test case
cargo test --test fast test_tc001_get_root

# Run with output capture disabled
cargo test --test fast -- --nocapture

# Run tests matching a category (e.g. all auth tests)
cargo test --test fast auth
```

### Test Harness

The `build_test_router()` function constructs the same router tree used in production (`main.rs`), but with a `BridgeConfig` passed directly (no TOML file dependency). The `spawn_test_server()` helper binds a `TcpListener` to `127.0.0.1:0` (OS-assigned random port), spawns the server on a tokio task, and polls `/health` until the server is ready.

A `ConfigBackupGuard` protects `opencode2api.toml` from accidental modification during config-save tests.

### Environment Override

Tests that modify `DASHBOARD_ADMIN_TOKEN` or other environment variables use a global `ENV_MUTEX` (`std::sync::Mutex`) to prevent concurrent environment variable races.

---

## 3. Auth Separation Matrix

The application has **two independent authentication realms**:

| Realm | Auth Mechanism | Header | Token Source |
|-------|---------------|--------|-------------|
| **Bridge API** | Bearer token | `Authorization: Bearer <token>` | `BRIDGE_AUTH_TOKEN` env / config |
| **Dashboard API** | Custom header | `X-Dashboard-Token: <token>` | `DASHBOARD_ADMIN_TOKEN` env / config |

### Token → Endpoint Compatibility

| Endpoint | No Token | Bridge Token | Dashboard Token | Anonymous (no auth configured) |
|----------|----------|-------------|-----------------|-------------------------------|
| `GET /` (landing) | OK (200) | OK (200) | OK (200) | OK (200) |
| `GET /health` | OK (200) | OK (200) | OK (200) | OK (200) |
| `GET /dashboard*` (SPA) | Redirect (302) | **401** | OK (200*) | OK (200) |
| `GET /api/dashboard/status` | 401 | **401** | OK (200) | OK (200) |
| `POST /api/dashboard/login` | 401 | **401** | OK (200) | OK (200) |
| `GET /api/dashboard/auth/status` | OK (200*, unauthenticated) | OK (200*, unauthenticated) | OK (200*, authenticated) | OK (200*) |
| `POST /api/dashboard/logout` | OK (200) | OK (200) | OK (200) | OK (200) |
| `GET /api/dashboard/config` | 401 | **401** | OK (200) | OK (200) |
| `POST /api/dashboard/config/save` | 401 | **401** | OK (200) | OK (200) |
| `GET /api/dashboard/diagnostics` | 401 | **401** | OK (200) | OK (200) |
| `POST /api/dashboard/proxy/:port/restart` | 401 | **401** | OK (200) | OK (200) |
| `GET /api/dashboard/events` (SSE) | 401 | **401** | OK (200) | OK (200) |
| `POST /v1/messages` | 401 | OK (200/422) | **401** | OK (200) |
| `GET /v1/models` | 401 | OK (200) | **401** | OK (200) |

**Notes:**
- `*` = SPA routes require a session cookie (set by login) in addition to dashboard token.
- `AUTH-Status` endpoint always returns 200; it reports `authenticated: true/false` in the JSON body.
- Cells marked **bold** are cross-realm rejection points verified by dedicated tests (TC-057 through TC-061).

---

## 4. Test Case Catalog

### 4.1 Group 1 — Routing & Headers (TC-001 to TC-008)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-001 | `test_tc001_get_root` | routing | Landing page (`/`) is served as `text/html` with `<title>OpenCode2API Bridge</title>` and a password form (`id="password"`). |
| TC-002 | `test_tc002_get_dashboard_no_cookie` | auth | `GET /dashboard` without session cookie returns 302 redirect to `/`. Auth enforcement — no cookie, no entry. |
| TC-003 | `test_tc003_get_dashboard_slash_no_cookie` | auth | `GET /dashboard/` without session cookie returns 302 redirect to `/`. Same enforcement for trailing slash path. |
| TC-004 | `test_tc004_get_dashboard_index_no_cookie` | auth | `GET /dashboard/index.html` without session cookie returns 302 redirect to `/`. |
| TC-005 | `test_tc005_get_static_asset_css` | routing | CSS assets (e.g. `/dashboard/style.css`) are publicly accessible without auth. |
| TC-006 | `test_tc006_get_static_asset_js` | routing | JS assets (e.g. `/dashboard/main.js`) are publicly accessible without auth. |
| TC-007 | `test_tc007_spa_fallback_behavior` | routing | `GET /dashboard/some/subroute` with a valid session cookie renders the dashboard HTML. Verifies no info leak on unknown subroutes. |
| TC-008 | `test_tc008_cache_control_headers` | security-headers | Landing page sets `Cache-Control: no-store, no-cache, must-revalidate` and `Pragma: no-cache` to prevent caching. |

### 4.2 Group 2 — Dashboard Auth (TC-009 to TC-018, AUTH-STATUS-* , LOGOUT-*)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-009 | `test_tc009_status_no_token` | auth | `GET /api/dashboard/status` returns 401 when no `X-Dashboard-Token` header is sent. |
| TC-010 | `test_tc010_status_wrong_token` | auth | `GET /api/dashboard/status` returns 401 with incorrect `X-Dashboard-Token: wrong-token`. |
| TC-011 | `test_tc011_status_correct_token` | auth | `GET /api/dashboard/status` returns 200 with correct `X-Dashboard-Token`. |
| TC-012 | `test_tc012_status_legacy_default_token` | auth | `X-Dashboard-Token: 123456` is rejected when a strong `DASHBOARD_ADMIN_TOKEN` is configured. |
| TC-013 | `test_tc013_login_no_token` | auth | `POST /api/dashboard/login` returns 401 with no token. |
| TC-014 | `test_tc014_login_wrong_token` | auth | `POST /api/dashboard/login` returns 401 with wrong token. |
| TC-015 | `test_tc015_login_correct_token` | auth | `POST /api/dashboard/login` returns 200 with correct token. |
| TC-016 | `test_tc016_events_no_token` | auth | `GET /api/dashboard/events` (SSE) returns 401 without token. |
| TC-017 | `test_tc017_events_wrong_token` | auth | `GET /api/dashboard/events?token=wrong` returns 401 with wrong token as query param. |
| TC-018 | `test_tc018_events_correct_token` | auth | `GET /api/dashboard/events?token=correct` streams events with correct token query param. |
| AUTH-STATUS-01 | `test_auth_status_no_token` | auth | `GET /api/dashboard/auth/status` without token returns `admin_token_configured: true` and `authenticated: false`. Safe info disclosure. |
| AUTH-STATUS-02 | `test_auth_status_authenticated` | auth | `GET /api/dashboard/auth/status` with valid token returns `authenticated: true`. |
| AUTH-STATUS-03 | `test_auth_status_no_admin_token_configured` | auth | `GET /api/dashboard/auth/status` shows `admin_token_configured: false` when `DASHBOARD_ADMIN_TOKEN` is unset. |
| LOGOUT-01 | `test_logout_clears_cookie` | auth | `POST /api/dashboard/logout` clears session cookie with `Max-Age=0`. |
| LOGOUT-02 | `test_logout_no_auth` | auth | `POST /api/dashboard/logout` returns `{"ok":true}` even without auth token (graceful handling). |

### 4.3 Group 3 — Dashboard Config (TC-019 to TC-028)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-019 | `test_tc019_get_config_no_token` | config | `GET /api/dashboard/config` returns 401 without auth token. |
| TC-020 | `test_tc020_get_config_wrong_token` | config | `GET /api/dashboard/config` rejects wrong `X-Dashboard-Token`. |
| TC-021 | `test_tc021_get_config_correct_token` | config | `GET /api/dashboard/config` returns bridge config JSON with correct token. |
| TC-022 | `test_tc022_save_config_no_token` | config | `POST /api/dashboard/config/save` returns 401 without auth token. |
| TC-023 | `test_tc023_save_config_wrong_token` | config | `POST /api/dashboard/config/save` rejects wrong token. |
| TC-024 | `test_tc024_save_config_correct_token_valid_toml` | config | `POST /api/dashboard/config/save` succeeds with valid TOML content and auth. |
| TC-025 | `test_tc025_save_config_correct_token_invalid_toml` | config | `POST /api/dashboard/config/save` rejects invalid TOML with 400 error. |
| TC-026 | `test_tc026_save_config_correct_token_empty_body` | config | `POST /api/dashboard/config/save` returns 400 when body has no `content` field. |
| TC-027 | `test_tc027_sensitive_config_masking` | config | `GET /api/dashboard/config` masks API keys (`TAVILY_API_KEY`, `SERPER_API_KEY`) as `***` in the response. Prevents credential leak in output. |
| TC-028 | `test_tc028_config_reload_verification` | config | After `POST /api/dashboard/config/save`, the config file is verified to exist on disk with the written content. Verifies config persistence. |

### 4.4 Group 4 — Bridge API & Messages (TC-029 to TC-040)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-029 | `test_tc029_get_models_anonymous_auth_disabled` | bridge-api | `GET /v1/models` accessible without auth when `BRIDGE_AUTH_TOKEN` is unset. |
| TC-030 | `test_tc030_post_messages_anonymous_auth_disabled` | bridge-api | `POST /v1/messages` accessible without auth when `BRIDGE_AUTH_TOKEN` is unset (returns 422 for missing fields, not 401). |
| TC-031 | `test_tc031_get_models_valid_bearer` | bridge-api | `GET /v1/models` accessible with valid `Authorization: Bearer <token>`. |
| TC-032 | `test_tc032_get_models_invalid_bearer` | bridge-api | `GET /v1/models` returns 401 with invalid bearer token. |
| TC-033 | `test_tc033_get_models_missing_bearer` | bridge-api | `GET /v1/models` returns 401 without bearer token when `BRIDGE_AUTH_TOKEN` is set. |
| TC-034 | `test_tc034_post_messages_missing_messages_field` | bridge-api | `POST /v1/messages` returns 422 when the required `messages` field is missing from the JSON body. |
| TC-035 | `test_tc035_post_messages_empty_messages_array` | bridge-api | `POST /v1/messages` returns 400 `invalid_request_error` when `messages` is an empty array. |
| TC-036 | `test_tc036_post_messages_non_streaming` | bridge-api | Non-streaming `POST /v1/messages` request is handled (no 401 auth bypass). |
| TC-037 | `test_tc037_post_messages_streaming` | bridge-api | Streaming `POST /v1/messages` request is handled (no 401 auth bypass). |
| TC-038 | `test_tc038_post_messages_unsupported_model_fallback` | bridge-api | Falls back gracefully for unsupported model names (no crash/panic). |
| TC-039 | `test_tc039_post_messages_large_payload` | bridge-api | Returns 413 for payload exceeding 1 MB body limit. |
| TC-040 | `test_tc040_post_messages_malformed_json` | bridge-api | Returns 400 for malformed JSON body. |

### 4.5 Group 5 — Health & Diagnostics (TC-041 to TC-048)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-041 | `test_tc041_health_check_minimal` | health | `GET /health` returns `{"status":"ok"}` with a `version` field. No auth required. |
| TC-042 | `test_tc042_health_check_zero_topology_leak` | health | `GET /health` does NOT expose `proxy_pool`, daemon status, or bridge config in its response body. Prevents topology information leak. |
| TC-043 | `test_tc043_diagnostics_no_token` | diagnostics | `GET /api/dashboard/diagnostics` returns 401 without dashboard token. |
| TC-044 | `test_tc044_diagnostics_wrong_token` | diagnostics | `GET /api/dashboard/diagnostics` rejects wrong `X-Dashboard-Token`. |
| TC-045 | `test_tc045_diagnostics_correct_token` | diagnostics | `GET /api/dashboard/diagnostics` returns proxy and health details with correct token. |
| TC-046 | `test_tc046_diagnostics_daemon_status` | diagnostics | Diagnostics response includes a `daemon_status` object. |
| TC-047 | `test_tc047_diagnostics_config_properties` | diagnostics | Diagnostics response includes config properties (e.g. `shell_policy`). |
| TC-048 | `test_tc048_diagnostics_proxy_node_roles` | diagnostics | Diagnostics response includes proxy node role information (tier, status, port). |

### 4.6 Group 6 — Proxy Restart (TC-049 to TC-056)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-049 | `test_tc049_restart_proxy_no_token` | proxy | `POST /api/dashboard/proxy/40001/restart` returns 401 without dashboard token. |
| TC-050 | `test_tc050_restart_proxy_wrong_token` | proxy | `POST /api/dashboard/proxy/40001/restart` rejects wrong `X-Dashboard-Token`. |
| TC-051 | `test_tc051_restart_proxy_valid_node_40001` | proxy | Proxy restart returns 200 for valid primary node port 40001 with auth. |
| TC-052 | `test_tc052_restart_proxy_valid_node_40003` | proxy | Proxy restart returns 200 for valid primary node port 40003 with auth. |
| TC-053 | `test_tc053_restart_proxy_out_of_range_9999` | proxy | Proxy restart returns 400 for out-of-range port 9999 with error message. |
| TC-054 | `test_tc054_restart_proxy_non_numeric` | proxy | Proxy restart returns 400 for non-numeric port string (e.g. `abc`). |
| TC-055 | `test_tc055_restart_proxy_out_of_range_40000` | proxy | Proxy restart returns 400 for port 40000 (below valid range minimum 40001). |
| TC-056 | `test_tc056_restart_proxy_out_of_range_40006` | proxy | Proxy restart returns 400 for port 40006 (above valid range maximum 40005). |

### 4.7 Group 7 — Auth Separation & Boundary Conditions (TC-057 to TC-074)

| ID | Function | Category | Description |
|----|----------|----------|-------------|
| TC-057 | `test_tc057_access_bridge_api_using_dashboard_token` | auth-separation | Dashboard admin token must NOT authenticate bridge API. Sending `X-Dashboard-Token` to `GET /v1/models` returns 401. |
| TC-058 | `test_tc058_access_dashboard_api_using_bridge_token` | auth-separation | Bridge bearer token must NOT authenticate dashboard status API. Sending `Authorization: Bearer <bridge-token>` to `GET /api/dashboard/status` returns 401. |
| TC-059 | `test_tc059_access_config_api_using_bridge_token` | auth-separation | Bridge bearer token must NOT authenticate dashboard config API (`GET /api/dashboard/config`). |
| TC-060 | `test_tc060_access_diagnostics_api_using_bridge_token` | auth-separation | Bridge bearer token must NOT authenticate dashboard diagnostics API. |
| TC-061 | `test_tc061_access_events_sse_using_bridge_token` | auth-separation | Bridge bearer token must NOT authenticate dashboard events SSE endpoint. |
| TC-062 | `test_tc062_access_status_api_anonymously` | auth-separation | Dashboard status API returns 401 for anonymous access when auth is configured. |
| TC-063 | `test_tc063_access_config_api_anonymously` | auth-separation | Dashboard config API returns 401 for anonymous access. |
| TC-064 | `test_tc064_access_restart_api_anonymously` | auth-separation | Dashboard proxy restart API returns 401 for anonymous access. |
| TC-065 | `test_tc065_fail_closed_when_unset_default_token` | fail-closed | When `DASHBOARD_ADMIN_TOKEN` is unset, even the legacy default `123456` is rejected with `Dashboard is disabled`. |
| TC-066 | `test_tc066_reject_123456_when_strong_token_configured` | auth | Rejects weak default `123456` token when a strong `DASHBOARD_ADMIN_TOKEN` is configured. |
| TC-067 | `test_tc067_security_headers_on_landing` | security-headers | Landing page (`/`) sets `Content-Security-Policy`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`. |
| TC-068 | `test_tc068_security_headers_on_dashboard_spa` | security-headers | Dashboard SPA sets same security headers as landing (CSP, frame protection, nosniff, referrer). |
| TC-069 | `test_tc069_safe_error_responses_no_stack_traces` | error-handling | Error responses do NOT leak stack traces, panic messages, or internal file paths in the response body. |
| TC-070 | `test_tc070_public_binding_abort_on_weak_token` | error-handling | Binding `0.0.0.0` with weak token (`123456`) fails security validation. |
| TC-071 | `test_tc071_public_binding_abort_on_empty_token` | error-handling | Binding `0.0.0.0` without `DASHBOARD_ADMIN_TOKEN` fails security validation. |
| TC-072 | `test_tc072_unsupported_http_method` | error-handling | Unsupported HTTP methods (e.g. `PUT`, `DELETE`) on existing routes return 405 `Method Not Allowed`. |
| TC-073 | `test_tc073_fail_closed_on_diagnostics_unset` | fail-closed | Diagnostics endpoint fails closed: returns 401 when `DASHBOARD_ADMIN_TOKEN` is unset, even with token `123456`. |
| TC-074 | `test_tc074_content_type_validation` | bridge-api | API responses have correct `Content-Type: application/json` header with charset. |

---

## 5. Critical Security Regressions

These 7 mandatory checks must pass before any production release. Each check has clear pass/fail criteria.

### CR-1: Auth Realm Isolation

**Pass:** A valid bridge bearer token must NOT authenticate any dashboard endpoint (`/api/dashboard/*`). A valid dashboard token must NOT authenticate any bridge endpoint (`/v1/*`).  
**Fail:** Any cross-realm auth success.  
**Tests:** TC-057, TC-058, TC-059, TC-060, TC-061

### CR-2: Fail-Close Without Admin Token

**Pass:** When `DASHBOARD_ADMIN_TOKEN` is unset, all dashboard API endpoints return 401 with `Dashboard is disabled` or equivalent. The legacy default `123456` is also rejected.  
**Fail:** Any dashboard endpoint returns 200 or serves data when `DASHBOARD_ADMIN_TOKEN` is unset.  
**Tests:** TC-065, TC-073

### CR-3: Weak Default Token Rejection

**Pass:** The hardcoded default `123456` is rejected when a strong token is configured.  
**Fail:** Token `123456` authenticates successfully against a configured strong token.  
**Tests:** TC-012, TC-066

### CR-4: Public Binding Guardrails

**Pass:** Binding `0.0.0.0` with no admin token or a weak admin token (`123456`) fails security validation (startup aborted).  
**Fail:** Server starts on `0.0.0.0` without a strong admin token.  
**Tests:** TC-070, TC-071

### CR-5: Secrets Never Leaked

**Pass:** API keys (`TAVILY_API_KEY`, `SERPER_API_KEY`, `EXA_API_KEY`) are masked as `***` in config output. Health endpoint does not expose proxy topology or config.  
**Fail:** Any plaintext API key or proxy pool topology appears in a non-admin response.  
**Tests:** TC-027, TC-042

### CR-6: Security Headers Present

**Pass:** Landing page and dashboard SPA set all four security headers: `Content-Security-Policy`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.  
**Fail:** Any of these headers is missing or set to a permissive value (e.g. `SAMEORIGIN` for X-Frame-Options).  
**Tests:** TC-067, TC-068

### CR-7: No Information Leak in Errors

**Pass:** Error responses contain only standard HTTP status text and safe error messages. No stack traces, file paths, variable names, or panic details are present in the response body.  
**Fail:** Any error response body contains file paths, Rust panic details, line numbers, or internal variable names.  
**Tests:** TC-069, TC-074

---

## 6. Test Results

### 6.1 Results Matrix

| ID | Name | Status | Date | Notes |
|----|------|--------|------|-------|
| TC-001 | `test_tc001_get_root` | | | |
| TC-002 | `test_tc002_get_dashboard_no_cookie` | | | |
| TC-003 | `test_tc003_get_dashboard_slash_no_cookie` | | | |
| TC-004 | `test_tc004_get_dashboard_index_no_cookie` | | | |
| TC-005 | `test_tc005_get_static_asset_css` | | | |
| TC-006 | `test_tc006_get_static_asset_js` | | | |
| TC-007 | `test_tc007_spa_fallback_behavior` | | | |
| TC-008 | `test_tc008_cache_control_headers` | | | |
| TC-009 | `test_tc009_status_no_token` | | | |
| TC-010 | `test_tc010_status_wrong_token` | | | |
| TC-011 | `test_tc011_status_correct_token` | | | |
| TC-012 | `test_tc012_status_legacy_default_token` | | | |
| TC-013 | `test_tc013_login_no_token` | | | |
| TC-014 | `test_tc014_login_wrong_token` | | | |
| TC-015 | `test_tc015_login_correct_token` | | | |
| TC-016 | `test_tc016_events_no_token` | | | |
| TC-017 | `test_tc017_events_wrong_token` | | | |
| TC-018 | `test_tc018_events_correct_token` | | | |
| AUTH-STATUS-01 | `test_auth_status_no_token` | | | |
| AUTH-STATUS-02 | `test_auth_status_authenticated` | | | |
| AUTH-STATUS-03 | `test_auth_status_no_admin_token_configured` | | | |
| LOGOUT-01 | `test_logout_clears_cookie` | | | |
| LOGOUT-02 | `test_logout_no_auth` | | | |
| TC-019 | `test_tc019_get_config_no_token` | | | |
| TC-020 | `test_tc020_get_config_wrong_token` | | | |
| TC-021 | `test_tc021_get_config_correct_token` | | | |
| TC-022 | `test_tc022_save_config_no_token` | | | |
| TC-023 | `test_tc023_save_config_wrong_token` | | | |
| TC-024 | `test_tc024_save_config_correct_token_valid_toml` | | | |
| TC-025 | `test_tc025_save_config_correct_token_invalid_toml` | | | |
| TC-026 | `test_tc026_save_config_correct_token_empty_body` | | | |
| TC-027 | `test_tc027_sensitive_config_masking` | | | |
| TC-028 | `test_tc028_config_reload_verification` | | | |
| TC-029 | `test_tc029_get_models_anonymous_auth_disabled` | | | |
| TC-030 | `test_tc030_post_messages_anonymous_auth_disabled` | | | |
| TC-031 | `test_tc031_get_models_valid_bearer` | | | |
| TC-032 | `test_tc032_get_models_invalid_bearer` | | | |
| TC-033 | `test_tc033_get_models_missing_bearer` | | | |
| TC-034 | `test_tc034_post_messages_missing_messages_field` | | | |
| TC-035 | `test_tc035_post_messages_empty_messages_array` | | | |
| TC-036 | `test_tc036_post_messages_non_streaming` | | | |
| TC-037 | `test_tc037_post_messages_streaming` | | | |
| TC-038 | `test_tc038_post_messages_unsupported_model_fallback` | | | |
| TC-039 | `test_tc039_post_messages_large_payload` | | | |
| TC-040 | `test_tc040_post_messages_malformed_json` | | | |
| TC-041 | `test_tc041_health_check_minimal` | | | |
| TC-042 | `test_tc042_health_check_zero_topology_leak` | | | |
| TC-043 | `test_tc043_diagnostics_no_token` | | | |
| TC-044 | `test_tc044_diagnostics_wrong_token` | | | |
| TC-045 | `test_tc045_diagnostics_correct_token` | | | |
| TC-046 | `test_tc046_diagnostics_daemon_status` | | | |
| TC-047 | `test_tc047_diagnostics_config_properties` | | | |
| TC-048 | `test_tc048_diagnostics_proxy_node_roles` | | | |
| TC-049 | `test_tc049_restart_proxy_no_token` | | | |
| TC-050 | `test_tc050_restart_proxy_wrong_token` | | | |
| TC-051 | `test_tc051_restart_proxy_valid_node_40001` | | | |
| TC-052 | `test_tc052_restart_proxy_valid_node_40003` | | | |
| TC-053 | `test_tc053_restart_proxy_out_of_range_9999` | | | |
| TC-054 | `test_tc054_restart_proxy_non_numeric` | | | |
| TC-055 | `test_tc055_restart_proxy_out_of_range_40000` | | | |
| TC-056 | `test_tc056_restart_proxy_out_of_range_40006` | | | |
| TC-057 | `test_tc057_access_bridge_api_using_dashboard_token` | | | |
| TC-058 | `test_tc058_access_dashboard_api_using_bridge_token` | | | |
| TC-059 | `test_tc059_access_config_api_using_bridge_token` | | | |
| TC-060 | `test_tc060_access_diagnostics_api_using_bridge_token` | | | |
| TC-061 | `test_tc061_access_events_sse_using_bridge_token` | | | |
| TC-062 | `test_tc062_access_status_api_anonymously` | | | |
| TC-063 | `test_tc063_access_config_api_anonymously` | | | |
| TC-064 | `test_tc064_access_restart_api_anonymously` | | | |
| TC-065 | `test_tc065_fail_closed_when_unset_default_token` | | | |
| TC-066 | `test_tc066_reject_123456_when_strong_token_configured` | | | |
| TC-067 | `test_tc067_security_headers_on_landing` | | | |
| TC-068 | `test_tc068_security_headers_on_dashboard_spa` | | | |
| TC-069 | `test_tc069_safe_error_responses_no_stack_traces` | | | |
| TC-070 | `test_tc070_public_binding_abort_on_weak_token` | | | |
| TC-071 | `test_tc071_public_binding_abort_on_empty_token` | | | |
| TC-072 | `test_tc072_unsupported_http_method` | | | |
| TC-073 | `test_tc073_fail_closed_on_diagnostics_unset` | | | |
| TC-074 | `test_tc074_content_type_validation` | | | |

### 6.2 Critical Regression Summary

| # | Check | Required Tests | Status |
|---|-------|----------------|--------|
| CR-1 | Auth Realm Isolation | TC-057, TC-058, TC-059, TC-060, TC-061 | |
| CR-2 | Fail-Close Without Admin Token | TC-065, TC-073 | |
| CR-3 | Weak Default Token Rejection | TC-012, TC-066 | |
| CR-4 | Public Binding Guardrails | TC-070, TC-071 | |
| CR-5 | Secrets Never Leaked | TC-027, TC-042 | |
| CR-6 | Security Headers Present | TC-067, TC-068 | |
| CR-7 | No Information Leak in Errors | TC-069, TC-074 | |

### 6.3 Pass Rate

| Metric | Value |
|--------|-------|
| Total Tests | 74 |
| Passed | |
| Failed | |
| Pass Rate | |

---

## 7. Runbook

### Initial Run

```bash
# From project root
cargo test --test fast 2>&1 | tee qa_run_$(date +%Y%m%d_%H%M).log
```

### Failed Test Debugging

1. Run the failing test in isolation:
   ```bash
   cargo test --test fast test_tcXXX -- --nocapture
   ```
2. Check the assertion line in `tests/fast.rs` for expected values.
3. If a test depends on environment state, ensure `ENV_MUTEX` is being used and no concurrent test is modifying the same env vars.

### Adding New Tests

Add new test cases to `tests/fast.rs` following the existing pattern:
1. Increment the TC number in sequence.
2. Add the test function with `#[tokio::test]`.
3. Update this specification with the new entry.
4. Update the test file header comment with the new TC range.
