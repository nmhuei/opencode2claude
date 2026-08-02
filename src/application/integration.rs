//! Client integration values shared by CLI and dashboard.

use crate::config::BridgeConfig;
use serde::Serialize;

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
    ];
    if let Some(model) = config
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        exports.push(format!("export OPENCODE_MODEL={}", shell_quote(model)));
    }
    IntegrationEnvironment {
        anthropic_base_url: base.clone(),
        openai_base_url: format!("{base}/v1"),
        api_key: key,
        model: config.model.clone(),
        shell_exports: exports,
    }
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
}
