//! Configuration schema, loading, precedence, and security validation.
//!
//! Precedence is intentionally explicit: CLI overrides environment variables,
//! environment variables override TOML, and TOML overrides defaults.

mod file;
mod loader;
pub mod migration;
mod security;
mod types;

pub use file::{StringList, TomlConfig};
pub use types::{
    BridgeConfig, CliOverrides, EgressConfig, EgressMode, HistoryCaptureMode, HistoryConfig,
    ManagementConfig, ObservabilityConfig, ProtocolConfig, RetryConfig, RuntimeConfig,
    SearchConfig, SecretString,
};

pub const DEFAULT_BRIDGE_PORT: u16 = 4000;
pub const DEFAULT_OPENCODE_PORT: u16 = 4096;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_MODEL: &str = "claude-3-5-sonnet";
pub const DEFAULT_STREAM_BUFFER_SIZE: usize = 64 * 1024;
pub const DEFAULT_CHANNEL_CAPACITY: usize = 2048;
pub const DEFAULT_MAX_BODY_SIZE: usize = 64 * 1024 * 1024;
pub const MSG_ID_SHELL: &str = "msg_local_shell";

/// Load environment variables from a deterministic `.env` location.
///
/// All process-environment access lives in `loader`; this wrapper preserves
/// the existing public bootstrap API without opening a second config boundary.
pub fn load_dotenv() -> Option<std::path::PathBuf> {
    loader::load_dotenv()
}

pub(crate) use loader::{ambient_home, ambient_shell, ambient_zdotdir, terminal_columns};

const DEFAULT_SHELL_ALLOWLIST: &str = "git,ls,pwd,cat,find,grep,echo,wc,head,tail,diff";
const DEFAULT_PRIMARY_PROXIES: &str = "socks5h://127.0.0.1:40001";
const DEFAULT_WARM_STANDBY_PROXIES: &str = "socks5h://127.0.0.1:40004";

/// Force SOCKS5 hostname resolution through the proxy.
///
/// Local DNS resolution (`socks5://`) can select IPv6 for one identity
/// endpoint and IPv4 for another, making the same WARP exit fail consensus.
pub(crate) fn normalize_proxy_url(value: &str) -> String {
    match value.split_once("://") {
        Some((scheme, remainder)) if scheme.eq_ignore_ascii_case("socks5") => {
            format!("socks5h://{remainder}")
        }
        Some((scheme, remainder)) if scheme.eq_ignore_ascii_case("socks5h") => {
            format!("socks5h://{remainder}")
        }
        _ => value.to_string(),
    }
}

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
