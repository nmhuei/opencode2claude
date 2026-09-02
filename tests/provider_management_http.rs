use axum::body::{to_bytes, Body};
use axum::http::Request;
use opencode2api::config::{BridgeConfig, ManagementConfig};
use opencode2api::provider::types::{
    ModelAlias, ModelCandidate, ModelInfo, Provider, ProviderKind, ProviderProtocol,
};
use opencode2api::provider::ProviderRegistry;
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn management_api_exposes_registry_without_secret_values() {
    let mut registry = ProviderRegistry::new();
    registry
        .register_provider(Provider {
            id: "kilo".into(),
            name: "Kilo".into(),
            kind: ProviderKind::Kilo,
            base_url: "http://127.0.0.1:9".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();
    registry.insert_model(ModelInfo {
        provider_id: "kilo".into(),
        model_id: "kilo-auto/free".into(),
        wire_model_id: "kilo-auto/free".into(),
        context_window: Some(1_000_000),
        max_output_tokens: Some(128_000),
        supports_thinking: false,
        verified_context: true,
        free: true,
    });
    registry
        .register_alias(ModelAlias::one_million(
            "free-1m",
            vec![ModelCandidate {
                provider_id: "kilo".into(),
                model_id: "kilo-auto/free".into(),
                credential_id: None,
                priority: 0,
            }],
        ))
        .unwrap();
    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        provider_registry: Some(Arc::new(registry)),
        active_alias: Some("free-1m".into()),
        management: ManagementConfig {
            rest_api_token: Some("rest-secret".into()),
            ..defaults.management
        },
        ..defaults
    };
    let app = build_router(AppState::new(config));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/providers")
                .header("authorization", "Bearer rest-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("kilo"));
    assert!(text.contains("free-1m"));
    assert!(!text.contains("rest-secret"));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/aliases")
                .header("authorization", "Bearer rest-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}
