use super::*;
use crate::shell::ShellPolicy;
use std::env;
use std::sync::Mutex;

/// Serializes tests that modify process-level environment variables.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_default_config() {
    let _lock = ENV_LOCK.lock().unwrap();
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
    assert!(
        matches!(config.shell_policy, ShellPolicy::Disabled),
        "default shell policy must be Disabled for security reasons"
    );
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
    assert_eq!(config.auth_tokens.as_deref(), Some("token1,token2"));
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
    let _lock = ENV_LOCK.lock().unwrap();
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
    let _lock = ENV_LOCK.lock().unwrap();
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
    let _lock = ENV_LOCK.lock().unwrap();
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
    let _lock = ENV_LOCK.lock().unwrap();
    env::remove_var("BRIDGE_AUTH_TOKEN");

    let mut config = BridgeConfig::from_env_and_cli(CliOverrides::default());

    // No auth configured — everything is valid
    config.auth_tokens = None;
    assert!(config.is_valid_token("anything"));

    // Auth configured — only matching tokens are valid
    config.auth_tokens = Some(vec!["secret-123".to_string(), "secret-456".to_string()]);
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
    let _lock = ENV_LOCK.lock().unwrap();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "super-secret-admin-token-12345");
    // 0.0.0.0 without auth — rejected
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: None,
        ..Default::default()
    };
    let result = config.validate_security();
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
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
    let _lock = ENV_LOCK.lock().unwrap();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "super-secret-admin-token-12345");
    // 0.0.0.0 with auth — OK
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid".to_string()]),
        ..Default::default()
    };
    let result = config.validate_security();
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    assert!(
        result.is_ok(),
        "public bind with auth must be allowed: {:?}",
        result.err()
    );
}

#[test]
fn test_security_public_bind_with_unrestricted_shell_rejected() {
    let _lock = ENV_LOCK.lock().unwrap();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "super-secret-admin-token-12345");
    // 0.0.0.0 + unrestricted shell — rejected regardless of auth
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Unrestricted,
        auth_tokens: Some(vec!["sk-valid".to_string()]),
        ..Default::default()
    };
    let result = config.validate_security();
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
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
    let _lock = ENV_LOCK.lock().unwrap();
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid".to_string()]),
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
    let _lock = ENV_LOCK.lock().unwrap();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "12345"); // too short (5 < 8)
    let config = BridgeConfig {
        host: "0.0.0.0".parse().unwrap(),
        shell_policy: ShellPolicy::Disabled,
        auth_tokens: Some(vec!["sk-valid".to_string()]),
        ..Default::default()
    };
    let result = config.validate_security();
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    assert!(
        result.is_err(),
        "public bind with weak dashboard token must be rejected"
    );
    assert!(result.unwrap_err().contains("too weak"));
}

#[test]
fn test_security_default_shell_policy_is_disabled() {
    let _lock = ENV_LOCK.lock().unwrap();
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
    let _lock = ENV_LOCK.lock().unwrap();
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
    let _lock = ENV_LOCK.lock().unwrap();
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
