//! Configuration schema, loading, precedence, and security validation.
//!
//! Precedence is intentionally explicit: CLI overrides environment variables,
//! environment variables override TOML, and TOML overrides defaults.

mod file;
mod loader;
mod security;
mod types;

pub use file::{StringList, TomlConfig};
pub use types::{
    BridgeConfig, CliOverrides, EgressConfig, EgressMode, ManagementConfig, ObservabilityConfig,
    ProtocolConfig, RetryConfig, RuntimeConfig, SecretString,
};

pub const DEFAULT_BRIDGE_PORT: u16 = 4000;
pub const DEFAULT_OPENCODE_PORT: u16 = 4096;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_MODEL: &str = "claude-3-5-sonnet";
pub const DEFAULT_STREAM_BUFFER_SIZE: usize = 4096;
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;
pub const DEFAULT_MAX_BODY_SIZE: usize = 10 * 1024 * 1024;
pub const MSG_ID_SHELL: &str = "msg_local_shell";

const DEFAULT_SHELL_ALLOWLIST: &str = "git,ls,pwd,cat,find,grep,echo,wc,head,tail,diff";
const DEFAULT_PRIMARY_PROXIES: &str =
    "socks5://127.0.0.1:40001,socks5://127.0.0.1:40002,socks5://127.0.0.1:40003";
const DEFAULT_WARM_STANDBY_PROXIES: &str = "socks5://127.0.0.1:40004,socks5://127.0.0.1:40005";

impl BridgeConfig {
    pub fn from_env_and_cli(overrides: CliOverrides) -> Self {
        loader::load(overrides)
    }

    pub fn auth_enabled(&self) -> bool {
        self.auth_tokens
            .as_ref()
            .is_some_and(|tokens| !tokens.is_empty())
    }

    #[allow(dead_code)]
    pub fn is_valid_token(&self, token: &str) -> bool {
        self.auth_tokens.as_ref().is_none_or(|tokens| {
            tokens.iter().any(|candidate| {
                crate::management::auth::token_eq(candidate.expose().as_bytes(), token.as_bytes())
            })
        })
    }

    pub fn validate_security(&self) -> Result<(), String> {
        security::validate(self)
    }
}

#[cfg(test)]
mod tests;
