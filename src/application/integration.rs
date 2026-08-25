//! Client integration values shared by CLI and dashboard.

use crate::config::BridgeConfig;
use serde::Serialize;

pub const OX_ALPHA_MODEL: &str = "opencode/x-preview-f-free";
pub const OX_ALPHA_CLAUDE_MODEL: &str = "sonnet[1m]";
pub const OX_ALPHA_MAX_OUTPUT_TOKENS: &str = "128000";
pub const OX_ALPHA_AUTO_COMPACT_WINDOW: &str = "870000";
pub const OX_ALPHA_MAX_THINKING_TOKENS: &str = "120000";

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationEnvironment {
    pub anthropic_base_url: String,
    pub openai_base_url: String,
    pub api_key: String,
    pub model: Option<String>,
    pub shell_exports: Vec<String>,
}

pub fn api_key(config: &BridgeConfig) -> &str {
    config
        .auth_tokens
        .as_ref()
        .and_then(|tokens| tokens.first())
        .map(|token| token.expose())
        .unwrap_or("opencode-bridge")
}

pub fn base_url(config: &BridgeConfig) -> String {
    format!("http://127.0.0.1:{}", config.bridge_port)
}

pub fn environment(config: &BridgeConfig) -> IntegrationEnvironment {
    let base = base_url(config);
    let key = api_key(config).to_string();
    let mut exports = vec![
        format!("export ANTHROPIC_API_KEY={}", shell_quote(&key)),
        format!("export ANTHROPIC_BASE_URL={}", shell_quote(&base)),
        format!("export OPENAI_API_KEY={}", shell_quote(&key)),
        format!(
            "export OPENAI_BASE_URL={}",
            shell_quote(&format!("{base}/v1"))
        ),
        "unset ANTHROPIC_AUTH_TOKEN".to_string(),
    ];
    let effective_model = config
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(OX_ALPHA_MODEL);

    exports.push(format!("export OPENCODE_MODEL={}", shell_quote(effective_model)));
    if effective_model == OX_ALPHA_MODEL {
        exports.extend(ox_alpha_claude_code_exports());
    }

    IntegrationEnvironment {
        anthropic_base_url: base.clone(),
        openai_base_url: format!("{base}/v1"),
        api_key: key,
        model: Some(effective_model.to_string()),
        shell_exports: exports,
    }
}

pub fn ox_alpha_claude_code_exports() -> Vec<String> {
    [
        ("ANTHROPIC_MODEL", OX_ALPHA_CLAUDE_MODEL),
        ("CLAUDE_CODE_DISABLE_1M_CONTEXT", "0"),
        ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", OX_ALPHA_MAX_OUTPUT_TOKENS),
        (
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            OX_ALPHA_AUTO_COMPACT_WINDOW,
        ),
        ("CLAUDE_CODE_DISABLE_THINKING", "0"),
        ("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "0"),
        ("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT", "1"),
        ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
        ("MAX_THINKING_TOKENS", OX_ALPHA_MAX_THINKING_TOKENS),
    ]
    .into_iter()
    .map(|(key, value)| format!("export {key}={}", shell_quote(value)))
    .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_contains_anthropic_and_openai_values() {
        let config = BridgeConfig {
            bridge_port: 4555,
            model: Some("opencode/test".to_string()),
            auth_tokens: Some(vec!["secret'value".into()]),
            ..Default::default()
        };
        let env = environment(&config);
        assert_eq!(env.anthropic_base_url, "http://127.0.0.1:4555");
        assert_eq!(env.openai_base_url, "http://127.0.0.1:4555/v1");
        assert!(env
            .shell_exports
            .iter()
            .any(|line| line.contains("OPENAI_BASE_URL")));
        assert!(env
            .shell_exports
            .iter()
            .all(|line| !line.contains("secret'value")));
    }

    #[test]
    fn ox_alpha_environment_exports_verified_claude_code_session_tuning() {
        let config = BridgeConfig {
            model: Some("opencode/x-preview-f-free".to_string()),
            ..Default::default()
        };

        let exports = environment(&config).shell_exports;
        for expected in [
            "unset ANTHROPIC_AUTH_TOKEN",
            "export ANTHROPIC_MODEL='sonnet[1m]'",
            "export CLAUDE_CODE_DISABLE_1M_CONTEXT='0'",
            "export CLAUDE_CODE_MAX_OUTPUT_TOKENS='128000'",
            "export CLAUDE_CODE_AUTO_COMPACT_WINDOW='870000'",
            "export CLAUDE_CODE_DISABLE_THINKING='0'",
            "export CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING='0'",
            "export CLAUDE_CODE_ALWAYS_ENABLE_EFFORT='1'",
            "export CLAUDE_CODE_EFFORT_LEVEL='max'",
            "export MAX_THINKING_TOKENS='120000'",
        ] {
            assert!(
                exports.iter().any(|line| line == expected),
                "missing {expected}"
            );
        }
    }
}
