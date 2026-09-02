use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{response::Response, Router};
use opencode2api::config::{BridgeConfig, EgressMode};
use opencode2api::provider::types::{
    ModelAlias, ModelCandidate, ModelInfo, Provider, ProviderKind, ProviderProtocol,
};
use opencode2api::provider::ProviderRegistry;
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone)]
struct FixtureState {
    status: StatusCode,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn fixture(
    State(state): State<FixtureState>,
    body: axum::extract::Json<Value>,
) -> Response<Body> {
    state.requests.lock().await.push(body.0);
    if state.status.is_success() {
        Response::builder().status(StatusCode::OK).header("content-type", "application/json").body(Body::from(serde_json::to_vec(&json!({"id":"ok","model":"wire-model","choices":[{"message":{"role":"assistant","content":"fallback-ok"},"finish_reason":"stop"}]})).unwrap())).unwrap()
    } else {
        Response::builder()
            .status(state.status)
            .body(Body::from("temporary failure"))
            .unwrap()
    }
}

async fn server(status: StatusCode) -> (String, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = FixtureState {
        status,
        requests: requests.clone(),
    };
    let app = Router::new()
        .route("/chat/completions", post(fixture))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), requests)
}

#[derive(Clone)]
struct SequenceState {
    statuses: Arc<Mutex<Vec<StatusCode>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn sequence_fixture(
    State(state): State<SequenceState>,
    body: axum::extract::Json<Value>,
) -> Response<Body> {
    state.requests.lock().await.push(body.0);
    let status = state.statuses.lock().await.remove(0);
    if status == StatusCode::OK {
        let body = serde_json::to_vec(&json!({
            "id": "pool-fallback",
            "model": "wire-model",
            "choices": [{"message": {"role": "assistant", "content": "pool-fallback-ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2}
        }))
        .unwrap();
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
    }
    Response::builder()
        .status(status)
        .header("retry-after", "1")
        .body(Body::from("quota exceeded"))
        .unwrap()
}

async fn sequence_server(statuses: Vec<StatusCode>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let state = SequenceState {
        statuses: Arc::new(Mutex::new(statuses)),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let requests = state.requests.clone();
    let app = Router::new()
        .route("/chat/completions", post(sequence_fixture))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), requests)
}

async fn run_two_provider_fallback(
    first_status: StatusCode,
) -> (StatusCode, Vec<Value>, Vec<Value>) {
    let (first_url, first_requests) = server(first_status).await;
    let (second_url, second_requests) = server(StatusCode::OK).await;
    let mut registry = ProviderRegistry::new();
    for (id, url) in [("deepseek", first_url), ("glm", second_url)] {
        registry
            .register_provider(Provider {
                id: id.into(),
                name: id.into(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: url,
                protocol: ProviderProtocol::OpenAiChatCompletions,
                headers: BTreeMap::new(),
                enabled: true,
            })
            .unwrap();
        registry.insert_model(ModelInfo {
            provider_id: id.into(),
            model_id: format!("{id}-1m"),
            wire_model_id: format!("wire-{id}"),
            context_window: Some(1_000_000),
            max_output_tokens: Some(128_000),
            supports_thinking: true,
            verified_context: true,
            free: true,
        });
    }
    registry
        .register_alias(ModelAlias::one_million(
            "free-1m",
            vec![
                ModelCandidate {
                    provider_id: "deepseek".into(),
                    model_id: "deepseek-1m".into(),
                    credential_id: None,
                    credential_pool_id: None,
                    priority: 0,
                },
                ModelCandidate {
                    provider_id: "glm".into(),
                    model_id: "glm-1m".into(),
                    credential_id: None,
                    credential_pool_id: None,
                    priority: 1,
                },
            ],
        ))
        .unwrap();
    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        active_alias: Some("free-1m".into()),
        provider_registry: Some(Arc::new(registry)),
        model: Some("sonnet[1m]".into()),
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        retry: opencode2api::config::RetryConfig {
            max_network_attempts: 0,
            ..defaults.retry
        },
        ..defaults
    };
    let app = build_router(AppState::new(config));
    let response = app.oneshot(Request::builder().method("POST").uri("/v1/messages").header("content-type", "application/json").body(Body::from(serde_json::to_vec(&json!({"model":"sonnet[1m]","messages":[{"role":"user","content":"large context"}],"max_tokens":128})).unwrap())).unwrap()).await.unwrap();
    let first_requests = first_requests.lock().await.clone();
    let second_requests = second_requests.lock().await.clone();
    (response.status(), first_requests, second_requests)
}

#[tokio::test]
async fn rate_limited_one_million_alias_falls_across_providers_without_downgrading_context() {
    let (status, first_requests, second_requests) =
        run_two_provider_fallback(StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_requests[0]["model"], "wire-deepseek");
    assert_eq!(second_requests[0]["model"], "wire-glm");
}

#[tokio::test]
async fn billing_failure_falls_to_the_next_provider() {
    let (status, first_requests, second_requests) =
        run_two_provider_fallback(StatusCode::PAYMENT_REQUIRED).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_requests[0]["model"], "wire-deepseek");
    assert_eq!(second_requests[0]["model"], "wire-glm");
}

#[tokio::test]
async fn pool_member_429_is_quarantined_before_same_alias_fallback() {
    let (upstream_url, requests) =
        sequence_server(vec![StatusCode::TOO_MANY_REQUESTS, StatusCode::OK]).await;
    let mut registry = ProviderRegistry::new();
    registry
        .register_provider(Provider {
            id: "bai".into(),
            name: "B.AI".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: upstream_url,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();
    for id in ["key-a", "key-b"] {
        registry
            .register_credential(opencode2api::provider::types::Credential {
                id: id.into(),
                provider_id: "bai".into(),
                source: opencode2api::provider::types::SecretSource::Env {
                    variable: format!("POOL_{id}"),
                },
                auth_scheme: opencode2api::provider::types::AuthScheme::None,
            })
            .unwrap();
    }
    registry.insert_model(ModelInfo {
        provider_id: "bai".into(),
        model_id: "deepseek-1m".into(),
        wire_model_id: "wire-model".into(),
        context_window: Some(1_000_000),
        max_output_tokens: Some(128_000),
        supports_thinking: false,
        verified_context: true,
        free: true,
    });
    registry
        .upsert_pool(
            opencode2api::provider::types::CredentialPool::new(
                "free-1m",
                "bai",
                opencode2api::provider::types::PoolStrategy::RoundRobin,
                ["key-a", "key-b"]
                    .into_iter()
                    .map(|id| {
                        opencode2api::provider::types::CredentialPoolMember::new(
                            id,
                            format!("account-{id}"),
                            1,
                            None,
                            None,
                            1,
                        )
                        .unwrap()
                    })
                    .collect(),
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_alias(ModelAlias::one_million(
            "free-1m",
            vec![ModelCandidate {
                provider_id: "bai".into(),
                model_id: "deepseek-1m".into(),
                credential_id: None,
                credential_pool_id: Some("free-1m".into()),
                priority: 0,
            }],
        ))
        .unwrap();
    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        active_alias: Some("free-1m".into()),
        model: Some("sonnet[1m]".into()),
        provider_registry: Some(Arc::new(registry)),
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        retry: opencode2api::config::RetryConfig {
            max_network_attempts: 0,
            ..defaults.retry
        },
        ..defaults
    };
    let response = build_router(AppState::new(config))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "sonnet[1m]",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 128
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let request_log = requests.lock().await.clone();
    assert_eq!(request_log.len(), 2);
    assert_eq!(request_log[0]["model"], "wire-model");
    assert_eq!(request_log[1]["model"], "wire-model");
}
