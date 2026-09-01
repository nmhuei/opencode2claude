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
    // Developer shells and the repository `.env` often export machine-wide
    // values below; without clearing them "default" silently inherits a real
    // deployment TOML (BRIDGE_CONFIG_PATH) or proxy topology overrides.
    let previous = [
        ("BRIDGE_CONFIG_PATH", env::var("BRIDGE_CONFIG_PATH").ok()),
        (
            "BRIDGE_PRIMARY_PROXIES",
            env::var("BRIDGE_PRIMARY_PROXIES").ok(),
        ),
        (
            "BRIDGE_WARM_STANDBY_PROXIES",
            env::var("BRIDGE_WARM_STANDBY_PROXIES").ok(),
        ),
        (
            "BRIDGE_ACTIVE_PROXY_COUNT",
            env::var("BRIDGE_ACTIVE_PROXY_COUNT").ok(),
        ),
    ];
    for (name, _) in &previous {
        env::remove_var(name);
    }

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

    // Restore the caller's environment.
    for (name, value) in previous {
        match value {
            Some(restored) => env::set_var(name, restored),
            None => env::remove_var(name),
        }
    }
}

#[test]
fn agent_team_defaults_have_high_capacity_without_global_concurrency_cap() {
    let config = BridgeConfig::default();

    assert_eq!(config.max_body_size, 64 * 1024 * 1024);
    assert_eq!(config.stream_buffer_size, 64 * 1024);
    assert_eq!(config.channel_capacity, 2048);
    assert_eq!(config.max_search_loops, 20);
    assert_eq!(config.retry.max_network_attempts, 8);
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
    // Unified with the loader's effective default (2 MiB); the two
    // constructions paths must not disagree.
    assert_eq!(config.history.max_response_bytes, 2 * 1024 * 1024);
    assert_eq!(config.history.max_tool_payload_bytes, 4 * 1024 * 1024);
    assert_eq!(config.history.max_record_bytes, 48 * 1024 * 1024);
    assert_eq!(config.history.queue_capacity, 8192);
}

#[test]
fn hybrid_egress_mode_parses_without_changing_direct_or_proxy() {
    assert_eq!(EgressMode::parse("hybrid"), Some(EgressMode::Hybrid));
    assert_eq!(EgressMode::parse("direct"), Some(EgressMode::Direct));
    assert_eq!(EgressMode::parse("proxy"), Some(EgressMode::Proxy));
    assert_eq!(EgressMode::parse("warp"), Some(EgressMode::Proxy));
}

#[test]
fn hybrid_timing_defaults_are_bounded() {
    let config = BridgeConfig::default();
    assert_eq!(
        config.egress.bootstrap_timeout,
        std::time::Duration::from_secs(30)
    );
    assert_eq!(
        config.egress.verify_timeout,
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        config.egress.recovery_backoff_max,
        std::time::Duration::from_secs(120)
    );
}

#[test]
fn hybrid_timing_env_overrides_defaults() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let names = [
        "BRIDGE_PROXY_BOOTSTRAP_TIMEOUT_SECS",
        "BRIDGE_PROXY_VERIFY_TIMEOUT_SECS",
        "BRIDGE_PROXY_RECOVERY_BACKOFF_MAX_SECS",
    ];
    let previous = names.map(|name| env::var(name).ok());
    env::set_var(names[0], "7");
    env::set_var(names[1], "4");
    env::set_var(names[2], "45");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.egress.bootstrap_timeout,
        std::time::Duration::from_secs(7)
    );
    assert_eq!(
        config.egress.verify_timeout,
        std::time::Duration::from_secs(4)
    );
    assert_eq!(
        config.egress.recovery_backoff_max,
        std::time::Duration::from_secs(45)
    );

    for (name, value) in names.into_iter().zip(previous) {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }
}

#[test]
fn strict_proxy_still_rejects_direct_fallback_flag() {
    let mut config = BridgeConfig::default();
    config.egress.mode = EgressMode::Proxy;
    config.egress.allow_direct_fallback = true;
    assert!(config.validate_security().is_err());
}

#[test]
fn hybrid_does_not_require_legacy_allow_direct_fallback() {
    let mut config = BridgeConfig::default();
    config.egress.mode = EgressMode::Hybrid;
    config.egress.allow_direct_fallback = false;
    assert!(config.validate_security().is_ok());
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
        auth_tokens: Some(vec!["sk-valid-token-12345".into()]),
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
        auth_tokens: Some(vec!["sk-valid-token-12345".into()]),
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
fn toml_list_keys_accept_csv_string_form() {
    // A CSV string must not poison the whole document: with these keys typed
    // as plain `Vec<String>`, one string-form value made `from_file` drop
    // every setting in the file silently.
    let parsed: TomlConfig = toml::from_str(
        r#"
proxies = "socks5://127.0.0.1:49005, socks5h://127.0.0.1:49006"
primary_proxies = "socks5://127.0.0.1:49007"
warm_standby_proxies = "socks5h://127.0.0.1:49008"
model_fallbacks = "fb-a, fb-b"
identity_endpoints = "https://a.example/cdn-cgi/trace, https://b.example/?format=json"
"#,
    )
    .expect("CSV string form must be accepted for list-valued TOML keys");
    assert_eq!(
        parsed.proxies.map(StringList::into_vec),
        Some(vec![
            "socks5://127.0.0.1:49005".to_string(),
            "socks5h://127.0.0.1:49006".to_string(),
        ])
    );
    assert_eq!(
        parsed.primary_proxies.map(StringList::into_vec),
        Some(vec!["socks5://127.0.0.1:49007".to_string()])
    );
    assert_eq!(
        parsed.warm_standby_proxies.map(StringList::into_vec),
        Some(vec!["socks5h://127.0.0.1:49008".to_string()])
    );
    assert_eq!(
        parsed.model_fallbacks.map(StringList::into_vec),
        Some(vec!["fb-a".to_string(), "fb-b".to_string()])
    );
    assert_eq!(
        parsed
            .identity_endpoints
            .map(StringList::into_vec)
            .as_deref(),
        Some(
            [
                "https://a.example/cdn-cgi/trace".to_string(),
                "https://b.example/?format=json".to_string(),
            ]
            .as_slice()
        )
    );
}

#[test]
fn csv_string_toml_proxy_keys_feed_the_loader_with_precedence_intact() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();

    let tmp = std::env::temp_dir().join("opencode2api_csv_string_proxies.toml");
    std::fs::write(
        &tmp,
        r#"
proxies = "socks5://127.0.0.1:49011"
primary_proxies = "socks5://127.0.0.1:49012"
warm_standby_proxies = "socks5h://127.0.0.1:49013"
"#,
    )
    .unwrap();

    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(
        config.primary_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:49012".to_string()].as_slice()),
        "string-form primary_proxies must beat string-form legacy proxies"
    );
    assert_eq!(
        config.warm_standby_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:49013".to_string()].as_slice())
    );
    assert!(config.egress.proxies_explicitly_configured);

    env::set_var("BRIDGE_WARM_STANDBY_PROXIES", "socks5h://127.0.0.1:49014");
    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(
        config.warm_standby_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:49014".to_string()].as_slice()),
        "env must still beat CSV-string TOML"
    );

    restore_env(previous);
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn malformed_toml_document_is_rejected_wholesale() {
    // Pins the fail-safe semantics that the loader's diagnostics rely on:
    // any migration conflict or type confusion rejects the WHOLE document,
    // never a partial parse.
    let tmp = std::env::temp_dir().join("opencode2api_malformed_toml.toml");

    std::fs::write(&tmp, b"port = \"not-a-number\"\n").unwrap();
    assert!(TomlConfig::from_file(tmp.to_string_lossy().as_ref()).is_none());

    std::fs::write(&tmp, b"bridge_port = 4000\nport = 4001\n").unwrap();
    assert!(TomlConfig::from_file(tmp.to_string_lossy().as_ref()).is_none());

    std::fs::write(&tmp, b"schema_version = 99\n").unwrap();
    assert!(TomlConfig::from_file(tmp.to_string_lossy().as_ref()).is_none());

    let _ = std::fs::remove_file(tmp);
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
        "OPENCODE_UPSTREAM_API_KEY",
        "BRIDGE_UPSTREAM_BASE_URL",
        "BRIDGE_UPSTREAM_API_KEY",
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
fn test_upstream_api_key_and_base_url_resolution() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Hermetic start: a leaked upstream credential from the surrounding
    // environment (shell export, .env) would otherwise win the resolution
    // chain before the TOML file and break step 1's assertion.
    env::remove_var("OPENCODE_UPSTREAM_API_KEY");
    env::remove_var("BRIDGE_UPSTREAM_API_KEY");
    env::remove_var("OPENCODE_UPSTREAM_BASE_URL");
    env::remove_var("BRIDGE_UPSTREAM_BASE_URL");
    let tmp = std::env::temp_dir().join("opencode2api_upstream_test.toml");
    std::fs::write(
        &tmp,
        r#"
upstream_base_url = "https://toml.upstream.example/v1"
upstream_api_key = "sk-toml-upstream-key"
"#,
    )
    .expect("write test config");

    // 1. From TOML
    let config_toml = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(
        config_toml.retry.upstream_base_url,
        "https://toml.upstream.example/v1"
    );
    assert_eq!(
        config_toml
            .retry
            .upstream_api_key
            .as_ref()
            .map(|k| k.expose()),
        Some("sk-toml-upstream-key")
    );

    // 2. Compatibility env aliases
    env::set_var(
        "BRIDGE_UPSTREAM_BASE_URL",
        "https://bridge-env.upstream.example/v1",
    );
    env::set_var("BRIDGE_UPSTREAM_API_KEY", "sk-bridge-env-upstream-key");
    let config_bridge_env = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(
        config_bridge_env.retry.upstream_base_url,
        "https://bridge-env.upstream.example/v1"
    );
    assert_eq!(
        config_bridge_env
            .retry
            .upstream_api_key
            .as_ref()
            .map(|k| k.expose()),
        Some("sk-bridge-env-upstream-key")
    );
    env::remove_var("BRIDGE_UPSTREAM_BASE_URL");
    env::remove_var("BRIDGE_UPSTREAM_API_KEY");

    // 3. Canonical env override
    env::set_var(
        "OPENCODE_UPSTREAM_BASE_URL",
        "https://env.upstream.example/v1",
    );
    env::set_var("OPENCODE_UPSTREAM_API_KEY", "sk-env-upstream-key");

    let config_env = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(
        config_env.retry.upstream_base_url,
        "https://env.upstream.example/v1"
    );
    assert_eq!(
        config_env
            .retry
            .upstream_api_key
            .as_ref()
            .map(|k| k.expose()),
        Some("sk-env-upstream-key")
    );

    // 3. CLI override
    let config_cli = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        upstream_base_url: Some("https://cli.upstream.example/v1".to_string()),
        upstream_api_key: Some("sk-cli-upstream-key".to_string()),
        ..Default::default()
    });
    assert_eq!(
        config_cli.retry.upstream_base_url,
        "https://cli.upstream.example/v1"
    );
    assert_eq!(
        config_cli
            .retry
            .upstream_api_key
            .as_ref()
            .map(|k| k.expose()),
        Some("sk-cli-upstream-key")
    );

    env::remove_var("OPENCODE_UPSTREAM_BASE_URL");
    env::remove_var("OPENCODE_UPSTREAM_API_KEY");

    // 4. URL-only CLI overrides must never inherit a stored key from TOML.
    let config_url_only = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        upstream_base_url: Some("https://other-provider.example/v1".to_string()),
        clear_upstream_api_key: true,
        ..Default::default()
    });
    assert_eq!(
        config_url_only.retry.upstream_base_url,
        "https://other-provider.example/v1"
    );
    assert!(
        config_url_only.retry.upstream_api_key.is_none(),
        "URL-only provider override must clear inherited credentials"
    );

    let _ = std::fs::remove_file(tmp);
}

#[test]
fn upstream_api_key_list_is_trimmed_deduplicated_and_prefers_the_single_key() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = [
        (
            "OPENCODE_UPSTREAM_API_KEY",
            env::var("OPENCODE_UPSTREAM_API_KEY").ok(),
        ),
        (
            "BRIDGE_UPSTREAM_API_KEY",
            env::var("BRIDGE_UPSTREAM_API_KEY").ok(),
        ),
        (
            "OPENCODE_UPSTREAM_BASE_URL",
            env::var("OPENCODE_UPSTREAM_BASE_URL").ok(),
        ),
        (
            "BRIDGE_UPSTREAM_BASE_URL",
            env::var("BRIDGE_UPSTREAM_BASE_URL").ok(),
        ),
    ];
    for (name, _) in &previous {
        env::remove_var(name);
    }
    let tmp = std::env::temp_dir().join(format!(
        "opencode2api_upstream_keys_test_{}.toml",
        std::process::id()
    ));
    std::fs::write(
        &tmp,
        r#"
upstream_api_key = "single-key"
upstream_api_keys = [" list-key ", "single-key", "list-key", ""]
"#,
    )
    .expect("write test config");

    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });

    assert_eq!(
        config
            .retry
            .upstream_api_keys
            .iter()
            .map(SecretString::expose)
            .collect::<Vec<_>>(),
        vec!["single-key", "list-key"]
    );
    assert_eq!(
        config
            .retry
            .upstream_api_key
            .as_ref()
            .map(SecretString::expose),
        Some("single-key")
    );

    let _ = std::fs::remove_file(tmp);
    for (name, value) in previous {
        if let Some(value) = value {
            env::set_var(name, value);
        } else {
            env::remove_var(name);
        }
    }
}

#[test]
fn search_chain_budget_default_matches_search_policy() {
    let config = BridgeConfig::default();
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(25),
        "config default must stay in lockstep with SearchPolicy::default().chain_budget"
    );
}

#[test]
fn search_chain_budget_env_toml_precedence_and_invalid_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let name = "BRIDGE_SEARCH_CHAIN_BUDGET_SECS";
    let previous = (
        name,
        env::var(name).ok(),
        ("BRIDGE_CONFIG_PATH", env::var("BRIDGE_CONFIG_PATH").ok()),
    );
    env::remove_var(name);
    env::remove_var("BRIDGE_CONFIG_PATH");

    // Default (neither source configured).
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(25)
    );

    // TOML override applies when the environment is unset.
    let tmp = std::env::temp_dir().join("opencode2api_search_chain_budget.toml");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"search_chain_budget_secs = 33\n").unwrap();
    let toml_overrides = CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    };
    let config = BridgeConfig::from_env_and_cli(toml_overrides);
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(33)
    );

    // Environment beats TOML.
    env::set_var(name, "41");
    let toml_overrides = CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    };
    let config = BridgeConfig::from_env_and_cli(toml_overrides);
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(41),
        "env should override TOML"
    );

    // Invalid numeric text warns via env_parse and falls back to the default.
    env::set_var(name, "not-a-number");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(25)
    );

    // A zero budget would starve every provider; it must be rejected.
    env::set_var(name, "0");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(25),
        "zero budget must fall back to the default"
    );

    // Same rejection applies to a zero coming from TOML.
    env::remove_var(name);
    std::fs::write(&tmp, b"search_chain_budget_secs = 0\n").unwrap();
    let toml_overrides = CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    };
    let config = BridgeConfig::from_env_and_cli(toml_overrides);
    assert_eq!(
        config.search.chain_budget,
        std::time::Duration::from_secs(25),
        "zero budget from TOML must also fall back to the default"
    );

    match previous {
        (_, Some(restored), (config_name, config_value)) => {
            env::set_var(name, restored);
            match config_value {
                Some(value) => env::set_var(config_name, value),
                None => env::remove_var(config_name),
            }
        }
        (_, None, (config_name, config_value)) => {
            env::remove_var(name);
            match config_value {
                Some(value) => env::set_var(config_name, value),
                None => env::remove_var(config_name),
            }
        }
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

// ── Legacy `proxies` vs specific `primary_proxies` precedence ──

fn isolate_proxy_env() -> Vec<(String, Option<String>)> {
    let names = [
        "BRIDGE_PRIMARY_PROXIES",
        "BRIDGE_WARM_STANDBY_PROXIES",
        "BRIDGE_PROXIES",
        "BRIDGE_EGRESS_MODE",
        "BRIDGE_CONFIG_PATH",
    ];
    let previous = names
        .iter()
        .map(|name| ((*name).to_string(), env::var(name).ok()))
        .collect::<Vec<_>>();
    for (name, _) in &previous {
        env::remove_var(name);
    }
    previous
}

fn restore_env(previous: Vec<(String, Option<String>)>) {
    for (name, value) in previous {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }
}

#[test]
fn toml_legacy_proxies_lose_to_primary_proxies_in_same_document() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();

    let tmp = std::env::temp_dir().join("opencode2api_proxy_specificity.toml");
    std::fs::write(
        &tmp,
        r#"
proxies = ["socks5://127.0.0.1:49001"]
primary_proxies = ["socks5://127.0.0.1:49002"]
"#,
    )
    .unwrap();

    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });

    assert_eq!(
        config.primary_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:49002".to_string()].as_slice()),
        "the specific primary_proxies key must beat the legacy proxies key in the same document"
    );
    assert!(config.egress.proxies_explicitly_configured);

    restore_env(previous);
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn toml_legacy_proxies_alone_still_configure_primary_pool() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();

    let tmp = std::env::temp_dir().join("opencode2api_legacy_only.toml");
    std::fs::write(&tmp, r#"proxies = ["socks5://127.0.0.1:49003"]"#).unwrap();

    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });

    assert_eq!(
        config.primary_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:49003".to_string()].as_slice()),
        "a lone legacy proxies key must keep feeding the primary pool"
    );

    restore_env(previous);
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn default_pool_is_not_flagged_as_explicit_configuration() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(config.primary_proxies.is_some());
    assert!(
        !config.egress.proxies_explicitly_configured,
        "the built-in WARP fallback pool must not count as user configuration"
    );

    restore_env(previous);
}

// ── Host parse failure and malformed env fallthrough ──

#[test]
fn invalid_host_value_falls_back_to_loopback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous_host = env::var("BRIDGE_HOST").ok();
    env::remove_var("BRIDGE_HOST");

    env::set_var("BRIDGE_HOST", "not-a-host");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.host.to_string(),
        DEFAULT_HOST,
        "invalid BRIDGE_HOST must fall back to the loopback default"
    );

    // Same behavior when the bad value comes from TOML.
    let tmp = std::env::temp_dir().join("opencode2api_bad_host.toml");
    std::fs::write(&tmp, b"host = \"999.999.999.999\"\n").unwrap();
    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(
        config.host.to_string(),
        DEFAULT_HOST,
        "invalid TOML host must fall back to the loopback default"
    );

    match previous_host {
        Some(value) => env::set_var("BRIDGE_HOST", value),
        None => env::remove_var("BRIDGE_HOST"),
    }
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn malformed_numeric_env_falls_through_to_default() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = env::var("BRIDGE_PORT").ok();
    env::set_var("BRIDGE_PORT", "not-a-port");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.bridge_port, DEFAULT_BRIDGE_PORT,
        "malformed numeric env values must be ignored, not zero or panic"
    );

    match previous {
        Some(value) => env::set_var("BRIDGE_PORT", value),
        None => env::remove_var("BRIDGE_PORT"),
    }
}

// ── Public-bind token gates ──

#[test]
fn rest_api_token_alone_rejected_on_public_bind() {
    // REST_API_TOKEN never reaches the LLM-route admission registry, so a
    // bind justified by it alone would serve /v1/messages and
    // /v1/chat/completions unauthenticated. It must not satisfy the gate.
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: None,
        management: ManagementConfig {
            dashboard_token: Some("super-secret-admin-token-12345".into()),
            rest_api_token: Some("rest-api-token-1234567890".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(
        result.is_err(),
        "REST_API_TOKEN alone must NOT justify a public bind: {:?}",
        config.validate_security().err()
    );
    let msg = result.unwrap_err();
    assert!(
        msg.contains("SECURITY VIOLATION") && msg.contains("BRIDGE_AUTH_TOKEN"),
        "error must demand BRIDGE_AUTH_TOKEN: {msg}"
    );
    assert!(
        msg.contains("REST_API_TOKEN"),
        "error must explain why REST_API_TOKEN is insufficient: {msg}"
    );
}

#[test]
fn bridge_and_rest_api_tokens_together_pass_public_bind() {
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid-token-12345".into()]),
        management: ManagementConfig {
            dashboard_token: Some("super-secret-admin-token-12345".into()),
            rest_api_token: Some("rest-api-token-1234567890".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    assert!(
        config.validate_security().is_ok(),
        "BRIDGE_AUTH_TOKEN + REST_API_TOKEN must pass on a public bind: {:?}",
        config.validate_security().err()
    );
}

#[test]
fn loopback_bind_passes_with_either_token_alone() {
    // Loopback keeps working with only a REST API token (local dev) …
    let rest_only = BridgeConfig {
        host: "127.0.0.1".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: None,
        management: ManagementConfig {
            rest_api_token: Some("rest-api-token-1234567890".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    assert!(
        rest_only.validate_security().is_ok(),
        "loopback + REST_API_TOKEN alone must stay allowed"
    );

    // … and with only BRIDGE_AUTH_TOKEN.
    let auth_only = BridgeConfig {
        host: "127.0.0.1".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid-token-12345".into()]),
        ..Default::default()
    };
    assert!(
        auth_only.validate_security().is_ok(),
        "loopback + BRIDGE_AUTH_TOKEN alone must stay allowed: {:?}",
        auth_only.validate_security().err()
    );
}

#[test]
fn short_bridge_auth_token_rejected_on_public_bind() {
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["tiny".into(), "long-enough-token-123".into()]),
        management: ManagementConfig {
            dashboard_token: Some("super-secret-admin-token-12345".into()),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let result = config.validate_security();
    assert!(
        result.is_err(),
        "a 1-char bridge token must fail validation"
    );
    let msg = result.unwrap_err();
    assert!(
        msg.contains("too weak") && msg.contains("BRIDGE_AUTH_TOKEN"),
        "weak-token error must name BRIDGE_AUTH_TOKEN: {msg}"
    );
}

#[test]
fn empty_proxy_sources_do_not_count_as_explicit_configuration() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();

    // A comma-only list parses to zero proxies; it must NOT flip the
    // explicit-configuration flag, or `egress_mode="proxy"` would accept the
    // silently inherited built-in WARP pool that the gate exists to reject.
    env::set_var("BRIDGE_PRIMARY_PROXIES", " , ,");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.primary_proxies.as_deref(),
        Some(["socks5h://127.0.0.1:40001".to_string()].as_slice()),
        "comma-only primary list must keep the built-in default pool"
    );
    assert!(
        !config.egress.proxies_explicitly_configured,
        "comma-only BRIDGE_PRIMARY_PROXIES must not count as explicit configuration"
    );
    env::remove_var("BRIDGE_PRIMARY_PROXIES");

    env::set_var("BRIDGE_PROXIES", ",");
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(
        !config.egress.proxies_explicitly_configured,
        "comma-only legacy BRIDGE_PROXIES must not count as explicit configuration"
    );
    env::remove_var("BRIDGE_PROXIES");

    let tmp = std::env::temp_dir().join("opencode2api_empty_toml_proxies.toml");
    std::fs::write(&tmp, "primary_proxies = []\n").unwrap();
    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert!(
        !config.egress.proxies_explicitly_configured,
        "an empty TOML proxy array must not count as explicit configuration"
    );

    restore_env(previous);
    let _ = std::fs::remove_file(tmp);
}

// ── Explicit proxy-mode gate ──

#[test]
fn explicit_proxy_mode_without_explicit_proxies_rejected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();
    env::set_var("BRIDGE_EGRESS_MODE", "proxy");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    // The loader keeps materializing its built-in pool; only the gate changes.
    assert!(config.primary_proxies.is_some());

    let result = config.validate_security();
    assert!(
        result.is_err(),
        "explicit proxy mode with only inherited defaults must be rejected"
    );
    let msg = result.unwrap_err();
    assert!(
        msg.contains("primary_proxies"),
        "gate must point at the explicit proxy keys: {msg}"
    );

    restore_env(previous);
}

#[test]
fn explicit_proxy_mode_with_explicit_proxies_accepted() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = isolate_proxy_env();
    env::set_var("BRIDGE_EGRESS_MODE", "proxy");
    env::set_var("BRIDGE_PRIMARY_PROXIES", "socks5h://127.0.0.1:40009");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert!(
        config.validate_security().is_ok(),
        "explicitly configured proxies must satisfy proxy mode: {:?}",
        config.validate_security().err()
    );

    restore_env(previous);
}

#[test]
fn proxy_mode_validation_tracks_explicit_configuration_flag() {
    let mut config = BridgeConfig::default();
    config.egress.mode = EgressMode::Proxy;
    assert!(
        config.validate_security().is_err(),
        "Proxy mode over a silently inherited pool must fail validation"
    );

    // Flag alone is not enough for a hand-built config: the effective
    // resolved list must also be non-empty.
    config.egress.proxies_explicitly_configured = true;
    assert!(
        config.validate_security().is_err(),
        "empty resolved pool must keep failing the effective-list check"
    );

    config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40009".to_string()]);
    assert!(
        config.validate_security().is_ok(),
        "Proxy mode over a deliberately configured pool must pass: {:?}",
        config.validate_security().err()
    );
}

// ── Retired `max_provider_attempts` knob ──
//
// The knob was born dead: loaded from env/TOML and displayed, but enforced
// nowhere (the retry loop's only budget is max_network_attempts), while its
// documented purpose ("retry budget for non-rate-limit provider client
// errors") contradicts the deliberate fail-fast design of the retry loop.
// It is retired from every operator-facing surface and from `RetryConfig`
// itself. These tests pin backward compatibility: a stale environment value
// has no effect, while a legacy TOML key is stripped before typed parsing.

#[test]
fn retired_max_provider_attempts_env_is_ignored() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let names = ["BRIDGE_MAX_PROVIDER_ATTEMPTS", "BRIDGE_CONFIG_PATH"];
    let previous = names.map(|name| (name, env::var(name).ok()));
    env::remove_var("BRIDGE_CONFIG_PATH");
    env::set_var("BRIDGE_MAX_PROVIDER_ATTEMPTS", "7");

    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    assert_eq!(
        config.retry.max_network_attempts, 8,
        "the retired BRIDGE_MAX_PROVIDER_ATTEMPTS surface must not perturb the active retry policy"
    );

    for (name, value) in previous {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }
}

#[test]
fn retired_max_provider_attempts_toml_key_is_stripped_before_parse() {
    // Migration strips retired keys wholesale so hand-edited documents and
    // management-apply payloads never trip unknown-key gates after the
    // allowlist entry is removed.
    let (document, _) =
        super::migration::migrate_document("port = 6100\nmax_provider_attempts = 7\n")
            .expect("a document carrying only the retired key must still migrate");
    assert!(
        !document.contains("max_provider_attempts"),
        "retired key must be stripped during migration: {document}"
    );
    assert!(document.contains("port = 6100"));

    // End-to-end: the key must not reach the resolved config.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let names = [
        "BRIDGE_MAX_PROVIDER_ATTEMPTS",
        "BRIDGE_PORT",
        "BRIDGE_CONFIG_PATH",
    ];
    let previous = names.map(|name| (name, env::var(name).ok()));
    for (name, _) in &previous {
        env::remove_var(name);
    }

    let tmp = std::env::temp_dir().join("opencode2api_retired_provider_attempts.toml");
    std::fs::write(&tmp, "port = 6100\nmax_provider_attempts = 7\n").unwrap();
    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(tmp.to_string_lossy().to_string()),
        ..Default::default()
    });
    assert_eq!(config.bridge_port, 6100, "surviving keys must still apply");
    assert_eq!(
        config.retry.max_network_attempts, 8,
        "the retired TOML key must not perturb the active retry policy"
    );

    for (name, value) in previous {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }
    let _ = std::fs::remove_file(tmp);
}
