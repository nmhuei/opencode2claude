//! Configuration loading and validation.
//!
//! Reads all settings from environment variables with sensible defaults.
//! Priority: Environment variables > Hardcoded defaults.

use crate::shell::ShellPolicy;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::net::IpAddr;
use tracing::warn;

/// Default values used when environment variables are not set.
pub const DEFAULT_BRIDGE_PORT: u16 = 4000;
pub const DEFAULT_OPENCODE_PORT: u16 = 4096;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_MODEL: &str = "claude-3-5-sonnet";
pub const DEFAULT_STREAM_BUFFER_SIZE: usize = 4096;
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;
pub const DEFAULT_MAX_BODY_SIZE: usize = 1_048_576; // 1MB

/// Message IDs used in Anthropic SSE protocol responses.
pub const MSG_ID_SHELL: &str = "msg_local_shell";

/// Schema for `opencode2claude.toml` configuration file.
#[derive(Debug, Deserialize, Default)]
pub struct TomlConfig {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub opencode_port: Option<u16>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub shell_allowlist: Option<String>,
    pub auth_tokens: Option<String>,
    pub max_body_size: Option<usize>,
    pub stream_buffer_size: Option<usize>,
    pub channel_capacity: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub max_search_loops: Option<u32>,
    pub proxies: Option<Vec<String>>,
}

impl TomlConfig {
    pub fn from_file(path: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }
}

/// CLI argument overrides that take highest priority in the config chain.
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub bridge_port: Option<u16>,
    pub host: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub config_path: Option<String>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
}

/// Central configuration struct for the bridge.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Host address to bind to (default: 127.0.0.1)
    pub host: IpAddr,
    /// Port for the API bridge (default: 4000)
    pub bridge_port: u16,
    /// Port for the OpenCode daemon (default: 4096)
    pub opencode_port: u16,
    /// Target LLM model identifier
    pub model: Option<String>,
    /// Shell command execution policy
    pub shell_policy: ShellPolicy,
    /// Authentication tokens (None = auth disabled)
    pub auth_tokens: Option<Vec<String>>,
    /// Maximum request body size in bytes
    pub max_body_size: usize,
    /// Buffer size for streaming reads
    pub stream_buffer_size: usize,
    /// Channel capacity for SSE event queue
    pub channel_capacity: usize,
    /// Tavily API key for web search
    pub tavily_api_key: Option<String>,
    /// Exa API key for web search
    pub exa_api_key: Option<String>,
    /// Serper.dev API key for web search
    pub serper_api_key: Option<String>,
    /// SearXNG self-hosted instance URL
    pub searxng_url: Option<String>,
    /// SearXNG API key
    #[allow(dead_code)]
    pub searxng_api_key: Option<String>,
    /// Maximum number of search loops
    #[allow(dead_code)]
    pub max_search_loops: u32,
    /// Comma-separated list of SOCKS5/HTTP proxies for multi-agent support
    /// (deprecated, use primary_proxies + warm_standby_proxies instead)
    #[allow(dead_code)]
    pub proxies: Option<Vec<String>>,
    /// Primary proxy URLs (managed, restartable)
    pub primary_proxies: Option<Vec<String>>,
    /// Warm-standby proxy URLs (protected, never restarted)
    pub warm_standby_proxies: Option<Vec<String>>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 0,
            model: None,
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: DEFAULT_MAX_BODY_SIZE,
            stream_buffer_size: DEFAULT_STREAM_BUFFER_SIZE,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            tavily_api_key: None,
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 5,
            proxies: None,
            primary_proxies: None,
            warm_standby_proxies: None,
        }
    }
}

impl BridgeConfig {
    /// Load configuration with priority: CLI args > Env vars > TOML file > Defaults.
    pub fn from_env_and_cli(overrides: CliOverrides) -> Self {
        let config_path = overrides
            .config_path
            .as_deref()
            .unwrap_or("opencode2claude.toml");
        let toml_config = TomlConfig::from_file(config_path);

        // Host: CLI > Env > TOML > Default
        let host_str = overrides
            .host
            .or_else(|| env::var("BRIDGE_HOST").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.host.clone()))
            .unwrap_or_else(|| DEFAULT_HOST.to_string());
        let host: IpAddr = host_str
            .parse()
            .unwrap_or_else(|_| DEFAULT_HOST.parse().unwrap());

        if host.to_string() == "0.0.0.0" {
            warn!("⚠️  Bridge is binding to 0.0.0.0 — accessible from ALL network interfaces. Consider using 127.0.0.1 for local-only access.");
        }

        // Bridge port: CLI > Env > TOML > Default
        let bridge_port = overrides
            .bridge_port
            .or_else(|| env::var("BRIDGE_PORT").ok().and_then(|v| v.parse().ok()))
            .or_else(|| toml_config.as_ref().and_then(|t| t.port))
            .unwrap_or(DEFAULT_BRIDGE_PORT);

        // OpenCode port: Env > TOML > Default (no CLI flag)
        let opencode_port: u16 = env::var("OPENCODE_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.opencode_port))
            .unwrap_or(DEFAULT_OPENCODE_PORT);

        // Model: CLI > Env > TOML
        let model = overrides
            .model
            .or_else(|| env::var("OPENCODE_MODEL").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.model.clone()));

        // Shell policy: CLI > Env > TOML > Default
        let raw_shell_policy = overrides
            .shell_policy
            .or_else(|| env::var("BRIDGE_SHELL_POLICY").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.shell_policy.clone()))
            .unwrap_or_else(|| "disabled".to_string());

        // Shell allowlist: Env > TOML > Default
        let shell_allowlist_str = env::var("BRIDGE_SHELL_ALLOWLIST")
            .ok()
            .or_else(|| toml_config.as_ref().and_then(|t| t.shell_allowlist.clone()))
            .unwrap_or_else(|| "git,ls,pwd,cat,find,grep,echo,wc,head,tail,diff".to_string());

        let shell_policy = match raw_shell_policy.to_lowercase().as_str() {
            "disabled" => ShellPolicy::Disabled,
            "allowlist" => {
                let allowed: HashSet<String> = shell_allowlist_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                ShellPolicy::AllowList(allowed)
            }
            "unrestricted" => ShellPolicy::Unrestricted,
            _ => {
                warn!(
                    "Unknown shell policy '{}' — defaulting to Disabled for security. Valid values: 'disabled', 'allowlist', 'unrestricted'",
                    raw_shell_policy
                );
                ShellPolicy::Disabled
            }
        };

        // Auth tokens: Env > TOML
        let auth_tokens = env::var("BRIDGE_AUTH_TOKEN")
            .ok()
            .or_else(|| toml_config.as_ref().and_then(|t| t.auth_tokens.clone()))
            .map(|tokens| {
                tokens
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            });

        let max_body_size = env::var("BRIDGE_MAX_BODY_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.max_body_size))
            .unwrap_or(DEFAULT_MAX_BODY_SIZE);

        let stream_buffer_size = env::var("BRIDGE_STREAM_BUFFER_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.stream_buffer_size))
            .unwrap_or(DEFAULT_STREAM_BUFFER_SIZE);

        let channel_capacity = env::var("BRIDGE_CHANNEL_CAPACITY")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.channel_capacity))
            .unwrap_or(DEFAULT_CHANNEL_CAPACITY);

        let tavily_api_key = overrides
            .tavily_api_key
            .or_else(|| env::var("TAVILY_API_KEY").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.tavily_api_key.clone()));

        let exa_api_key = overrides
            .exa_api_key
            .or_else(|| env::var("EXA_API_KEY").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.exa_api_key.clone()));

        let serper_api_key = overrides
            .serper_api_key
            .or_else(|| env::var("SERPER_API_KEY").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.serper_api_key.clone()));

        let searxng_url = overrides
            .searxng_url
            .or_else(|| env::var("SEARXNG_URL").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.searxng_url.clone()));

        let searxng_api_key = overrides
            .searxng_api_key
            .or_else(|| env::var("SEARXNG_API_KEY").ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.searxng_api_key.clone()));

        let max_search_loops = env::var("BRIDGE_MAX_SEARCH_LOOPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| toml_config.as_ref().and_then(|t| t.max_search_loops))
            .unwrap_or(5);

        let proxies = env::var("BRIDGE_PROXIES")
            .ok()
            .or_else(|| {
                toml_config
                    .as_ref()
                    .and_then(|t| t.proxies.as_ref().map(|p| p.join(",")))
            })
            .map(|s| {
                s.split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<String>>()
            });

        // Primary proxies: BRIDGE_PRIMARY_PROXIES env var > derived from TOML proxies > default
        let primary_proxies = env::var("BRIDGE_PRIMARY_PROXIES")
            .ok()
            .or_else(|| {
                toml_config
                    .as_ref()
                    .and_then(|t| t.proxies.as_ref().map(|p| p.join(",")))
            })
            .or_else(|| {
                Some(
                    "socks5://127.0.0.1:40001,socks5://127.0.0.1:40002,socks5://127.0.0.1:40003"
                        .to_string(),
                )
            })
            .map(|s| {
                s.split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<String>>()
            });

        // Warm-standby proxies: BRIDGE_WARM_STANDBY_PROXIES env var > default
        let warm_standby_proxies = env::var("BRIDGE_WARM_STANDBY_PROXIES")
            .ok()
            .or_else(|| Some("socks5://127.0.0.1:40004,socks5://127.0.0.1:40005".to_string()))
            .map(|s| {
                s.split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<String>>()
            });

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

    /// Returns true if authentication is enabled.
    pub fn auth_enabled(&self) -> bool {
        self.auth_tokens.is_some()
    }

    /// Check if a given token is valid.
    #[allow(dead_code)]
    pub fn is_valid_token(&self, token: &str) -> bool {
        match &self.auth_tokens {
            Some(tokens) => tokens.iter().any(|t| t == token),
            None => true, // No auth configured = all tokens valid
        }
    }

    /// Validate security-sensitive configuration before binding the HTTP server.
    ///
    /// Returns an error with an actionable message if the configuration is unsafe.
    /// Call this after loading config and before starting the server.
    ///
    /// # Checks
    ///
    /// 1. **Public bind without auth** — non-loopback addresses must have auth enabled.
    /// 2. **Public bind + unrestricted shell** — non-loopback with unrestricted shell
    ///    is denied regardless of auth status.
    pub fn validate_security(&self) -> Result<(), String> {
        let is_loopback = self.host.is_loopback();

        if is_loopback {
            return Ok(());
        }

        // Non-loopback bind (e.g. 0.0.0.0 or ::) requires auth
        if !self.auth_enabled() {
            return Err(
                "SECURITY VIOLATION: Binding to a non-loopback address without authentication.\n"
                    .to_string()
                    + "  Set BRIDGE_AUTH_TOKEN to require authentication before binding publicly.\n"
                    + "  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n"
                    + "  Current host: " + &self.host.to_string(),
            );
        }

        // Non-loopback bind with unrestricted shell is always denied
        if matches!(self.shell_policy, ShellPolicy::Unrestricted) {
            return Err(
                "SECURITY VIOLATION: Binding to a non-loopback address with unrestricted shell policy.\n"
                    .to_string()
                    + "  Set BRIDGE_SHELL_POLICY=disabled or configure an allowlist.\n"
                    + "  Or set BRIDGE_HOST=127.0.0.1 to restrict to localhost only.\n"
                    + "  Current host: "
                    + &self.host.to_string(),
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let tmp = std::env::temp_dir().join("opencode2claude_test_loading.toml");
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&tmp, b"port = 6000\nhost = \"127.0.0.1\"\n").unwrap();

        let config = TomlConfig::from_file(tmp.to_string_lossy().as_ref()).unwrap();
        assert_eq!(config.port, Some(6000));
        assert_eq!(config.host.as_deref(), Some("127.0.0.1"));

        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn test_toml_file_not_found() {
        let config = TomlConfig::from_file("/tmp/nonexistent_opencode2claude_test.toml");
        assert!(config.is_none());
    }

    #[test]
    fn test_env_overrides_toml() {
        let _lock = ENV_LOCK.lock().unwrap();
        env::remove_var("BRIDGE_PORT");
        env::remove_var("BRIDGE_HOST");

        let tmp = std::env::temp_dir().join("opencode2claude_test_env_override.toml");
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

        let tmp = std::env::temp_dir().join("opencode2claude_test_defaults.toml");
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
        // 0.0.0.0 without auth — rejected
        let config = BridgeConfig {
            host: "0.0.0.0".parse().unwrap(),
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
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
        // 0.0.0.0 with auth — OK
        let config = BridgeConfig {
            host: "0.0.0.0".parse().unwrap(),
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: Some(vec!["sk-valid".to_string()]),
            ..Default::default()
        };
        assert!(
            config.validate_security().is_ok(),
            "public bind with auth must be allowed"
        );
    }

    #[test]
    fn test_security_public_bind_with_unrestricted_shell_rejected() {
        // 0.0.0.0 + unrestricted shell — rejected regardless of auth
        let config = BridgeConfig {
            host: "0.0.0.0".parse().unwrap(),
            shell_policy: ShellPolicy::Unrestricted,
            auth_tokens: Some(vec!["sk-valid".to_string()]),
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
}
