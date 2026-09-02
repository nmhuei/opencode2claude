use opencode2api::provider::adapters::{AdapterRegistry, FailureClass};
use opencode2api::provider::types::{
    AttemptTarget, AuthScheme, Provider, ProviderKind, ProviderProtocol, ProviderRequest,
};
use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use std::collections::BTreeMap;

fn provider() -> Provider {
    Provider {
        id: "generic".into(),
        name: "Generic".into(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.example.test/v1".into(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        headers: BTreeMap::new(),
        enabled: true,
    }
}

fn target(auth_scheme: AuthScheme) -> AttemptTarget {
    AttemptTarget {
        alias_id: "default".into(),
        client_model: "default".into(),
        provider_id: "generic".into(),
        model_id: "model".into(),
        wire_model_id: "wire-model".into(),
        credential_id: Some("primary".into()),
        auth_scheme,
        context_window: 200_000,
        candidate_index: 0,
    }
}

#[test]
fn generic_adapter_uses_configured_auth_scheme() {
    let adapter = AdapterRegistry::for_provider(ProviderKind::OpenAiCompatible);
    let request = ProviderRequest {
        client_model: "default".into(),
        messages: serde_json::json!([]),
        max_output_tokens: Some(100),
        stream: false,
        body: None,
    };
    let prepared = adapter
        .prepare(
            &provider(),
            &target(AuthScheme::XApiKey),
            &request,
            Some(&"secret".into()),
        )
        .unwrap();
    assert_eq!(prepared.headers.get("x-api-key").unwrap(), "secret");
    assert!(!prepared.headers.contains_key("Authorization"));
}

#[test]
fn generic_adapter_preserves_tool_and_request_fields_when_selecting_wire_model() {
    let adapter = AdapterRegistry::for_provider(ProviderKind::OpenAiCompatible);
    let request = ProviderRequest {
        client_model: "default".into(),
        messages: serde_json::json!([{"role":"user","content":"hello"}]),
        max_output_tokens: Some(100),
        stream: true,
        body: Some(serde_json::json!({
            "model": "client-alias",
            "messages": [{"role":"user","content":"hello"}],
            "tools": [{"type":"function","function":{"name":"lookup"}}],
            "tool_choice": "auto",
            "temperature": 0.2,
            "stream": true,
            "max_tokens": 100
        })),
    };
    let prepared = adapter
        .prepare(&provider(), &target(AuthScheme::None), &request, None)
        .unwrap();
    assert_eq!(prepared.body["model"], "wire-model");
    assert_eq!(prepared.body["tools"][0]["function"]["name"], "lookup");
    assert_eq!(prepared.body["tool_choice"], "auto");
    assert_eq!(prepared.body["temperature"], 0.2);
}

#[test]
fn adapter_failure_classification_is_provider_independent() {
    let adapter = AdapterRegistry::for_provider(ProviderKind::OpenAiCompatible);
    let headers = HeaderMap::new();
    assert_eq!(
        adapter.classify_failure(Some(StatusCode::TOO_MANY_REQUESTS), &headers, ""),
        FailureClass::RateLimit
    );
    assert_eq!(
        adapter.classify_failure(Some(StatusCode::UNAUTHORIZED), &headers, ""),
        FailureClass::CredentialRejected
    );
    assert_eq!(
        adapter.classify_failure(Some(StatusCode::NOT_FOUND), &headers, ""),
        FailureClass::ModelUnavailable
    );
    assert_eq!(
        adapter.classify_failure(Some(StatusCode::BAD_GATEWAY), &headers, ""),
        FailureClass::ProviderServer
    );
}
