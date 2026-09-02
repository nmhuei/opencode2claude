use opencode2api::application::client_config::{generate, ClientConfigFormat};
use opencode2api::application::integration::{
    environment, model_claude_code_vars, process_environment,
};
use opencode2api::application::models::ModelProfile;
use opencode2api::config::BridgeConfig;

fn value<'a>(vars: &'a [(&'static str, String)], key: &str) -> &'a str {
    vars.iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("missing environment variable {key}"))
}

fn process_value<'a>(vars: &'a [(String, Option<String>)], key: &str) -> &'a str {
    vars.iter()
        .find(|(candidate, _)| candidate == key)
        .and_then(|(_, value)| value.as_deref())
        .unwrap_or_else(|| panic!("missing process environment variable {key}"))
}

#[test]
fn launcher_environment_preserves_upstream_id_and_exposes_1m_client_contract() {
    let profile = ModelProfile::from_context("route/free-1m", 1_000_000, 128_000, true);
    let vars = model_claude_code_vars(&profile);

    assert_eq!(profile.id, "route/free-1m");
    assert_eq!(profile.client_model_alias(), "sonnet[1m]");
    assert_eq!(value(&vars, "CLAUDE_CODE_DISABLE_1M_CONTEXT"), "0");
    assert_eq!(value(&vars, "CLAUDE_CODE_MAX_CONTEXT_TOKENS"), "1000000");
    assert_eq!(value(&vars, "CLAUDE_CODE_MAX_OUTPUT_TOKENS"), "128000");
    assert_eq!(value(&vars, "CLAUDE_CODE_AUTO_COMPACT_WINDOW"), "800000");

    let config = BridgeConfig {
        bridge_port: 4567,
        model: Some("opencode/deepseek-v4-flash-free".to_string()),
        ..Default::default()
    };
    let process_vars = process_environment(&config);
    assert_eq!(
        process_value(&process_vars, "OPENCODE_MODEL"),
        "opencode/deepseek-v4-flash-free"
    );
    assert_eq!(
        process_value(&process_vars, "ANTHROPIC_AUTH_TOKEN"),
        "opencode-bridge"
    );
    assert_eq!(
        process_value(&process_vars, "ANTHROPIC_MODEL"),
        "sonnet[1m]"
    );
}

#[test]
fn generated_claude_code_settings_use_alias_and_1m_environment() {
    let config = BridgeConfig {
        bridge_port: 4567,
        model: Some("opencode/deepseek-v4-flash-free".to_string()),
        ..Default::default()
    };
    let generated = generate(
        ClientConfigFormat::ClaudeCode,
        &environment(&config),
        "sk-oc2-test",
        true,
    );
    let settings: serde_json::Value = serde_json::from_str(&generated.content).unwrap();

    assert_eq!(settings["model"], "sonnet[1m]");
    assert_eq!(settings["env"]["ANTHROPIC_MODEL"], "sonnet[1m]");
    assert_eq!(settings["env"]["CLAUDE_CODE_DISABLE_1M_CONTEXT"], "0");
    assert_eq!(settings["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "1000000");
    assert_eq!(settings["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"], "800000");
}
