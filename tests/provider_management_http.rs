use axum::body::{to_bytes, Body};
use axum::http::Request;
use axum::routing::get;
use axum::{response::Response, Router};
use opencode2api::config::{BridgeConfig, ManagementConfig};
use opencode2api::provider::types::{
    ModelAlias, ModelCandidate, ModelInfo, Provider, ProviderKind, ProviderProtocol,
};
use opencode2api::provider::ProviderRegistry;
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::net::TcpListener;
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
    let app = build_router(AppState::new(config.clone()));
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
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/provider-runtime")
                .header("authorization", "Bearer rest-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let runtime_body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let runtime_text = String::from_utf8(runtime_body.to_vec()).unwrap();
    assert!(runtime_text.contains("\"active_alias\":\"free-1m\""));
    assert!(runtime_text.contains("\"provider_count\":1"));
    assert!(runtime_text.contains("\"cooling_down\":0"));
    assert!(!runtime_text.contains("rest-secret"));

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

#[tokio::test]
async fn provider_health_classifies_rate_limit_and_retry_after() {
    let upstream = Router::new().route(
        "/models",
        get(|| async {
            Response::builder()
                .status(429)
                .header("retry-after", "5")
                .body(Body::from("quota exceeded"))
                .unwrap()
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let mut registry = ProviderRegistry::new();
    registry
        .register_provider(Provider {
            id: "rate-limited".into(),
            name: "Rate limited fixture".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("http://{address}"),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();
    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        provider_registry: Some(Arc::new(registry)),
        management: ManagementConfig {
            rest_api_token: Some("rest-secret".into()),
            ..defaults.management
        },
        ..defaults
    };
    let response = build_router(AppState::new(config))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/providers/rate-limited/health")
                .header("authorization", "Bearer rest-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["healthy"], false);
    assert_eq!(body["state"], "RateLimit");
    assert_eq!(body["failure_class"], "RateLimit");
    assert_eq!(body["retry_after_ms"], 5000);
}
