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
| `BRIDGE_MAX_BODY_SIZE` | `max_body_size` | `10485760` | Incoming `/v1/*` request limit in bytes; `0` disables the limit. |
| `BRIDGE_STREAM_BUFFER_SIZE` | `stream_buffer_size` | `4096` | Initial upstream SSE line-buffer capacity. |
| `BRIDGE_CHANNEL_CAPACITY` | `channel_capacity` | `256` | Bounded downstream SSE channel capacity. |
| `BRIDGE_MAX_SSE_LINE_BYTES` | `max_sse_line_bytes` | `262144` | Maximum upstream SSE line size. |
| `BRIDGE_MAX_SYNC_RESPONSE_BYTES` | `max_sync_response_bytes` | `4194304` | Maximum non-streaming upstream response body. |
| `BRIDGE_MIN_REASONING_STREAM_TOKENS` | `min_reasoning_stream_tokens` | `1024` | Minimum mapped token budget for reasoning streams. |
| `OPENCODE_PORT` | `opencode_port` | `4096` | Optional local OpenCode daemon probe port used by diagnostics. |
| `OPENCODE_MODEL` | `model` | `claude-3-5-sonnet` | Default upstream model identifier. |
| `OPENCODE_UPSTREAM_BASE_URL` | `upstream_base_url` | `https://opencode.ai/zen/v1` | OpenAI-compatible upstream base URL. |

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
| `BRIDGE_MAX_NETWORK_ATTEMPTS` | `max_network_attempts` | `5` | Transport/rate/provider-server retry budget. |
| `BRIDGE_MAX_PROVIDER_ATTEMPTS` | `max_provider_attempts` | `1` | Retry budget for non-rate-limit provider client errors. |
| `BRIDGE_RETRY_BASE_BACKOFF_MS` | `retry_base_backoff_ms` | `2000` | Initial jittered backoff. |
| `BRIDGE_RETRY_MAX_BACKOFF_MS` | `retry_max_backoff_ms` | `16000` | Maximum jittered backoff. |

Model fallback and transport retry are accounted independently. Provider errors do not mark a healthy proxy as a transport failure.

## Egress and WARP pool

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_EGRESS_MODE` | `egress_mode` | `proxy` | `proxy` or `direct`. Proxy mode fails closed. |
| `BRIDGE_PRIMARY_PROXIES` | `primary_proxies` | ports `40001-40003` | Comma-separated managed primary proxy URLs. Use `socks5h://` for proxy-side DNS. |
| `BRIDGE_WARM_STANDBY_PROXIES` | `warm_standby_proxies` | ports `40004-40005` | Protected standby proxy URLs. |
| `BRIDGE_ACTIVE_PROXY_COUNT` | `active_proxy_count` | `3` | Number of primary nodes enabled for normal routing. |
| `BRIDGE_ALLOW_DIRECT_FALLBACK` | `allow_direct_fallback` | `false` | Direct fallback policy. It is rejected when proxy mode is configured. |
| `BRIDGE_REQUIRE_VERIFIED_EXIT_IP` | `require_verified_exit_ip` | `false` | Requires fresh verified exit identity before routing. |
| `BRIDGE_MINIMUM_UNIQUE_EXIT_IPS` | `minimum_unique_exit_ips` | `1` | Minimum unique verified public exits required for readiness. |
| `BRIDGE_IDENTITY_ENDPOINTS` | `identity_endpoints` | Cloudflare trace, ipify | Comma-separated identity endpoints queried through each proxy. |
| `BRIDGE_IDENTITY_TTL_SECS` | `identity_ttl_secs` | `300` | Exit-identity freshness period. |
| `BRIDGE_PROXY_HEALTH_INTERVAL_SECS` | `proxy_health_interval_secs` | `10` | Health and identity worker cadence. |
| `BRIDGE_PROXY_RESTART_INTERVAL_SECS` | `proxy_restart_interval_secs` | `2` | Managed restart queue cadence. |
| `BRIDGE_MAX_PROXY_RESTART_ATTEMPTS` | `max_proxy_restart_attempts` | `3` | Maximum managed restart attempts. |
| `BRIDGE_DOCKER_BINARY` | `docker_binary` | `docker` | Container runtime executable. |
| `BRIDGE_WARP_CLI_BINARY` | `warp_cli_binary` | `warp-cli` | Optional host WARP controller executable. |
| `BRIDGE_WARP_IMAGE` | `warp_image` | `ghcr.io/mon-ius/docker-warp-socks:latest` | Managed WARP/SOCKS image. Pin a digest in controlled production deployments. |

`--no-proxy` resolves the startup command to direct mode and must not touch Docker containers.

## Search providers

Fallback order is Tavily, Exa, Serper, SearXNG, then DuckDuckGo HTML.

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `BRIDGE_MAX_SEARCH_LOOPS` | `max_search_loops` | `5` | Maximum intercepted search iterations per request. |
| `BRIDGE_SEARCH_MAX_RESULTS` | `search_max_results` | `5` | Maximum structured results. |
| `BRIDGE_SEARCH_MAX_SNIPPET_CHARS` | `search_max_snippet_chars` | `500` | Maximum snippet characters per result. |
| `BRIDGE_SEARCH_MAX_RESPONSE_BYTES` | `search_max_response_bytes` | `1048576` | Maximum provider response body. |
| `BRIDGE_SEARCH_TIMEOUT_SECS` | `search_timeout_secs` | `15` | Per-provider request timeout. |
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

Private SearXNG expands SSRF reach and is rejected unless `BRIDGE_ALLOW_PRIVATE_SEARXNG=true` or `allow_private_searxng = true` is explicitly configured.

## Runtime and observability

| Environment variable | TOML key | Default | Purpose |
|---|---|---:|---|
| `RUNTIME_DIR` | `runtime_dir` | platform user runtime path | PID, logs, and supervisor state root. |
| `BRIDGE_WORKER_SHUTDOWN_TIMEOUT_SECS` | `worker_shutdown_timeout_secs` | `10` | Worker cancellation/join deadline. |
| `BRIDGE_SERVER_SHUTDOWN_TIMEOUT_SECS` | `server_shutdown_timeout_secs` | `15` | HTTP graceful-shutdown deadline. |
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
