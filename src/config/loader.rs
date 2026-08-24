//! Deterministic configuration resolution.

use super::{
    BridgeConfig, CliOverrides, EgressMode, HistoryCaptureMode, SecretString, TomlConfig,
    DEFAULT_BRIDGE_PORT, DEFAULT_CHANNEL_CAPACITY, DEFAULT_HOST, DEFAULT_MAX_BODY_SIZE,
    DEFAULT_OPENCODE_PORT, DEFAULT_PRIMARY_PROXIES, DEFAULT_SHELL_ALLOWLIST,
    DEFAULT_STREAM_BUFFER_SIZE, DEFAULT_WARM_STANDBY_PROXIES,
};
use crate::shell::ShellPolicy;
use std::collections::HashSet;
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::warn;

pub(super) fn load(overrides: CliOverrides) -> BridgeConfig {
    let mut resolved = BridgeConfig::default();
    let config_path = overrides
        .config_path
        .clone()
        .or_else(|| env_string("BRIDGE_CONFIG_PATH"))
        .unwrap_or_else(|| "opencode2api.toml".to_string());
    let file = TomlConfig::from_file(&config_path);

    resolved.host = resolve_host(
        overrides.host,
        file.as_ref().and_then(|cfg| cfg.host.clone()),
    );
    if resolved.host.is_unspecified() {
        warn!("bridge is binding to all network interfaces; strong authentication is required");
    }

    resolved.bridge_port = overrides
        .bridge_port
        .or_else(|| env_parse("BRIDGE_PORT"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.port))
        .unwrap_or(DEFAULT_BRIDGE_PORT);
    resolved.opencode_port = env_parse("OPENCODE_PORT")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.opencode_port))
        .unwrap_or(DEFAULT_OPENCODE_PORT);
    resolved.model = overrides
        .model
        .or_else(|| env_string("OPENCODE_MODEL"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.model.clone()));

    resolved.shell_policy = resolve_shell_policy(
        overrides
            .shell_policy
            .or_else(|| env_string("BRIDGE_SHELL_POLICY"))
            .or_else(|| file.as_ref().and_then(|cfg| cfg.shell_policy.clone()))
            .unwrap_or_else(|| "disabled".to_string()),
        env_string("BRIDGE_SHELL_ALLOWLIST")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.shell_allowlist.clone()))
            .unwrap_or_else(|| DEFAULT_SHELL_ALLOWLIST.to_string()),
    );

    resolved.auth_tokens = env_string("BRIDGE_AUTH_TOKEN")
        .and_then(|value| parse_secret_csv_optional(Some(value)))
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.auth_tokens.clone())
                .map(|value| {
                    value
                        .into_vec()
                        .into_iter()
                        .filter_map(SecretString::new)
                        .collect::<Vec<_>>()
                })
                .filter(|tokens| !tokens.is_empty())
        });
    resolved.max_body_size = overrides
        .max_body_size
        .or_else(|| env_parse("BRIDGE_MAX_BODY_SIZE"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_body_size))
        .unwrap_or(DEFAULT_MAX_BODY_SIZE);
    resolved.stream_buffer_size = env_parse("BRIDGE_STREAM_BUFFER_SIZE")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.stream_buffer_size))
        .unwrap_or(DEFAULT_STREAM_BUFFER_SIZE);
    resolved.channel_capacity = env_parse("BRIDGE_CHANNEL_CAPACITY")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.channel_capacity))
        .unwrap_or(DEFAULT_CHANNEL_CAPACITY);
    if resolved.max_body_size == 0 {
        warn!("request body limit is disabled (BRIDGE_MAX_BODY_SIZE=0)");
    }

    resolved.tavily_api_key = secret_option(
        overrides
            .tavily_api_key
            .or_else(|| env_string("TAVILY_API_KEY"))
            .or_else(|| file.as_ref().and_then(|cfg| cfg.tavily_api_key.clone())),
    );
    resolved.exa_api_key = secret_option(
        overrides
            .exa_api_key
            .or_else(|| env_string("EXA_API_KEY"))
            .or_else(|| file.as_ref().and_then(|cfg| cfg.exa_api_key.clone())),
    );
    resolved.serper_api_key = secret_option(
        overrides
            .serper_api_key
            .or_else(|| env_string("SERPER_API_KEY"))
            .or_else(|| file.as_ref().and_then(|cfg| cfg.serper_api_key.clone())),
    );
    resolved.searxng_url = overrides
        .searxng_url
        .or_else(|| env_string("SEARXNG_URL"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.searxng_url.clone()));
    resolved.searxng_api_key = secret_option(
        overrides
            .searxng_api_key
            .or_else(|| env_string("SEARXNG_API_KEY"))
            .or_else(|| file.as_ref().and_then(|cfg| cfg.searxng_api_key.clone())),
    );
    resolved.max_search_loops = env_parse("BRIDGE_MAX_SEARCH_LOOPS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_search_loops))
        .unwrap_or(20);

    let legacy_proxy_value = env_string("BRIDGE_PROXIES").or_else(|| {
        file.as_ref()
            .and_then(|cfg| cfg.proxies.as_ref().map(|items| items.join(",")))
    });
    resolved.proxies = parse_csv_optional(legacy_proxy_value.clone());
    resolved.primary_proxies = parse_csv_optional(Some(
        env_string("BRIDGE_PRIMARY_PROXIES")
            .or(legacy_proxy_value)
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.primary_proxies.as_ref().map(|items| items.join(",")))
            })
            .unwrap_or_else(|| DEFAULT_PRIMARY_PROXIES.to_string()),
    ));
    resolved.warm_standby_proxies = parse_csv_optional(Some(
        env_string("BRIDGE_WARM_STANDBY_PROXIES")
            .or_else(|| {
                file.as_ref().and_then(|cfg| {
                    cfg.warm_standby_proxies
                        .as_ref()
                        .map(|items| items.join(","))
                })
            })
            .unwrap_or_else(|| DEFAULT_WARM_STANDBY_PROXIES.to_string()),
    ));

    resolved.management.config_path = PathBuf::from(config_path);
    resolved.management.dashboard_token =
        secret_option(env_string("DASHBOARD_ADMIN_TOKEN").or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.dashboard_admin_token.clone())
        }));
    resolved.management.rest_api_token = secret_option(
        env_string("REST_API_TOKEN")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.rest_api_token.clone())),
    );
    resolved.management.csrf_enabled = env_bool("DASHBOARD_CSRF_ENABLED")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.csrf_enabled))
        .unwrap_or(true);

    resolved.observability.max_concurrent_requests =
        env_parse("BRIDGE_RATE_LIMIT").or_else(|| file.as_ref().and_then(|cfg| cfg.rate_limit));
    resolved.observability.metrics_enabled = env_bool("BRIDGE_METRICS_ENABLED")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.metrics_enabled))
        .unwrap_or(true);
    resolved.observability.request_id_header = env_string("BRIDGE_REQUEST_ID_HEADER")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.request_id_header.clone()))
        .unwrap_or_else(|| "x-request-id".to_string());

    resolved.history.enabled = env_bool("BRIDGE_HISTORY_ENABLED")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_enabled))
        .unwrap_or(false);
    resolved.history.capture_mode = env_string("BRIDGE_HISTORY_CAPTURE_MODE")
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.history_capture_mode.clone())
        })
        .as_deref()
        .and_then(HistoryCaptureMode::parse)
        .unwrap_or(HistoryCaptureMode::Redacted);
    resolved.history.capture_inbound = env_bool("BRIDGE_HISTORY_CAPTURE_INBOUND")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_capture_inbound))
        .unwrap_or(true);
    resolved.history.capture_effective = env_bool("BRIDGE_HISTORY_CAPTURE_EFFECTIVE")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_capture_effective))
        .unwrap_or(true);
    resolved.history.capture_reasoning = env_bool("BRIDGE_HISTORY_CAPTURE_REASONING")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_capture_reasoning))
        .unwrap_or(true);
    resolved.history.capture_response = env_bool("BRIDGE_HISTORY_CAPTURE_RESPONSE")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_capture_response))
        .unwrap_or(true);
    resolved.history.capture_tools = env_bool("BRIDGE_HISTORY_CAPTURE_TOOLS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_capture_tools))
        .unwrap_or(true);
    resolved.history.capture_search_queries = env_bool("BRIDGE_HISTORY_CAPTURE_SEARCH_QUERIES")
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.history_capture_search_queries)
        })
        .unwrap_or(true);
    resolved.history.capture_search_results = env_bool("BRIDGE_HISTORY_CAPTURE_SEARCH_RESULTS")
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.history_capture_search_results)
        })
        .unwrap_or(false);
    resolved.history.capture_shell_commands = env_bool("BRIDGE_HISTORY_CAPTURE_SHELL_COMMANDS")
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.history_capture_shell_commands)
        })
        .unwrap_or(false);
    resolved.history.retention_days = env_parse("BRIDGE_HISTORY_RETENTION_DAYS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_retention_days))
        .unwrap_or(30);
    resolved.history.max_records = env_parse("BRIDGE_HISTORY_MAX_RECORDS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_max_records))
        .unwrap_or(1_000_000)
        .max(1);
    resolved.history.max_database_bytes = env_parse("BRIDGE_HISTORY_MAX_DATABASE_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_max_database_bytes))
        .unwrap_or(16 * 1024 * 1024 * 1024);
    resolved.history.max_request_bytes = env_parse("BRIDGE_HISTORY_MAX_REQUEST_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_max_request_bytes))
        .unwrap_or(8 * 1024 * 1024);
    resolved.history.max_reasoning_bytes = env_parse("BRIDGE_HISTORY_MAX_REASONING_BYTES")
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.history_max_reasoning_bytes)
        })
        .unwrap_or(16 * 1024 * 1024);
    resolved.history.max_response_bytes = env_parse("BRIDGE_HISTORY_MAX_RESPONSE_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_max_response_bytes))
        .unwrap_or(2 * 1024 * 1024);
    resolved.history.max_tool_payload_bytes = env_parse("BRIDGE_HISTORY_MAX_TOOL_PAYLOAD_BYTES")
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.history_max_tool_payload_bytes)
        })
        .unwrap_or(4 * 1024 * 1024);
    resolved.history.max_record_bytes = env_parse("BRIDGE_HISTORY_MAX_RECORD_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_max_record_bytes))
        .unwrap_or(48 * 1024 * 1024);
    resolved.history.queue_capacity = env_parse("BRIDGE_HISTORY_QUEUE_CAPACITY")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_queue_capacity))
        .unwrap_or(8192)
        .max(1);
    resolved.history.path = env_string("BRIDGE_HISTORY_PATH")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.history_path.clone()))
        .map(PathBuf::from);

    resolved.protocol.min_reasoning_stream_tokens =
        env_parse::<u32>("BRIDGE_MIN_REASONING_STREAM_TOKENS")
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.min_reasoning_stream_tokens)
            })
            .filter(|value| *value > 0)
            .unwrap_or(1024);
    resolved.protocol.max_sse_line_bytes = env_parse("BRIDGE_MAX_SSE_LINE_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_sse_line_bytes))
        .unwrap_or(4 * 1024 * 1024);
    resolved.protocol.max_sync_response_bytes = env_parse("BRIDGE_MAX_SYNC_RESPONSE_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_sync_response_bytes))
        .unwrap_or(32 * 1024 * 1024);

    resolved.search.max_results = env_parse("BRIDGE_SEARCH_MAX_RESULTS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.search_max_results))
        .unwrap_or(20);
    resolved.search.max_snippet_chars = env_parse("BRIDGE_SEARCH_MAX_SNIPPET_CHARS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.search_max_snippet_chars))
        .unwrap_or(2000);
    resolved.search.max_response_bytes = env_parse("BRIDGE_SEARCH_MAX_RESPONSE_BYTES")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.search_max_response_bytes))
        .unwrap_or(8 * 1024 * 1024);
    resolved.search.request_timeout = Duration::from_secs(
        env_parse("BRIDGE_SEARCH_TIMEOUT_SECS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.search_timeout_secs))
            .unwrap_or(30),
    );
    resolved.search.allow_private_searxng = env_bool("BRIDGE_ALLOW_PRIVATE_SEARXNG")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.allow_private_searxng))
        .unwrap_or(false);
    resolved.search.tavily_url = env_string("TAVILY_API_URL")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.tavily_url.clone()))
        .unwrap_or_else(|| "https://api.tavily.com/search".to_string());
    resolved.search.exa_url = env_string("EXA_API_URL")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.exa_url.clone()))
        .unwrap_or_else(|| "https://api.exa.ai/search".to_string());
    resolved.search.serper_url = env_string("SERPER_API_URL")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.serper_url.clone()))
        .unwrap_or_else(|| "https://google.serper.dev/search".to_string());
    resolved.search.duckduckgo_url = env_string("DUCKDUCKGO_SEARCH_URL")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.duckduckgo_url.clone()))
        .unwrap_or_else(|| "https://html.duckduckgo.com/html/".to_string());
    resolved.search.yahoo_url = env_string("YAHOO_SEARCH_URL")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.yahoo_url.clone()))
        .unwrap_or_else(|| "https://search.yahoo.com/search".to_string());

    resolved.retry.upstream_base_url = env_string("OPENCODE_UPSTREAM_BASE_URL")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.upstream_base_url.clone()))
        .unwrap_or_else(|| "https://opencode.ai/zen/v1".to_string())
        .trim_end_matches('/')
        .to_string();
    resolved.retry.model_fallbacks = env_string("OPENCODE_MODEL_FALLBACKS")
        .map(|value| parse_csv(&value))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.model_fallbacks.clone()))
        .unwrap_or_default();
    resolved.retry.default_fallbacks_enabled = env_bool("OPENCODE_ENABLE_DEFAULT_FALLBACKS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.enable_default_fallbacks))
        .unwrap_or(false);
    resolved.retry.max_network_attempts = env_parse("BRIDGE_MAX_NETWORK_ATTEMPTS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_network_attempts))
        .unwrap_or(8);
    resolved.retry.max_provider_attempts = env_parse("BRIDGE_MAX_PROVIDER_ATTEMPTS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_provider_attempts))
        .unwrap_or(2);
    resolved.retry.base_backoff = Duration::from_millis(
        env_parse("BRIDGE_RETRY_BASE_BACKOFF_MS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.retry_base_backoff_ms))
            .unwrap_or(1_000),
    );
    resolved.retry.max_backoff = Duration::from_millis(
        env_parse("BRIDGE_RETRY_MAX_BACKOFF_MS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.retry_max_backoff_ms))
            .unwrap_or(30_000),
    );

    resolved.egress.mode = overrides
        .egress_mode
        .or_else(|| env_string("BRIDGE_EGRESS_MODE"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.egress_mode.clone()))
        .as_deref()
        .and_then(EgressMode::parse)
        .unwrap_or(EgressMode::Proxy);
    resolved.egress.active_proxy_count = env_parse("BRIDGE_ACTIVE_PROXY_COUNT")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.active_proxy_count))
        .unwrap_or(1);
    resolved.egress.require_verified_exit_ip = env_bool("BRIDGE_REQUIRE_VERIFIED_EXIT_IP")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.require_verified_exit_ip))
        .unwrap_or(true);
    resolved.egress.minimum_unique_exit_ips = env_parse("BRIDGE_MINIMUM_UNIQUE_EXIT_IPS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.minimum_unique_exit_ips))
        .unwrap_or(1);
    resolved.egress.identity_endpoints = env_string("BRIDGE_IDENTITY_ENDPOINTS")
        .map(|value| parse_csv(&value))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.identity_endpoints.clone()))
        .unwrap_or_else(|| BridgeConfig::default().egress.identity_endpoints);
    resolved.egress.identity_ttl = Duration::from_secs(
        env_parse("BRIDGE_IDENTITY_TTL_SECS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.identity_ttl_secs))
            .unwrap_or(300),
    );
    resolved.egress.health_interval = Duration::from_secs(
        env_parse("BRIDGE_PROXY_HEALTH_INTERVAL_SECS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.proxy_health_interval_secs))
            .unwrap_or(10),
    );
    resolved.egress.restart_interval = Duration::from_secs(
        env_parse("BRIDGE_PROXY_RESTART_INTERVAL_SECS")
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.proxy_restart_interval_secs)
            })
            .unwrap_or(2),
    );
    resolved.egress.max_restart_attempts = env_parse("BRIDGE_MAX_PROXY_RESTART_ATTEMPTS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_proxy_restart_attempts))
        .unwrap_or(6);
    resolved.egress.allow_direct_fallback = env_bool("BRIDGE_ALLOW_DIRECT_FALLBACK")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.allow_direct_fallback))
        .unwrap_or(false);
    resolved.egress.bootstrap_timeout = Duration::from_secs(
        env_parse("BRIDGE_PROXY_BOOTSTRAP_TIMEOUT_SECS")
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.proxy_bootstrap_timeout_secs)
            })
            .unwrap_or(30),
    );
    resolved.egress.verify_timeout = Duration::from_secs(
        env_parse("BRIDGE_PROXY_VERIFY_TIMEOUT_SECS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.proxy_verify_timeout_secs))
            .unwrap_or(10),
    );
    resolved.egress.recovery_backoff_max = Duration::from_secs(
        env_parse("BRIDGE_PROXY_RECOVERY_BACKOFF_MAX_SECS")
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.proxy_recovery_backoff_max_secs)
            })
            .unwrap_or(120),
    );

    resolved.runtime.runtime_dir = env_string("RUNTIME_DIR")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.runtime_dir.clone()))
        .map(PathBuf::from);
    resolved.runtime.docker_binary = env_string("BRIDGE_DOCKER_BINARY")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.docker_binary.clone()))
        .unwrap_or_else(|| "docker".to_string());
    resolved.runtime.warp_cli_binary = env_string("BRIDGE_WARP_CLI_BINARY")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.warp_cli_binary.clone()))
        .unwrap_or_else(|| "warp-cli".to_string());
    resolved.runtime.warp_image = env_string("BRIDGE_WARP_IMAGE")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.warp_image.clone()))
        .unwrap_or_else(|| "ghcr.io/mon-ius/docker-warp-socks:latest".to_string());
    resolved.runtime.worker_shutdown_timeout = Duration::from_secs(
        env_parse("BRIDGE_WORKER_SHUTDOWN_TIMEOUT_SECS")
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.worker_shutdown_timeout_secs)
            })
            .unwrap_or(30),
    );
    resolved.runtime.server_shutdown_timeout = Duration::from_secs(
        env_parse("BRIDGE_SERVER_SHUTDOWN_TIMEOUT_SECS")
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.server_shutdown_timeout_secs)
            })
            .unwrap_or(30),
    );

    resolved
}

fn resolve_host(cli_value: Option<String>, file_value: Option<String>) -> IpAddr {
    cli_value
        .or_else(|| env_string("BRIDGE_HOST"))
        .or(file_value)
        .unwrap_or_else(|| DEFAULT_HOST.to_string())
        .parse()
        .unwrap_or_else(|_| {
            DEFAULT_HOST
                .parse()
                .expect("hardcoded default host must be valid")
        })
}

fn resolve_shell_policy(raw_policy: String, allowlist: String) -> ShellPolicy {
    match raw_policy.to_ascii_lowercase().as_str() {
        "disabled" => ShellPolicy::Disabled,
        "allowlist" => ShellPolicy::AllowList(
            parse_csv(&allowlist)
                .into_iter()
                .collect::<HashSet<String>>(),
        ),
        "unrestricted" => ShellPolicy::Unrestricted,
        _ => {
            warn!(
                "unknown shell policy '{}'; defaulting to disabled",
                raw_policy
            );
            ShellPolicy::Disabled
        }
    }
}

fn secret_option(value: Option<String>) -> Option<SecretString> {
    value.and_then(SecretString::new)
}

fn parse_secret_csv_optional(value: Option<String>) -> Option<Vec<SecretString>> {
    value
        .map(|raw| {
            parse_csv(&raw)
                .into_iter()
                .filter_map(SecretString::new)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
}

fn env_string(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn env_parse<T>(name: &str) -> Option<T>
where
    T: FromStr,
{
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_bool(name: &str) -> Option<bool> {
    env_string(name).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    })
}

fn parse_csv_optional(value: Option<String>) -> Option<Vec<String>> {
    value
        .map(|raw| parse_csv(&raw))
        .filter(|items| !items.is_empty())
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(super::normalize_proxy_url)
        .collect()
}
