//! Deterministic configuration resolution.

use super::{
    BridgeConfig, CliOverrides, TomlConfig, DEFAULT_BRIDGE_PORT, DEFAULT_CHANNEL_CAPACITY,
    DEFAULT_HOST, DEFAULT_MAX_BODY_SIZE, DEFAULT_OPENCODE_PORT, DEFAULT_PRIMARY_PROXIES,
    DEFAULT_SHELL_ALLOWLIST, DEFAULT_STREAM_BUFFER_SIZE, DEFAULT_WARM_STANDBY_PROXIES,
};
use crate::shell::ShellPolicy;
use std::collections::HashSet;
use std::env;
use std::net::IpAddr;
use std::str::FromStr;
use tracing::warn;

pub(super) fn load(overrides: CliOverrides) -> BridgeConfig {
    let config_path = overrides
        .config_path
        .as_deref()
        .unwrap_or("opencode2api.toml");
    let file = TomlConfig::from_file(config_path);

    let host = resolve_host(
        overrides.host,
        file.as_ref().and_then(|cfg| cfg.host.clone()),
    );
    if host.is_unspecified() {
        warn!("⚠️  Bridge is binding to all network interfaces. Consider 127.0.0.1 for local-only access.");
    }

    let bridge_port = overrides
        .bridge_port
        .or_else(|| env_parse("BRIDGE_PORT"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.port))
        .unwrap_or(DEFAULT_BRIDGE_PORT);

    let opencode_port = env_parse("OPENCODE_PORT")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.opencode_port))
        .unwrap_or(DEFAULT_OPENCODE_PORT);

    let model = overrides
        .model
        .or_else(|| env_string("OPENCODE_MODEL"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.model.clone()));

    let shell_policy = resolve_shell_policy(
        overrides
            .shell_policy
            .or_else(|| env_string("BRIDGE_SHELL_POLICY"))
            .or_else(|| file.as_ref().and_then(|cfg| cfg.shell_policy.clone()))
            .unwrap_or_else(|| "disabled".to_string()),
        env_string("BRIDGE_SHELL_ALLOWLIST")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.shell_allowlist.clone()))
            .unwrap_or_else(|| DEFAULT_SHELL_ALLOWLIST.to_string()),
    );

    let auth_tokens = parse_csv_optional(
        env_string("BRIDGE_AUTH_TOKEN")
            .or_else(|| file.as_ref().and_then(|cfg| cfg.auth_tokens.clone())),
    );

    let max_body_size = overrides
        .max_body_size
        .or_else(|| env_parse("BRIDGE_MAX_BODY_SIZE"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_body_size))
        .unwrap_or(DEFAULT_MAX_BODY_SIZE);
    if max_body_size == 0 {
        warn!("⚠️  Request body limit is disabled (max_body_size=0).");
    }

    let stream_buffer_size = env_parse("BRIDGE_STREAM_BUFFER_SIZE")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.stream_buffer_size))
        .unwrap_or(DEFAULT_STREAM_BUFFER_SIZE);

    let channel_capacity = env_parse("BRIDGE_CHANNEL_CAPACITY")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.channel_capacity))
        .unwrap_or(DEFAULT_CHANNEL_CAPACITY);

    let tavily_api_key = overrides
        .tavily_api_key
        .or_else(|| env_string("TAVILY_API_KEY"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.tavily_api_key.clone()));
    let exa_api_key = overrides
        .exa_api_key
        .or_else(|| env_string("EXA_API_KEY"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.exa_api_key.clone()));
    let serper_api_key = overrides
        .serper_api_key
        .or_else(|| env_string("SERPER_API_KEY"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.serper_api_key.clone()));
    let searxng_url = overrides
        .searxng_url
        .or_else(|| env_string("SEARXNG_URL"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.searxng_url.clone()));
    let searxng_api_key = overrides
        .searxng_api_key
        .or_else(|| env_string("SEARXNG_API_KEY"))
        .or_else(|| file.as_ref().and_then(|cfg| cfg.searxng_api_key.clone()));

    let max_search_loops = env_parse("BRIDGE_MAX_SEARCH_LOOPS")
        .or_else(|| file.as_ref().and_then(|cfg| cfg.max_search_loops))
        .unwrap_or(5);

    let legacy_proxy_value = env_string("BRIDGE_PROXIES").or_else(|| {
        file.as_ref()
            .and_then(|cfg| cfg.proxies.as_ref().map(|items| items.join(",")))
    });
    let proxies = parse_csv_optional(legacy_proxy_value.clone());

    let primary_proxies = parse_csv_optional(Some(
        env_string("BRIDGE_PRIMARY_PROXIES")
            .or(legacy_proxy_value)
            .unwrap_or_else(|| DEFAULT_PRIMARY_PROXIES.to_string()),
    ));
    let warm_standby_proxies = parse_csv_optional(Some(
        env_string("BRIDGE_WARM_STANDBY_PROXIES")
            .unwrap_or_else(|| DEFAULT_WARM_STANDBY_PROXIES.to_string()),
    ));

    BridgeConfig {
        host,
        bridge_port,
        opencode_port,
        model,
        shell_policy,
        auth_tokens,
        max_body_size,
        stream_buffer_size,
        channel_capacity,
        tavily_api_key,
        exa_api_key,
        serper_api_key,
        searxng_url,
        searxng_api_key,
        max_search_loops,
        proxies,
        primary_proxies,
        warm_standby_proxies,
    }
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
                "Unknown shell policy '{}' — defaulting to Disabled. Valid values: disabled, allowlist, unrestricted",
                raw_policy
            );
            ShellPolicy::Disabled
        }
    }
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
        .map(ToOwned::to_owned)
        .collect()
}
