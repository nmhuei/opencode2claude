# Configuration reference

OpenCode2API resolves configuration with this precedence, from lowest to highest:

```text
defaults < TOML file < environment variables < CLI options
```

The default TOML path is `opencode2api.toml`. Override it with `BRIDGE_CONFIG_PATH` or `--config`. Generate a current template with:

```bash
opencode2api init --output opencode2api.toml
```

## Server and protocol

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_HOST` | `host` | `127.0.0.1` | Bind address. Non-loopback binding requires strong client and dashboard authentication. |
| `BRIDGE_PORT` | `port` | `4000` | HTTP listen port. |
| `BRIDGE_MAX_BODY_SIZE` | `max_body_size` | `67108864` | Incoming `/v1/*` request limit in bytes; `0` disables the limit. |
| `BRIDGE_STREAM_BUFFER_SIZE` | `stream_buffer_size` | `65536` | Initial upstream SSE line-buffer capacity. |
| `BRIDGE_CHANNEL_CAPACITY` | `channel_capacity` | `2048` | Bounded downstream SSE channel capacity. |
| `BRIDGE_MAX_SSE_LINE_BYTES` | `max_sse_line_bytes` | `4194304` | Maximum upstream SSE line size. |
| `BRIDGE_MAX_SYNC_RESPONSE_BYTES` | `max_sync_response_bytes` | `33554432` | Maximum non-streaming upstream response body. |
| `BRIDGE_MIN_REASONING_STREAM_TOKENS` | `min_reasoning_stream_tokens` | `1024` | Minimum mapped token budget for reasoning streams. |
| `OPENCODE_PORT` | `opencode_port` | `4096` | Optional local OpenCode daemon probe port used by diagnostics. |
| `OPENCODE_MODEL` | `model` | `claude-3-5-sonnet` | Default upstream model identifier. |
| `OPENCODE_UPSTREAM_BASE_URL` | `upstream_base_url` | `https://opencode.ai/zen/v1` | OpenAI-compatible upstream base URL. |
| `OPENCODE_UPSTREAM_API_KEY` | `upstream_api_key` | unset | Bearer credential for an external OpenAI-compatible upstream. Stored as a secret. |

`BRIDGE_UPSTREAM_BASE_URL` and `BRIDGE_UPSTREAM_API_KEY` are accepted as compatibility aliases. Prefer the `OPENCODE_*` names in new deployments.

Upstream Bearer credentials are sent only over HTTPS, except for loopback HTTP endpoints (localhost, 127.0.0.1, ::1). Secret values are intentionally not accepted as ordinary CLI arguments. Prefer opencode2api provider opencode [MODEL] for OpenCode Zen or pipe a key to opencode2api provider api <URL> <MODEL> --api-key-stdin for a custom API. A one-off legacy --upstream-base-url override never inherits a stored key from another provider.


### Curated custom-API model profiles

| Model | Context window | 80% auto-compact | Max output | Default output | Billing |
|---|---:|---:|---:|---:|---|
| deepseek-v4-flash | 1,000,000 | 800,000 | 384,000 | provider-defined | Free (0 Credits) |
| deepseek-v4-flash-vision-exp | 1,000,000 | 800,000 | 384,000 | provider-defined | Free (0 Credits) |
| glm-5.3-flash | 1,000,000 | 800,000 | 131,072 | 65,536 | Free (0 Credits) |

These exact profiles are applied to Claude Code environment tuning and to model discovery output when the API exposes the matching IDs.

## Authentication and shell policy

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_AUTH_TOKEN` | `auth_tokens` | unset | Comma-separated client API bearer tokens. Required for non-loopback binding. |
| `DASHBOARD_ADMIN_TOKEN` | `dashboard_admin_token` | unset | Dashboard login/header token. Must be at least 12 characters for public binding. |
| `REST_API_TOKEN` | `rest_api_token` | dashboard token fallback | Bearer token for `/api/v1/*`. |
| `DASHBOARD_CSRF_ENABLED` | `csrf_enabled` | `true` | Requires double-submit CSRF token for cookie-authenticated mutations. |
| `BRIDGE_SHELL_POLICY` | `shell_policy` | `disabled` | `disabled`, `allowlist`, or `unrestricted`. |
| `BRIDGE_SHELL_ALLOWLIST` | `shell_allowlist` | safe command list | Allowed base commands when policy is `allowlist`. Shell metacharacters remain rejected. |

Shell delegation emits an Anthropic `tool_use` response for the client. The bridge does not run arbitrary user shell commands inside HTTP handlers.

## Retry and model fallback

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `OPENCODE_MODEL_FALLBACKS` | `model_fallbacks` | empty | Ordered explicit fallback model list. |
| `OPENCODE_ENABLE_DEFAULT_FALLBACKS` | `enable_default_fallbacks` | `false` | Enables built-in fallbacks for compatible non-reasoning requests. |
| `BRIDGE_MAX_NETWORK_ATTEMPTS` | `max_network_attempts` | `8` | Bounded transport retry budget; provider/application failures keep their typed fail-fast/fallback semantics. |
| `BRIDGE_RETRY_BASE_BACKOFF_MS` | `retry_base_backoff_ms` | `1000` | Initial jittered backoff. |
| `BRIDGE_RETRY_MAX_BACKOFF_MS` | `retry_max_backoff_ms` | `30000` | Maximum jittered backoff. |

Model fallback and transport retry are accounted independently. Provider errors do not mark a healthy proxy as a transport failure.

## Egress and WARP pool

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_EGRESS_MODE` | `egress_mode` | `hybrid` | `direct`, strict `proxy`, or `hybrid`. Hybrid is direct-ready at startup and prefers verified proxy routes once ready. |
| `BRIDGE_PRIMARY_PROXIES` | `primary_proxies` | `socks5h://127.0.0.1:40001` | Comma-separated managed primary proxy URLs. Use `socks5h://` for proxy-side DNS. |
| `BRIDGE_WARM_STANDBY_PROXIES` | `warm_standby_proxies` | `socks5h://127.0.0.1:40004` | Protected standby proxy URLs. |
| `BRIDGE_ACTIVE_PROXY_COUNT` | `active_proxy_count` | `1` | Number of primary nodes enabled for normal routing. |
| `BRIDGE_ALLOW_DIRECT_FALLBACK` | `allow_direct_fallback` | `false` | Direct fallback policy. It is rejected when proxy mode is configured. |
| `BRIDGE_REQUIRE_VERIFIED_EXIT_IP` | `require_verified_exit_ip` | `true` | Requires fresh verified exit identity before routing. |
| `BRIDGE_MINIMUM_UNIQUE_EXIT_IPS` | `minimum_unique_exit_ips` | `1` | Minimum unique verified public exits required for readiness. |
| `BRIDGE_IDENTITY_ENDPOINTS` | `identity_endpoints` | Cloudflare trace, ipify | Comma-separated identity endpoints queried through each proxy. |
| `BRIDGE_IDENTITY_TTL_SECS` | `identity_ttl_secs` | `300` | Exit-identity freshness period. |
| `BRIDGE_PROXY_HEALTH_INTERVAL_SECS` | `proxy_health_interval_secs` | `10` | Health and identity worker cadence. |
| `BRIDGE_PROXY_RESTART_INTERVAL_SECS` | `proxy_restart_interval_secs` | `2` | Managed restart queue cadence. |
| `BRIDGE_MAX_PROXY_RESTART_ATTEMPTS` | `max_proxy_restart_attempts` | `6` | Maximum managed restart attempts. |
| `BRIDGE_PROXY_BOOTSTRAP_TIMEOUT_SECS` | `proxy_bootstrap_timeout_secs` | `30` | Per-cycle bounded topology/bootstrap stage timeout. |
| `BRIDGE_PROXY_VERIFY_TIMEOUT_SECS` | `proxy_verify_timeout_secs` | `10` | Bounded staged proxy verification timeout. |
| `BRIDGE_PROXY_RECOVERY_BACKOFF_MAX_SECS` | `proxy_recovery_backoff_max_secs` | `120` | Maximum background reconcile recovery backoff. |
| `BRIDGE_DOCKER_BINARY` | `docker_binary` | `docker` | Container runtime executable. |
| `BRIDGE_WARP_CLI_BINARY` | `warp_cli_binary` | `warp-cli` | Optional host WARP controller executable. |
| `BRIDGE_WARP_IMAGE` | `warp_image` | `ghcr.io/mon-ius/docker-warp-socks:latest` | Managed WARP/SOCKS image. Pin a digest in controlled production deployments. |

`--no-proxy` resolves the startup command to direct mode and must not touch Docker containers.

## Search providers

Fallback order is Tavily, Exa, Serper, SearXNG, then DuckDuckGo HTML.

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_MAX_SEARCH_LOOPS` | `max_search_loops` | `5` | Maximum intercepted search iterations per request. |
| `BRIDGE_SEARCH_MAX_RESULTS` | `search_max_results` | `20` | Maximum structured results. |
| `BRIDGE_SEARCH_MAX_SNIPPET_CHARS` | `search_max_snippet_chars` | `2000` | Maximum snippet characters per result. |
| `BRIDGE_SEARCH_MAX_RESPONSE_BYTES` | `search_max_response_bytes` | `8388608` | Maximum provider response body. |
| `BRIDGE_SEARCH_TIMEOUT_SECS` | `search_timeout_secs` | `30` | Per-provider request timeout. |
| `BRIDGE_SEARCH_CHAIN_BUDGET_SECS` | `search_chain_budget_secs` | `25` | Wall-clock budget for one provider fallback-chain walk; zero is rejected. |
| `TAVILY_API_KEY` | `tavily_api_key` | unset | Tavily credential. |
| `EXA_API_KEY` | `exa_api_key` | unset | Exa credential. |
| `SERPER_API_KEY` | `serper_api_key` | unset | Serper credential. |
| `SEARXNG_URL` | `searxng_url` | unset | SearXNG base URL. |
| `SEARXNG_API_KEY` | `searxng_api_key` | unset | Optional SearXNG credential. |
| `BRIDGE_ALLOW_PRIVATE_SEARXNG` | `allow_private_searxng` | `false` | Explicitly permits private/loopback SearXNG destinations. |
| `TAVILY_API_URL` | `tavily_url` | Tavily public endpoint | Controlled endpoint override. |
| `EXA_API_URL` | `exa_url` | Exa public endpoint | Controlled endpoint override. |
| `SERPER_API_URL` | `serper_url` | Serper public endpoint | Controlled endpoint override. |
| `DUCKDUCKGO_SEARCH_URL` | `duckduckgo_url` | DuckDuckGo HTML endpoint | Controlled endpoint override. |
| `YAHOO_SEARCH_URL` | `yahoo_url` | Yahoo search endpoint | Keyless scraper fallback used after DuckDuckGo returns no usable result/captcha. |

Private SearXNG expands SSRF reach and is rejected unless `BRIDGE_ALLOW_PRIVATE_SEARXNG=true` or `allow_private_searxng = true` is explicitly configured.

## Request history

History is disabled by default. When enabled, content is stored locally in SQLite using the configured capture mode and bounded per-section/total sizes.

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_HISTORY_ENABLED` | `history_enabled` | `false` | Enable request-history persistence. |
| `BRIDGE_HISTORY_CAPTURE_MODE` | `history_capture_mode` | `redacted` | `off`, `metadata`, `redacted`, or `full`. |
| `BRIDGE_HISTORY_CAPTURE_INBOUND` | `history_capture_inbound` | `true` | Capture bounded inbound request content. |
| `BRIDGE_HISTORY_CAPTURE_EFFECTIVE` | `history_capture_effective` | `true` | Capture bounded effective upstream payload. |
| `BRIDGE_HISTORY_CAPTURE_REASONING` | `history_capture_reasoning` | `true` | Capture bounded reasoning content. |
| `BRIDGE_HISTORY_CAPTURE_RESPONSE` | `history_capture_response` | `true` | Capture bounded response content. |
| `BRIDGE_HISTORY_CAPTURE_TOOLS` | `history_capture_tools` | `true` | Capture bounded tool metadata/payloads according to mode. |
| `BRIDGE_HISTORY_CAPTURE_SEARCH_QUERIES` | `history_capture_search_queries` | `true` | Capture search queries according to mode. |
| `BRIDGE_HISTORY_CAPTURE_SEARCH_RESULTS` | `history_capture_search_results` | `false` | Capture bounded search-result bodies; disabled by default. |
| `BRIDGE_HISTORY_CAPTURE_SHELL_COMMANDS` | `history_capture_shell_commands` | `false` | Capture delegated shell command content; disabled by default. |
| `BRIDGE_HISTORY_RETENTION_DAYS` | `history_retention_days` | `30` | Age retention window; `0` disables age-based eviction. |
| `BRIDGE_HISTORY_MAX_RECORDS` | `history_max_records` | `1000000` | Maximum retained request rows. |
| `BRIDGE_HISTORY_MAX_DATABASE_BYTES` | `history_max_database_bytes` | `17179869184` | Logical stored-byte cap; oldest records are evicted at runtime. |
| `BRIDGE_HISTORY_MAX_REQUEST_BYTES` | `history_max_request_bytes` | `8388608` | Per-request inbound/effective capture bound. |
| `BRIDGE_HISTORY_MAX_REASONING_BYTES` | `history_max_reasoning_bytes` | `16777216` | Per-request reasoning capture bound. |
| `BRIDGE_HISTORY_MAX_RESPONSE_BYTES` | `history_max_response_bytes` | `2097152` | Per-request response capture bound. |
| `BRIDGE_HISTORY_MAX_TOOL_PAYLOAD_BYTES` | `history_max_tool_payload_bytes` | `4194304` | Per-request tool/search payload bound. |
| `BRIDGE_HISTORY_MAX_RECORD_BYTES` | `history_max_record_bytes` | `50331648` | Aggregate captured-content bound per request. |
| `BRIDGE_HISTORY_QUEUE_CAPACITY` | `history_queue_capacity` | `8192` | Bounded asynchronous history writer queue. |
| `BRIDGE_HISTORY_PATH` | `history_path` | user history directory | Override SQLite path. |

## Runtime and observability

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `RUNTIME_DIR` | `runtime_dir` | platform user runtime path | PID, logs, and supervisor state root. |
| `BRIDGE_WORKER_SHUTDOWN_TIMEOUT_SECS` | `worker_shutdown_timeout_secs` | `30` | Worker cancellation/join deadline. |
| `BRIDGE_SERVER_SHUTDOWN_TIMEOUT_SECS` | `server_shutdown_timeout_secs` | `30` | HTTP graceful-shutdown deadline. |
| `BRIDGE_RATE_LIMIT` | `rate_limit` | unset | Maximum concurrent requests. |
| `BRIDGE_METRICS_ENABLED` | `metrics_enabled` | `true` | Enables authenticated metrics snapshot. |
| `BRIDGE_REQUEST_ID_HEADER` | `request_id_header` | `x-request-id` | Correlation header accepted and echoed after validation. |

## Validation rules

Startup fails before socket binding when any of these conditions is detected:

- non-loopback binding without strong client and dashboard authentication;
- unrestricted shell policy on a public bind;
- proxy mode without configured nodes;
- direct fallback enabled in proxy mode;
- required unique exit count greater than configured nodes;
- invalid retry or buffer bounds;
- provider endpoint with embedded credentials;
- private SearXNG without explicit opt-in;
- future unsupported config schema version.

## Safe inspection

```bash
opencode2api server config
opencode2api --json server config
```

These outputs redact secrets. Raw secret export is intentionally not part of the normal management contract.
