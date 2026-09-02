use opencode2api::provider::config::load_provider_registry;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_path(name: &str, content: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("opencode2api-{name}-{nonce}.toml"));
    fs::write(&path, content).unwrap();
    path
}

#[test]
fn v3_named_tables_compile_to_provider_registry() {
    let path = fixture_path(
        "provider-v3",
        r#"
schema_version = 3

[providers.bai]
name = "B.AI"
kind = "openai-compatible"
base_url = "https://api.b.ai/v1"
protocol = "openai_chat_completions"

[credentials.bai-main]
provider = "bai"
source = "env:BAI_API_KEY"
auth_scheme = "bearer"

[models.bai."deepseek-v4-flash"]
wire_model = "deepseek-v4-flash"
context_window = 1000000
max_output_tokens = 384000
verified_context = true
supports_streaming = true
supports_tools = true
free = true

[aliases.free-1m]
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.free-1m.candidates]]
provider = "bai"
model = "deepseek-v4-flash"
credential = "bai-main"
priority = 10
"#,
    );
    let registry = load_provider_registry(&path).unwrap();
    assert_eq!(registry.provider("bai").unwrap().name, "B.AI");
    assert_eq!(
        registry
            .model("bai", "deepseek-v4-flash")
            .unwrap()
            .context_window,
        Some(1_000_000)
    );
    assert_eq!(
        registry.alias("free-1m").unwrap().client_model,
        "sonnet[1m]"
    );
    assert_eq!(
        registry.alias("free-1m").unwrap().auto_compact_window(),
        800_000
    );
    let _ = fs::remove_file(path);
}

#[test]
fn strict_v3_alias_rejects_unverified_context_metadata() {
    let path = fixture_path(
        "provider-v3-invalid",
        r#"
schema_version = 3
[providers.bai]
kind = "openai-compatible"
base_url = "https://api.b.ai/v1"
[models.bai.deepseek]
wire_model = "deepseek"
context_window = 1000000
verified_context = false
[aliases.free-1m]
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true
[[aliases.free-1m.candidates]]
provider = "bai"
model = "deepseek"
"#,
    );
    let error = load_provider_registry(&path).unwrap_err();
    assert!(error.to_string().contains("unknown context metadata"));
    let _ = fs::remove_file(path);
}

#[test]
fn every_alias_derives_eighty_percent_compaction_boundary() {
    let path = fixture_path(
        "provider-v3-contexts",
        r#"
schema_version = 3
[providers.local]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9999/v1"
[models.local.small]
wire_model = "small"
context_window = 200000
verified_context = true
[aliases.small]
client_model = "small"
context_window = 200000
[[aliases.small.candidates]]
provider = "local"
model = "small"
"#,
    );
    let registry = load_provider_registry(&path).unwrap();
    assert_eq!(
        registry.alias("small").unwrap().auto_compact_window(),
        160_000
    );
    let _ = fs::remove_file(path);
}
