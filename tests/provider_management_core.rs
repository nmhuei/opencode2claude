use opencode2api::provider::adapters::AdapterRegistry;
use opencode2api::provider::catalog::{CatalogSource, ModelCatalog};
use opencode2api::provider::config::ProviderFileConfig;
use opencode2api::provider::routing::RoutePlanner;
use opencode2api::provider::types::{AuthScheme, ModelInfo, ProviderRequest};
use opencode2api::provider::ProviderRuntimeHandle;

const CONFIG: &str = r#"
schema_version = 2

[[providers]]
id = "bai"
name = "B.AI"
kind = "bai"
base_url = "https://api.b.ai/v1"

[[providers]]
id = "kilo"
name = "Kilo"
kind = "kilo"
base_url = "https://api.kilo.ai/api/gateway"

[[credentials]]
id = "bai-main"
provider_id = "bai"
env = "BAI_API_KEY"
auth_scheme = "x-api-key"

[[models]]
provider_id = "bai"
model_id = "deepseek-1m"
context_window = 1000000
verified_context = true

[[models]]
provider_id = "kilo"
model_id = "small-128k"
context_window = 128000
verified_context = true

[[aliases]]
id = "free-1m"
client_model = "claude-sonnet-5[1m]"
context_window = 1000000
strict_context = true

[[aliases.candidates]]
provider_id = "bai"
model_id = "deepseek-1m"
credential_id = "bai-main"
priority = 0
"#;

#[test]
fn schema_v2_keeps_provider_scoped_models_and_strict_aliases() {
    let registry: ProviderFileConfig = toml::from_str(CONFIG).unwrap();
    let registry = registry.into_registry().unwrap();
    assert_eq!(
        registry.provider("bai").unwrap().base_url,
        "https://api.b.ai/v1"
    );
    assert_eq!(
        registry.model("kilo", "small-128k").unwrap().context_window,
        Some(128000)
    );
    let target = registry.resolve_alias("free-1m").unwrap();
    assert_eq!(target[0].wire_model_id, "deepseek-1m");
    assert_eq!(target[0].client_model, "claude-sonnet-5[1m]");
}

#[test]
fn catalog_rejects_unknown_and_sub_million_models_for_one_m_alias() {
    let mut catalog = ModelCatalog::new();
    catalog.refresh(
        [
            ModelInfo {
                provider_id: "bai".into(),
                model_id: "deepseek".into(),
                wire_model_id: "deepseek".into(),
                context_window: Some(1_000_000),
                max_output_tokens: None,
                supports_thinking: true,
                verified_context: true,
                free: true,
            },
            ModelInfo {
                provider_id: "kilo".into(),
                model_id: "small".into(),
                wire_model_id: "small".into(),
                context_window: Some(128_000),
                max_output_tokens: None,
                supports_thinking: false,
                verified_context: true,
                free: true,
            },
            ModelInfo {
                provider_id: "bai".into(),
                model_id: "unknown".into(),
                wire_model_id: "unknown".into(),
                context_window: None,
                max_output_tokens: None,
                supports_thinking: false,
                verified_context: false,
                free: true,
            },
        ],
        CatalogSource::Configured,
    );
    let eligible = catalog.eligible_for_alias(1_000_000);
    assert_eq!(eligible.len(), 1);
    assert_eq!(eligible[0].model_id, "deepseek");
}

#[test]
fn bai_adapter_uses_x_api_key_and_wire_model_id() {
    let registry: ProviderFileConfig = toml::from_str(CONFIG).unwrap();
    let registry = registry.into_registry().unwrap();
    let provider = registry.provider("bai").unwrap();
    let target = registry.resolve_alias("free-1m").unwrap().remove(0);
    let credential = registry.credentials().next().unwrap();
    let adapter = AdapterRegistry::for_provider(provider.kind);
    let request = ProviderRequest {
        client_model: target.client_model.clone(),
        messages: serde_json::json!([]),
        max_output_tokens: Some(1024),
        stream: true,
    };
    let prepared = adapter
        .prepare(provider, &target, &request, Some(&"secret".into()))
        .unwrap();
    assert_eq!(prepared.url, "https://api.b.ai/v1/chat/completions");
    assert_eq!(prepared.headers.get("x-api-key").unwrap(), "secret");
    assert_eq!(prepared.body["model"], "deepseek-1m");
    assert_eq!(credential.auth_scheme, AuthScheme::XApiKey);
}

#[test]
fn route_planner_keeps_fallback_order_and_context() {
    let registry = toml::from_str::<ProviderFileConfig>(CONFIG)
        .unwrap()
        .into_registry()
        .unwrap();
    let request = ProviderRequest {
        client_model: "claude-sonnet-5[1m]".into(),
        messages: serde_json::json!([]),
        max_output_tokens: Some(128000),
        stream: false,
    };
    let targets = RoutePlanner::new(&registry)
        .plan(&request, "free-1m")
        .unwrap();
    assert_eq!(targets.len(), 1);
    assert!(targets
        .iter()
        .all(|target| target.context_window >= 1_000_000));
}

#[test]
fn runtime_handle_replaces_only_after_snapshot_validation() {
    let first = toml::from_str::<ProviderFileConfig>(CONFIG)
        .unwrap()
        .into_registry()
        .unwrap();
    let handle = ProviderRuntimeHandle::load(&first).unwrap();
    assert!(handle
        .snapshot()
        .aliases
        .keys()
        .any(|id| id.as_ref() == "free-1m"));

    let renamed = CONFIG.replace("free-1m", "free-1m-v2");
    let second = toml::from_str::<ProviderFileConfig>(&renamed)
        .unwrap()
        .into_registry()
        .unwrap();
    handle.replace(&second).unwrap();
    assert!(handle
        .snapshot()
        .aliases
        .keys()
        .any(|id| id.as_ref() == "free-1m-v2"));
}
