//! Client integration values shared by CLI and dashboard.

use crate::config::BridgeConfig;
use serde::Serialize;

pub const OX_ALPHA_MODEL: &str = "opencode/x-preview-f-free";
pub const OX_ALPHA_CLAUDE_MODEL: &str = "sonnet[1m]";
pub const OX_ALPHA_MAX_OUTPUT_TOKENS: &str = "128000";
pub const OX_ALPHA_AUTO_COMPACT_WINDOW: &str = "800000";
pub const OX_ALPHA_MAX_THINKING_TOKENS: &str = "120000";

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationEnvironment {
    pub anthropic_base_url: String,
    pub openai_base_url: String,
    pub api_key: String,
    pub model: Option<String>,
    pub shell_exports: Vec<String>,
}

/// Compile-time fallback credential for local loopback setups with no
/// authentication configured anywhere. The auth middleware only honors it
/// under those exact conditions; it never substitutes for a real credential.
pub const FALLBACK_API_KEY: &str = "opencode-bridge";

pub fn api_key(config: &BridgeConfig) -> &str {
    config
        .auth_tokens
        .as_ref()
        .and_then(|tokens| tokens.first())
        .map(|token| token.expose())
        .unwrap_or(FALLBACK_API_KEY)
}

pub fn base_url(config: &BridgeConfig) -> String {
    format!("http://127.0.0.1:{}", config.bridge_port)
}

pub fn process_environment(config: &BridgeConfig) -> Vec<(String, Option<String>)> {
    let base = base_url(config);
    let key = api_key(config).to_string();
    let effective_model = config
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(OX_ALPHA_MODEL)
        .to_string();
    let profile = client_profile(config, &effective_model);
    let claude_vars = configured_alias_vars(config)
        .unwrap_or_else(|| model_claude_code_vars(&profile));

    let mut vars = vec![
        ("ANTHROPIC_API_KEY".to_string(), Some(key.clone())),
        ("ANTHROPIC_BASE_URL".to_string(), Some(base.clone())),
        ("OPENAI_API_KEY".to_string(), Some(key.clone())),
        ("OPENAI_BASE_URL".to_string(), Some(format!("{base}/v1"))),
        ("ANTHROPIC_AUTH_TOKEN".to_string(), Some(key)),
        (
            "ANTHROPIC_MODEL".to_string(),
            Some(client_model_alias(config)),
        ),
        ("OPENCODE_MODEL".to_string(), Some(effective_model.clone())),
    ];

    for (k, v) in claude_vars {
        vars.push((k.to_string(), Some(v)));
    }

    vars
}

pub fn environment(config: &BridgeConfig) -> IntegrationEnvironment {
    let base = base_url(config);
    let key = api_key(config).to_string();
    let effective_model = config
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(OX_ALPHA_MODEL)
        .to_string();
    let profile = client_profile(config, &effective_model);

    let shell_exports = process_environment(config)
        .into_iter()
        .map(|(key, value)| match value {
            Some(value) => format!("export {key}={}", shell_quote(&value)),
            None => format!("unset {key}"),
        })
        .collect();

    IntegrationEnvironment {
        anthropic_base_url: base.clone(),
        openai_base_url: format!("{base}/v1"),
        api_key: key,
        model: Some(profile.client_model_alias().to_string()),
        shell_exports,
    }
}

fn configured_alias_vars(
    config: &BridgeConfig,
) -> Option<Vec<(&'static str, String)>> {
    let registry = config.provider_registry.as_deref()?;
    let alias_id = config.active_alias.as_deref()?;
    let alias = registry.alias(alias_id)?;
    let (max_output_tokens, supports_thinking) = alias
        .candidates
        .iter()
        .filter_map(|candidate| {
            registry
                .model(&candidate.provider_id, &candidate.model_id)
        })
        .next()
        .map(|model| {
            (
                model.max_output_tokens.unwrap_or(128_000),
                model.supports_thinking,
            )
        })
        .unwrap_or((128_000, false));
    Some(model_claude_code_vars_for_context(
        alias.context_window,
        max_output_tokens,
        supports_thinking,
    ))
}

/// Resolve the client contract independently from the upstream wire model.
/// A configured alias is authoritative for Claude Code identity; the legacy
/// model catalog remains the fallback for old configurations.
fn client_profile(
    config: &BridgeConfig,
    effective_model: &str,
) -> crate::application::models::ModelProfile {
    if config
        .active_alias
        .as_deref()
        .is_some_and(|alias| alias.contains("1m") || alias.contains("million"))
    {
        return crate::application::models::ModelProfile::from_context(
            OX_ALPHA_MODEL,
            1_000_000,
            128_000,
            true,
        );
    }
    crate::application::models::resolve_model_profile(effective_model)
}

pub fn client_model_alias(config: &BridgeConfig) -> String {
    if let (Some(registry), Some(alias_id)) = (
        config.provider_registry.as_deref(),
        config.active_alias.as_deref(),
    ) {
        if let Some(alias) = registry.alias(alias_id) {
            return alias.client_model.clone();
        }
    }
    let model = config
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(OX_ALPHA_MODEL);
    client_profile(config, model)
        .client_model_alias()
        .to_string()
}

pub fn model_claude_code_vars(
    profile: &crate::application::models::ModelProfile,
) -> Vec<(&'static str, String)> {
    model_claude_code_vars_for_context(
        profile.context_window,
        profile.max_output_tokens,
        profile.supports_thinking,
    )
}

pub fn model_claude_code_vars_for_context(
    context_window: usize,
    max_output_tokens: usize,
    supports_thinking: bool,
) -> Vec<(&'static str, String)> {
    let auto_compact = ((context_window / 100) * 80 + ((context_window % 100) * 80) / 100)
        .to_string();
    let max_output = max_output_tokens.to_string();
    let disable_1m = if context_window >= 1_000_000 {
        "0"
    } else {
        "1"
    };
    let (disable_thinking, disable_adaptive, effort) = if supports_thinking {
        ("0", "0", "1")
    } else {
        ("1", "1", "0")
    };
    let max_thinking = max_output_tokens
        .saturating_sub(1024)
        .min(120_000)
        .to_string();

    vec![
        ("CLAUDE_CODE_DISABLE_1M_CONTEXT", disable_1m.to_string()),
        (
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            context_window.to_string(),
        ),
        ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", max_output),
        ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", auto_compact),
        ("CLAUDE_CODE_DISABLE_THINKING", disable_thinking.to_string()),
        (
            "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING",
            disable_adaptive.to_string(),
        ),
        ("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT", effort.to_string()),
        ("MAX_THINKING_TOKENS", max_thinking),
    ]
}

pub fn ox_alpha_claude_code_exports() -> Vec<String> {
    let profile = crate::application::models::resolve_model_profile(OX_ALPHA_MODEL);
    std::iter::once(("ANTHROPIC_MODEL", profile.client_model_alias().to_string()))
        .chain(model_claude_code_vars(&profile))
        .map(|(key, value)| format!("export {key}={}", shell_quote(&value)))
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
    fn dynamic_environment_exports_80_percent_autocompact_window() {
        let config_mimo = BridgeConfig {
            model: Some("opencode/mimo-v2.5-free".to_string()),
            ..Default::default()
        };
        let exports = environment(&config_mimo).shell_exports;
        assert!(exports
            .iter()
            .any(|line| line == "export ANTHROPIC_MODEL='claude-sonnet-5'"));
        assert!(exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_AUTO_COMPACT_WINDOW='204800'"));
        assert!(
            exports
                .iter()
                .all(|line| !line.starts_with("export CLAUDE_CODE_EFFORT_LEVEL=")),
            "the bridge must not override the user's Claude Code effort selection"
        );
        assert!(
            exports
                .iter()
                .all(|line| line != "export DISABLE_COMPACT='1'"),
            "auto-compact tuning must never be paired with DISABLE_COMPACT"
        );
        assert!(exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_MAX_OUTPUT_TOKENS='64000'"));

        let config_nemotron = BridgeConfig {
            model: Some("opencode/nemotron-3-ultra-free".to_string()),
            ..Default::default()
        };
        let exports_nemotron = environment(&config_nemotron).shell_exports;
        assert!(exports_nemotron
            .iter()
            .any(|line| line == "export CLAUDE_CODE_AUTO_COMPACT_WINDOW='102400'"));

        let million = BridgeConfig {
            model: Some("opencode/x-preview-f-free".to_string()),
            ..Default::default()
        };
        let exports_million = environment(&million).shell_exports;
        assert!(exports_million
            .iter()
            .any(|line| line == "export CLAUDE_CODE_MAX_CONTEXT_TOKENS='1000000'"));
        assert!(exports_million
            .iter()
            .any(|line| line == "export CLAUDE_CODE_AUTO_COMPACT_WINDOW='800000'"));
        assert!(exports_million
            .iter()
            .all(|line| line != "export DISABLE_COMPACT='1'"));

        let deepseek = BridgeConfig {
            model: Some("deepseek-v4-flash".to_string()),
            ..Default::default()
        };
        let deepseek_exports = environment(&deepseek).shell_exports;
        assert!(deepseek_exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_MAX_CONTEXT_TOKENS='1000000'"));
        assert!(deepseek_exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_AUTO_COMPACT_WINDOW='800000'"));
        assert!(deepseek_exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_MAX_OUTPUT_TOKENS='384000'"));

        let glm = BridgeConfig {
            model: Some("glm-5.3-flash".to_string()),
            ..Default::default()
        };
        let glm_exports = environment(&glm).shell_exports;
        assert!(glm_exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_MAX_CONTEXT_TOKENS='1000000'"));
        assert!(glm_exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_AUTO_COMPACT_WINDOW='800000'"));
        assert!(glm_exports
            .iter()
            .any(|line| line == "export CLAUDE_CODE_MAX_OUTPUT_TOKENS='131072'"));
        assert!(glm_exports
            .iter()
            .any(|line| line == "export ANTHROPIC_MODEL='sonnet[1m]'"));
    }
}
