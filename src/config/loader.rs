//! Deterministic configuration resolution.

use super::{
    BridgeConfig, CliOverrides, EgressMode, HistoryCaptureMode, SecretString, StringList,
    TomlConfig, DEFAULT_BRIDGE_PORT, DEFAULT_CHANNEL_CAPACITY, DEFAULT_HOST, DEFAULT_MAX_BODY_SIZE,
    DEFAULT_OPENCODE_PORT, DEFAULT_PRIMARY_PROXIES, DEFAULT_SHELL_ALLOWLIST,
    DEFAULT_STREAM_BUFFER_SIZE, DEFAULT_WARM_STANDBY_PROXIES,
};
use crate::shell::ShellPolicy;
use std::collections::HashSet;
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::warn;

/// Ambient process environment belongs to the configuration/bootstrap
/// boundary even when it controls shell integration or presentation rather
/// than BridgeConfig fields. Keeping these reads here makes process-dependent
/// behavior auditable from one source boundary.
pub(crate) fn ambient_home() -> Option<std::ffi::OsString> {
    std::env::var_os("HOME")
}

pub(crate) fn ambient_zdotdir() -> Option<std::ffi::OsString> {
    std::env::var_os("ZDOTDIR").filter(|value| !value.is_empty())
}

pub(crate) fn ambient_shell() -> String {
    std::env::var("SHELL").unwrap_or_default()
}

pub(crate) fn terminal_columns() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

static PRE_DOTENV_UPSTREAM_ENV_OVERRIDE: OnceLock<bool> = OnceLock::new();

fn upstream_env_override_present() -> bool {
    [
        "OPENCODE_UPSTREAM_BASE_URL",
        "BRIDGE_UPSTREAM_BASE_URL",
        "OPENCODE_UPSTREAM_API_KEY",
        "BRIDGE_UPSTREAM_API_KEY",
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// True when an upstream URL/API-key override already existed in the parent
/// process environment before dotenv loading. Persistent config commands
/// cannot remove such parent-shell state and must fail closed rather than
/// claim that a provider switch took effect.
pub(crate) fn pre_dotenv_upstream_env_override_present() -> bool {
    *PRE_DOTENV_UPSTREAM_ENV_OVERRIDE.get_or_init(upstream_env_override_present)
}

pub(crate) fn load_dotenv() -> Option<std::path::PathBuf> {
    let _ = PRE_DOTENV_UPSTREAM_ENV_OVERRIDE.get_or_init(upstream_env_override_present);
    if let Some(explicit) = std::env::var_os("BRIDGE_ENV_PATH") {
        let path = std::path::PathBuf::from(explicit);
        if path.is_file() && dotenvy::from_path(&path).is_ok() {
            return Some(path);
        }
    }

    if let Ok(path) = dotenvy::dotenv() {
        return Some(path);
    }

    let executable = std::env::current_exe().ok()?;
    for directory in executable.parent()?.ancestors() {
        let candidate = directory.join(".env");
        if candidate.is_file() && dotenvy::from_path(&candidate).is_ok() {
            return Some(candidate);
        }
    }

    None
}

pub(super) fn load(overrides: CliOverrides) -> BridgeConfig {
    let mut resolved = BridgeConfig::default();
    let config_path = overrides
        .config_path
        .clone()
        .or_else(|| env_string("BRIDGE_CONFIG_PATH"))
        .unwrap_or_else(|| {
            let local = std::path::Path::new("opencode2api.toml");
            if local.exists() {
                "opencode2api.toml".to_string()
            } else if !cfg!(test) {
                if let Some(home) = ambient_home() {
                    let home_buf = std::path::PathBuf::from(home);
                    let home_path = home_buf.join("opencode2api.toml");
                    if home_path.exists() {
                        return home_path.to_string_lossy().to_string();
                    }
                    let dot_config = home_buf
                        .join(".config")
                        .join("opencode2api")
                        .join("config.toml");
                    if dot_config.exists() {
                        return dot_config.to_string_lossy().to_string();
                    }
                }
                "opencode2api.toml".to_string()
            } else {
                "opencode2api.toml".to_string()
            }
        });
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

    // Legacy `proxies` vs specific `primary_proxies` resolution.
    //
    // Changelog (2026-08-26): within the SAME source the SPECIFIC key
    // (`primary_proxies`) now wins over the legacy alias (`proxies`). This
    // flips the previous behavior where the legacy key silently shadowed the
    // newer, more specific one when both appeared in one TOML document.
    // Cross-source precedence is unchanged: environment still beats TOML,
    // and a lone legacy key keeps feeding the primary pool as before.
    //
    // Sources are compared AFTER parsing: a source that yields zero proxies
    // (comma-only text, an empty array) configures nothing and must not flip
    // the explicit-configuration flag that gates `egress_mode="proxy"`.
    let env_primary_proxies = parse_csv_optional(env_string("BRIDGE_PRIMARY_PROXIES"));
    let env_legacy_proxies = parse_csv_optional(env_string("BRIDGE_PROXIES"));
    let toml_primary_proxies = file
        .as_ref()
        .and_then(|cfg| cfg.primary_proxies.clone())
        .map(StringList::into_vec)
        .and_then(normalized_list);
    let toml_legacy_proxies = file
        .as_ref()
        .and_then(|cfg| cfg.proxies.clone())
        .map(StringList::into_vec)
        .and_then(normalized_list);
    let toml_legacy_effective = match (&toml_primary_proxies, &toml_legacy_proxies) {
        (Some(_), Some(_)) => {
            warn!(
                "TOML config declares both legacy 'proxies' and 'primary_proxies'; \
                 preferring 'primary_proxies' and ignoring legacy 'proxies'"
            );
            None
        }
        (_, other) => other.clone(),
    };
    let proxies_explicitly_configured = env_primary_proxies.is_some()
        || env_legacy_proxies.is_some()
        || toml_primary_proxies.is_some()
        || toml_legacy_proxies.is_some();
    resolved.proxies = env_legacy_proxies.clone().or(toml_legacy_effective.clone());
    resolved.primary_proxies = Some(
        env_primary_proxies
            .or(env_legacy_proxies)
            .or(toml_primary_proxies)
            .or(toml_legacy_effective)
            .unwrap_or_else(|| parse_csv(DEFAULT_PRIMARY_PROXIES)),
    );
    resolved.egress.proxies_explicitly_configured = proxies_explicitly_configured;
    resolved.warm_standby_proxies = Some(
        parse_csv_optional(env_string("BRIDGE_WARM_STANDBY_PROXIES"))
            .or_else(|| {
                file.as_ref()
                    .and_then(|cfg| cfg.warm_standby_proxies.clone())
                    .map(StringList::into_vec)
                    .and_then(normalized_list)
            })
            .unwrap_or_else(|| parse_csv(DEFAULT_WARM_STANDBY_PROXIES)),
    );

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
    resolved.history.capture_mode = match env_string("BRIDGE_HISTORY_CAPTURE_MODE").or_else(|| {
        file.as_ref()
            .and_then(|cfg| cfg.history_capture_mode.clone())
    }) {
        Some(value) => HistoryCaptureMode::parse(&value).unwrap_or_else(|| {
            warn!(
                "unknown history capture mode '{value}'; defaulting to redacted \
                     (valid: off/disabled, metadata, redacted, full)"
            );
            HistoryCaptureMode::Redacted
        }),
        None => HistoryCaptureMode::Redacted,
    };
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
    resolved.search.chain_budget = Duration::from_secs(
        env_parse("BRIDGE_SEARCH_CHAIN_BUDGET_SECS")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.search_chain_budget_secs))
            // A zero budget would starve every provider attempt before the
            // first response can arrive; reject the configured value (env or
            // TOML) with a warning and fall back to the default instead.
            .filter(|secs| {
                if *secs == 0 {
                    warn!(
                        "ignoring search chain budget of 0 seconds \
                         (BRIDGE_SEARCH_CHAIN_BUDGET_SECS / TOML \
                         'search_chain_budget_secs'); the value must be positive"
                    );
                    false
                } else {
                    true
                }
            })
            .unwrap_or(25),
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

    resolved.retry.upstream_base_url = overrides
        .upstream_base_url
        .or_else(|| env_string("OPENCODE_UPSTREAM_BASE_URL"))
        .or_else(|| env_string("BRIDGE_UPSTREAM_BASE_URL"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.upstream_base_url.clone()))
        .unwrap_or_else(|| "https://opencode.ai/zen/v1".to_string())
        .trim_end_matches('/')
        .to_string();
    let single_upstream_key = if overrides.clear_upstream_api_key {
        None
    } else {
        secret_option(
            overrides
                .upstream_api_key
                .or_else(|| env_string("OPENCODE_UPSTREAM_API_KEY"))
                .or_else(|| env_string("BRIDGE_UPSTREAM_API_KEY"))
                .or_else(|| file.as_ref().and_then(|cfg| cfg.upstream_api_key.clone())),
        )
    };
    let mut upstream_keys = Vec::new();
    if !overrides.clear_upstream_api_key {
        if let Some(list) = file.as_ref().and_then(|cfg| cfg.upstream_api_keys.clone()) {
            for k in list.into_vec() {
                if let Some(secret) = secret_option(Some(k)) {
                    if !upstream_keys
                        .iter()
                        .any(|existing: &SecretString| existing.expose() == secret.expose())
                    {
                        upstream_keys.push(secret);
                    }
                }
            }
        }
    }
    if let Some(ref single) = single_upstream_key {
        upstream_keys.retain(|existing| existing.expose() != single.expose());
        upstream_keys.insert(0, single.clone());
    }
    resolved.retry.upstream_api_key =
        single_upstream_key.or_else(|| upstream_keys.first().cloned());
    resolved.retry.upstream_api_keys = upstream_keys;
    resolved.retry.model_fallbacks = env_string("OPENCODE_MODEL_FALLBACKS")
        .map(|value| parse_csv(&value))
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.model_fallbacks.clone())
                .map(StringList::into_vec)
        })
        .unwrap_or_default();
    resolved.retry.default_fallbacks_enabled = env_bool("OPENCODE_ENABLE_DEFAULT_FALLBACKS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.enable_default_fallbacks))
        .unwrap_or(false);
    resolved.retry.max_network_attempts = env_parse("BRIDGE_MAX_NETWORK_ATTEMPTS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_network_attempts))
        .unwrap_or(8);
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
        .map(|value| match EgressMode::parse(&value) {
            Some(mode) => mode,
            None => {
                warn!(
                    "unknown egress mode '{value}'; defaulting to hybrid \
                     (valid: direct, proxy/warp, hybrid)"
                );
                EgressMode::Hybrid
            }
        })
        .unwrap_or(EgressMode::Hybrid);
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
        .or_else(|| {
            file.as_ref()
                .and_then(|cfg| cfg.identity_endpoints.clone())
                .map(StringList::into_vec)
        })
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
    let candidates = [
        cli_value.map(|value| ("the --host CLI flag", value)),
        env_string("BRIDGE_HOST").map(|value| ("BRIDGE_HOST", value)),
        file_value.map(|value| ("the TOML 'host' key", value)),
    ];
    let Some((source, value)) = candidates.into_iter().flatten().next() else {
        return DEFAULT_HOST
            .parse()
            .expect("hardcoded default host must be valid");
    };
    match value.parse::<IpAddr>() {
        Ok(host) => host,
        Err(_) => {
            warn!("ignoring invalid host '{value}' from {source}; falling back to {DEFAULT_HOST}");
            DEFAULT_HOST
                .parse()
                .expect("hardcoded default host must be valid")
        }
    }
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
    let value = env_string(name)?;
    match value.parse() {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            warn!("ignoring invalid value '{value}' for environment variable {name}");
            None
        }
    }
}

fn env_bool(name: &str) -> Option<bool> {
    let value = env_string(name)?;
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            warn!("ignoring invalid boolean '{value}' for environment variable {name}");
            None
        }
    }
}

fn parse_csv_optional(value: Option<String>) -> Option<Vec<String>> {
    value
        .map(|raw| parse_csv(&raw))
        .filter(|items| !items.is_empty())
}

/// Reject list values that parse to nothing so they can never be mistaken
/// for deliberate configuration (see `EgressConfig::proxies_explicitly_configured`).
fn non_empty_list(items: Vec<String>) -> Option<Vec<String>> {
    (!items.is_empty()).then_some(items)
}

/// Normalize a TOML proxy list exactly like the CSV path: trim, drop empty
/// entries, and force remote-DNS socks5h. Keeping both paths identical is
/// what preserves the historical `socks5://` → `socks5h://` behavior for
/// array-form TOML proxy values.
fn normalized_list(items: Vec<String>) -> Option<Vec<String>> {
    let items = items
        .into_iter()
        .map(|item| super::normalize_proxy_url(item.trim()))
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    non_empty_list(items)
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(super::normalize_proxy_url)
        .collect()
}
