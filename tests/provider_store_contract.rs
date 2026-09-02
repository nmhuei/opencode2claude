use opencode2api::provider::store::{ProviderConfigStore, ProviderMutation};
use opencode2api::provider::types::{Provider, ProviderKind, ProviderProtocol};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_path(name: &str, extension: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("opencode2api-{name}-{nonce}.{extension}"))
}

fn v2_document() -> &'static str {
    r#"schema_version = 2

[[providers]]
id = "bai"
name = "B.AI"
kind = "openai-compatible"
base_url = "https://api.b.ai/v1"

[[models]]
provider_id = "bai"
model_id = "deepseek"
context_window = 1000000
verified_context = true

[[aliases]]
id = "free-1m"
client_model = "sonnet[1m]"
context_window = 1000000
strict_context = true

[[aliases.candidates]]
provider_id = "bai"
model_id = "deepseek"
priority = 10
"#
}

#[test]
fn invalid_mutation_keeps_original_document_bytes() {
    let path = fixture_path("provider-store-invalid", "toml");
    fs::write(&path, v2_document()).unwrap();
    let before = fs::read(&path).unwrap();
    let store = ProviderConfigStore::open(&path);
    let result = store.transaction(ProviderMutation::RemoveProvider("bai".into()));
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let _ = fs::remove_file(path);
}

#[test]
fn migration_writes_v3_and_preserves_an_adjacent_backup() {
    let path = fixture_path("provider-store-migrate", "toml");
    fs::write(&path, v2_document()).unwrap();
    let store = ProviderConfigStore::open(&path);
    let report = store.migrate_to_v3().unwrap();
    assert_eq!(report.from_version, 2);
    assert_eq!(report.to_version, 3);
    assert!(report.backup_path.is_file());
    assert_eq!(
        fs::read(&report.backup_path).unwrap(),
        v2_document().as_bytes()
    );
    let migrated = fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("schema_version = 3"));
    assert!(migrated.contains("[providers.bai]"));
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(report.backup_path);
}

#[test]
fn valid_mutation_writes_a_generic_provider_without_adapter_code() {
    let path = fixture_path("provider-store-add", "toml");
    let store = ProviderConfigStore::open(&path);
    let provider = Provider {
        id: "moonshot".into(),
        name: "Moonshot".into(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.moonshot.example/v1".into(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        headers: BTreeMap::new(),
        enabled: true,
    };
    store
        .transaction(ProviderMutation::AddProvider(provider))
        .unwrap();
    let reloaded = store.load().unwrap();
    assert_eq!(reloaded.provider("moonshot").unwrap().name, "Moonshot");
    assert!(fs::read_to_string(&path)
        .unwrap()
        .contains("schema_version = 3"));
    let _ = fs::remove_file(path);
}

#[test]
fn provider_mutation_preserves_unrelated_bridge_configuration() {
    let path = fixture_path("provider-store-preserve", "toml");
    fs::write(
        &path,
        "schema_version = 1\nport = 4567\nmodel = \"legacy-model\"\nupstream_base_url = \"https://legacy.example/v1\"\n",
    )
    .unwrap();
    let store = ProviderConfigStore::open(&path);
    store
        .transaction(ProviderMutation::AddProvider(Provider {
            id: "new-api".into(),
            name: "New API".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example/v1".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        }))
        .unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("port = 4567"));
    assert!(text.contains("model = \"legacy-model\""));
    assert!(text.contains("upstream_base_url = \"https://legacy.example/v1\""));
    let _ = fs::remove_file(path);
}
