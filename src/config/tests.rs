use super::*;
use crate::shell::ShellPolicy;
use std::env;
use std::sync::Mutex;

/// Serializes tests that modify process-level environment variables.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_default_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Clear env vars that might affect test
    env::remove_var("BRIDGE_HOST");
    env::remove_var("BRIDGE_PORT");
    env::remove_var("OPENCODE_PORT");
    env::remove_var("OPENCODE_MODEL");
    env::remove_var("BRIDGE_SHELL_POLICY");
    env::remove_var("BRIDGE_AUTH_TOKEN");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(config.bridge_port, DEFAULT_BRIDGE_PORT);
    assert_eq!(config.opencode_port, DEFAULT_OPENCODE_PORT);
    assert_eq!(config.host.to_string(), DEFAULT_HOST);
    assert!(config.model.is_none());
    assert!(!config.auth_enabled());
    assert_eq!(config.stream_buffer_size, DEFAULT_STREAM_BUFFER_SIZE);
    assert_eq!(
        config.primary_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:40001".to_string()].as_slice())
    );
    assert_eq!(
        config.warm_standby_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:40004".to_string()].as_slice())
    );
    assert_eq!(config.egress.active_proxy_count, 1);
    assert!(
        matches!(config.shell_policy, ShellPolicy::Disabled),
        "default shell policy must be Disabled for security reasons"
    );
}

#[test]
fn agent_team_defaults_have_high_capacity_without_global_concurrency_cap() {
    let config = BridgeConfig::default();

    assert_eq!(config.max_body_size, 64 * 1024 * 1024);
    assert_eq!(config.stream_buffer_size, 64 * 1024);
    assert_eq!(config.channel_capacity, 2048);
    assert_eq!(config.max_search_loops, 20);
    assert_eq!(config.retry.max_network_attempts, 8);
    assert_eq!(config.retry.max_provider_attempts, 2);
    assert_eq!(config.retry.base_backoff, std::time::Duration::from_secs(1));
    assert_eq!(config.retry.max_backoff, std::time::Duration::from_secs(30));
    assert_eq!(config.egress.max_restart_attempts, 6);
    assert_eq!(
        config.runtime.worker_shutdown_timeout,
        std::time::Duration::from_secs(30)
    );
    assert_eq!(
        config.runtime.server_shutdown_timeout,
        std::time::Duration::from_secs(30)
    );
    assert!(config.observability.max_concurrent_requests.is_none());

    assert_eq!(config.protocol.max_sse_line_bytes, 4 * 1024 * 1024);
    assert_eq!(config.protocol.max_sync_response_bytes, 32 * 1024 * 1024);
    assert_eq!(config.search.max_results, 20);
    assert_eq!(config.search.max_snippet_chars, 2000);
    assert_eq!(config.search.max_response_bytes, 8 * 1024 * 1024);
    assert_eq!(
        config.search.request_timeout,
        std::time::Duration::from_secs(30)
    );

    assert_eq!(config.history.max_records, 1_000_000);
    assert_eq!(config.history.max_database_bytes, 16 * 1024 * 1024 * 1024);
    assert_eq!(config.history.max_request_bytes, 8 * 1024 * 1024);
    assert_eq!(config.history.max_reasoning_bytes, 16 * 1024 * 1024);
    assert_eq!(config.history.max_response_bytes, 16 * 1024 * 1024);
    assert_eq!(config.history.max_tool_payload_bytes, 4 * 1024 * 1024);
    assert_eq!(config.history.max_record_bytes, 48 * 1024 * 1024);
    assert_eq!(config.history.queue_capacity, 8192);
}

#[test]
fn test_toml_parsing() {
    let toml_str = r#"
            port = 5000
            host = "0.0.0.0"
            opencode_port = 4096
            model = "gpt-4"
            shell_policy = "allowlist"
            shell_allowlist = "git,ls,pwd"
            auth_tokens = "token1,token2"
            max_body_size = 2097152
            stream_buffer_size = 8192
            channel_capacity = 512
        "#;
    let config: TomlConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.port, Some(5000));
    assert_eq!(config.host.as_deref(), Some("0.0.0.0"));
    assert_eq!(config.opencode_port, Some(4096));
    assert_eq!(config.model.as_deref(), Some("gpt-4"));
    assert_eq!(config.shell_policy.as_deref(), Some("allowlist"));
    assert_eq!(config.shell_allowlist.as_deref(), Some("git,ls,pwd"));
    assert_eq!(
        config.auth_tokens.clone().map(StringList::into_vec),
        Some(vec!["token1".to_string(), "token2".to_string()])
    );
    assert_eq!(config.max_body_size, Some(2097152));
    assert_eq!(config.stream_buffer_size, Some(8192));
    assert_eq!(config.channel_capacity, Some(512));
}

#[test]
fn test_toml_file_loading() {
    let tmp = std::env::temp_dir().join("opencode2api_test_loading.toml");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"port = 6000\nhost = \"127.0.0.1\"\n").unwrap();

    let config = TomlConfig::from_file(tmp.to_string_lossy().as_ref()).unwrap();
    assert_eq!(config.port, Some(6000));
    assert_eq!(config.host.as_deref(), Some("127.0.0.1"));

    std::fs::remove_file(&tmp).unwrap();
}

#[test]
fn test_toml_file_not_found() {
    let config = TomlConfig::from_file("/tmp/nonexistent_opencode2api_test.toml");
    assert!(config.is_none());
}

#[test]
fn test_env_overrides_toml() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_PORT");
    env::remove_var("BRIDGE_HOST");

    let tmp = std::env::temp_dir().join("opencode2api_test_env_override.toml");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"port = 3000\nhost = \"0.0.0.0\"\n").unwrap();

    env::set_var("BRIDGE_PORT", "5000");

    let overrides = CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    };
    let config = BridgeConfig::from_env_and_cli(overrides);

    assert_eq!(config.bridge_port, 5000, "env should override TOML");
    assert_eq!(
        config.host.to_string(),
        "0.0.0.0",
        "TOML should apply when env is unset"
    );

    env::remove_var("BRIDGE_PORT");
    env::remove_var("BRIDGE_HOST");
    std::fs::remove_file(&tmp).unwrap();
}

#[test]
fn test_cli_overrides_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_PORT");
    env::remove_var("BRIDGE_HOST");

    env::set_var("BRIDGE_PORT", "3000");

    let overrides = CliOverrides {
        bridge_port: Some(7000),
        ..Default::default()
    };
    let config = BridgeConfig::from_env_and_cli(overrides);

    assert_eq!(config.bridge_port, 7000, "CLI should override env");

    env::remove_var("BRIDGE_PORT");
}

#[test]
fn test_toml_defaults_applied() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_PORT");
    env::remove_var("BRIDGE_HOST");
    env::remove_var("BRIDGE_SHELL_POLICY");

    let tmp = std::env::temp_dir().join("opencode2api_test_defaults.toml");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"shell_policy = \"disabled\"\n").unwrap();

    let overrides = CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    };
    let config = BridgeConfig::from_env_and_cli(overrides);

    assert_eq!(config.bridge_port, DEFAULT_BRIDGE_PORT);
    assert!(matches!(config.shell_policy, ShellPolicy::Disabled));
    assert_eq!(config.host.to_string(), DEFAULT_HOST);

    std::fs::remove_file(&tmp).unwrap();
}

#[test]
fn test_auth_validation() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_AUTH_TOKEN");

    let mut config = BridgeConfig::from_env_and_cli(CliOverrides::default());

    // No auth configured — everything is valid
    config.auth_tokens = None;
    assert!(config.is_valid_token("anything"));

    // Auth configured — only matching tokens are valid
    config.auth_tokens = Some(vec!["secret-123".into(), "secret-456".into()]);
    assert!(config.is_valid_token("secret-123"));
    assert!(config.is_valid_token("secret-456"));
    assert!(!config.is_valid_token("wrong-token"));
}

// ── Security validation tests (Phase 3) ──

#[test]
fn test_security_localhost_without_auth_allowed() {
    // 127.0.0.1 without auth — OK
    let config = BridgeConfig {
        host: "127.0.0.1".parse().unwrap(),
        shell_policy: ShellPolicy::Unrestricted,
        auth_tokens: None,
        ..Default::default()
    };
    assert!(
        config.validate_security().is_ok(),
        "localhost without auth must be allowed"
    );
}

#[test]
fn test_security_public_bind_without_auth_rejected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // 0.0.0.0 without auth — rejected
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: None,
        management: ManagementConfig {
            dashboard_token: Some("super-secret-admin-token-12345".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(result.is_err(), "public bind without auth must be rejected");
    let msg = result.unwrap_err();
    assert!(
        msg.contains("SECURITY VIOLATION"),
        "error should mention SECURITY VIOLATION: {}",
        msg
    );
    assert!(
        msg.contains("BRIDGE_AUTH_TOKEN"),
        "error should mention BRIDGE_AUTH_TOKEN: {}",
        msg
    );
}

#[test]
fn test_security_public_bind_with_auth_allowed() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // 0.0.0.0 with auth — OK
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid".into()]),
        management: ManagementConfig {
            dashboard_token: Some("super-secret-admin-token-12345".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(
        result.is_ok(),
        "public bind with auth must be allowed: {:?}",
        result.err()
    );
}

#[test]
fn test_security_public_bind_with_unrestricted_shell_rejected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // 0.0.0.0 + unrestricted shell — rejected regardless of auth
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Unrestricted,
        auth_tokens: Some(vec!["sk-valid".into()]),
        management: ManagementConfig {
            dashboard_token: Some("super-secret-admin-token-12345".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(
        result.is_err(),
        "public bind + unrestricted shell must be rejected even with auth"
    );
    let msg = result.unwrap_err();
    assert!(
        msg.contains("SECURITY VIOLATION"),
        "error should mention SECURITY VIOLATION: {}",
        msg
    );
    assert!(
        msg.contains("BRIDGE_SHELL_POLICY"),
        "error should mention BRIDGE_SHELL_POLICY: {}",
        msg
    );
}

#[test]
fn test_security_public_bind_without_dashboard_token_rejected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid".into()]),
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(
        result.is_err(),
        "public bind without dashboard token must be rejected"
    );
    assert!(result.unwrap_err().contains("DASHBOARD_ADMIN_TOKEN"));
}

#[test]
fn test_security_public_bind_with_weak_dashboard_token_rejected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid".into()]),
        management: ManagementConfig {
            dashboard_token: Some("12345".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(
        result.is_err(),
        "public bind with weak dashboard token must be rejected"
    );
    assert!(result.unwrap_err().contains("too weak"));
}

#[test]
fn test_security_default_shell_policy_is_disabled() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_SHELL_POLICY");
    env::remove_var("BRIDGE_HOST");
    env::remove_var("BRIDGE_AUTH_TOKEN");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(
        matches!(config.shell_policy, ShellPolicy::Disabled),
        "default shell policy must be Disabled"
    );
}

#[test]
fn test_unknown_shell_policy_defaults_to_disabled() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_HOST");
    env::remove_var("BRIDGE_AUTH_TOKEN");
    env::set_var("BRIDGE_SHELL_POLICY", "typo_all");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(
        matches!(config.shell_policy, ShellPolicy::Disabled),
        "unknown policy 'typo_all' must fall back to Disabled, not Unrestricted"
    );

    env::remove_var("BRIDGE_SHELL_POLICY");

    // Test case-insensitive unknown value
    env::set_var("BRIDGE_SHELL_POLICY", "ALL");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(
        matches!(config.shell_policy, ShellPolicy::Disabled),
        "unknown policy 'ALL' must fall back to Disabled"
    );

    env::remove_var("BRIDGE_SHELL_POLICY");
}

#[test]
fn test_known_shell_policies_still_work() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    env::remove_var("BRIDGE_HOST");
    env::remove_var("BRIDGE_AUTH_TOKEN");

    env::set_var("BRIDGE_SHELL_POLICY", "disabled");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(matches!(config.shell_policy, ShellPolicy::Disabled));
    env::remove_var("BRIDGE_SHELL_POLICY");

    env::set_var("BRIDGE_SHELL_POLICY", "allowlist");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(matches!(config.shell_policy, ShellPolicy::AllowList(_)));
    env::remove_var("BRIDGE_SHELL_POLICY");

    env::set_var("BRIDGE_SHELL_POLICY", "unrestricted");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(matches!(config.shell_policy, ShellPolicy::Unrestricted));
    env::remove_var("BRIDGE_SHELL_POLICY");
}

#[test]
fn test_auth_tokens_accept_toml_array() {
    let parsed: TomlConfig = toml::from_str(r#"auth_tokens = ["token-a", "token-b"]"#)
        .expect("array auth tokens should parse");
    assert_eq!(
        parsed.auth_tokens.map(StringList::into_vec),
        Some(vec!["token-a".to_string(), "token-b".to_string()])
    );
}

#[test]
fn test_operational_policy_precedence_env_over_toml() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let tmp = std::env::temp_dir().join("opencode2api_policy_precedence.toml");
    std::fs::write(
        &tmp,
        r#"
upstream_base_url = "https://toml.example/v1"
active_proxy_count = 1
rate_limit = 3
min_reasoning_stream_tokens = 2048
max_sse_line_bytes = 131072
max_sync_response_bytes = 2097152
search_max_results = 4
search_timeout_secs = 12
tavily_url = "https://toml-search.example/tavily"
model_fallbacks = ["toml-fallback"]
egress_mode = "proxy"
primary_proxies = ["socks5://127.0.0.1:40001"]
"#,
    )
    .expect("write test config");

    env::set_var("OPENCODE_UPSTREAM_BASE_URL", "https://env.example/v1/");
    env::set_var("BRIDGE_ACTIVE_PROXY_COUNT", "2");
    env::set_var("BRIDGE_RATE_LIMIT", "7");
    env::set_var("BRIDGE_MIN_REASONING_STREAM_TOKENS", "4096");
    env::set_var("BRIDGE_MAX_SSE_LINE_BYTES", "65536");
    env::set_var("BRIDGE_MAX_SYNC_RESPONSE_BYTES", "1048576");
    env::set_var("BRIDGE_SEARCH_MAX_RESULTS", "6");
    env::set_var("BRIDGE_SEARCH_TIMEOUT_SECS", "9");
    env::set_var("TAVILY_API_URL", "https://env-search.example/tavily");
    env::set_var("OPENCODE_MODEL_FALLBACKS", "env-a,env-b");

    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });

    assert_eq!(config.retry.upstream_base_url, "https://env.example/v1");
    assert_eq!(config.egress.active_proxy_count, 2);
    assert_eq!(config.observability.max_concurrent_requests, Some(7));
    assert_eq!(config.protocol.min_reasoning_stream_tokens, 4096);
    assert_eq!(config.protocol.max_sse_line_bytes, 65_536);
    assert_eq!(config.protocol.max_sync_response_bytes, 1_048_576);
    assert_eq!(config.search.max_results, 6);
    assert_eq!(
        config.search.request_timeout,
        std::time::Duration::from_secs(9)
    );
    assert_eq!(
        config.search.tavily_url,
        "https://env-search.example/tavily"
    );
    assert_eq!(config.retry.model_fallbacks, vec!["env-a", "env-b"]);

    for name in [
        "OPENCODE_UPSTREAM_BASE_URL",
        "BRIDGE_ACTIVE_PROXY_COUNT",
        "BRIDGE_RATE_LIMIT",
        "BRIDGE_MIN_REASONING_STREAM_TOKENS",
        "BRIDGE_MAX_SSE_LINE_BYTES",
        "BRIDGE_MAX_SYNC_RESPONSE_BYTES",
        "BRIDGE_SEARCH_MAX_RESULTS",
        "BRIDGE_SEARCH_TIMEOUT_SECS",
        "TAVILY_API_URL",
        "OPENCODE_MODEL_FALLBACKS",
    ] {
        env::remove_var(name);
    }
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn test_secret_string_formatting_is_redacted() {
    let secret = SecretString::from("do-not-print-me");
    assert_eq!(secret.to_string(), "[REDACTED]");
    let debug = format!("{secret:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("do-not-print-me"));
}

#[test]
fn socks5_proxy_environment_values_use_remote_dns() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let names = [
        "BRIDGE_PRIMARY_PROXIES",
        "BRIDGE_WARM_STANDBY_PROXIES",
        "BRIDGE_CONFIG_PATH",
    ];
    let previous = names
        .iter()
        .map(|name| ((*name).to_string(), env::var(name).ok()))
        .collect::<Vec<_>>();

    env::set_var(
        "BRIDGE_PRIMARY_PROXIES",
        "socks5://127.0.0.1:40001,socks5h://127.0.0.1:40002",
    );
    env::set_var("BRIDGE_WARM_STANDBY_PROXIES", "socks5://127.0.0.1:40004");
    env::set_var(
        "BRIDGE_CONFIG_PATH",
        "/tmp/opencode2api-missing-proxy-normalization.toml",
    );

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());

    assert_eq!(
        config.primary_proxies,
        Some(vec![
            "socks5h://127.0.0.1:40001".to_string(),
            "socks5h://127.0.0.1:40002".to_string(),
        ])
    );
    assert_eq!(
        config.warm_standby_proxies,
        Some(vec!["socks5h://127.0.0.1:40004".to_string()])
    );

    for (name, value) in previous {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }
}

#[test]
fn test_cli_egress_mode_overrides_environment() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = env::var("BRIDGE_EGRESS_MODE").ok();
    env::set_var("BRIDGE_EGRESS_MODE", "proxy");

    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        egress_mode: Some("direct".to_string()),
        ..Default::default()
    });

    assert_eq!(config.egress.mode, EgressMode::Direct);

    match previous {
        Some(value) => env::set_var("BRIDGE_EGRESS_MODE", value),
        None => env::remove_var("BRIDGE_EGRESS_MODE"),
    }
}
