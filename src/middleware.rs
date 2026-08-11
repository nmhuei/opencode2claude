//! Client authentication and per-key admission control.
//!
//! Both Anthropic and OpenAI compatible routes use the hot-reloadable API-key
//! registry. Legacy `auth_tokens` values are imported into that registry at
//! startup, so existing deployments keep working while newly managed keys can
//! be created, disabled, rotated, and rate-limited without restarting.

use crate::api_key::{ApiKeyAdmission, ApiKeyAuthError, ApiKeyPermissions, ApiKeyPolicy};
use crate::error::BridgeError;
use crate::management::auth::token_eq;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use futures_util::StreamExt;

/// Anthropic-compatible authentication for `/v1/messages`, token counting,
/// and model discovery routes.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, BridgeError> {
    if request.uri().path() == "/health" {
        return Ok(next.run(request).await);
    }

    let auth = request_auth_input(&request);
    match resolve_authenticated_client(&state, auth).await {
        Ok(Some(admission)) => {
            request.extensions_mut().insert(admission.client.clone());
            Ok(hold_admission(next.run(request).await, admission))
        }
        Ok(None) => Ok(next.run(request).await),
        Err(error) => Err(auth_error_to_bridge(error)),
    }
}

/// OpenAI-compatible authentication for `/v1/chat/completions`.
pub async fn openai_auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth = request_auth_input(&request);
    match resolve_authenticated_client(&state, auth).await {
        Ok(Some(admission)) => {
            request.extensions_mut().insert(admission.client.clone());
            hold_admission(next.run(request).await, admission)
        }
        Ok(None) => next.run(request).await,
        Err(error) => openai_auth_error(error),
    }
}

fn hold_admission(response: Response, admission: ApiKeyAdmission) -> Response {
    let (parts, body) = response.into_parts();
    let stream = async_stream::stream! {
        let _admission = admission;
        let mut body = body.into_data_stream();
        while let Some(chunk) = body.next().await {
            yield chunk;
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[derive(Debug)]
struct RequestAuthInput {
    path: String,
    tokens: Vec<String>,
}

fn request_auth_input(request: &Request) -> RequestAuthInput {
    let bearer = request
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(ToOwned::to_owned);
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    RequestAuthInput {
        path: request.uri().path().to_string(),
        tokens: [bearer, api_key].into_iter().flatten().collect(),
    }
}

async fn resolve_authenticated_client(
    state: &AppState,
    auth: RequestAuthInput,
) -> Result<Option<ApiKeyAdmission>, ApiKeyAuthError> {
    // Claude Code uses a bridge-owned integration credential that is separate
    // from dashboard-managed application keys. The dashboard may freely add,
    // rotate, disable, or revoke application credentials without invalidating
    // the long-lived local Claude Code connection.
    let claude_code_key = crate::application::integration::api_key(&state.config);
    if auth
        .tokens
        .iter()
        .any(|token| token_eq(token.as_bytes(), claude_code_key.as_bytes()))
    {
        let policy = ApiKeyPolicy {
            default_model: state.config.model.clone(),
            allow_model_override: false,
            permissions: ApiKeyPermissions {
                shell: state.config.shell_policy.kind() != "disabled",
                ..Default::default()
            },
            ..Default::default()
        };
        if !policy.endpoint_allowed(&auth.path) {
            return Err(ApiKeyAuthError::EndpointDenied(auth.path));
        }
        return Ok(Some(ApiKeyAdmission::claude_code(policy)));
    }

    let configured = state.api_keys.read().await.configured();
    if !configured {
        return Ok(None);
    }

    let mut last_error = ApiKeyAuthError::Invalid;
    for token in auth.tokens {
        let matched = {
            let registry = state.api_keys.read().await;
            registry.match_secret(&token, &auth.path)
        };
        match matched {
            Ok(matched) => return matched.admit().await.map(Some),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn auth_error_to_bridge(error: ApiKeyAuthError) -> BridgeError {
    match error {
        ApiKeyAuthError::Invalid => BridgeError::Unauthorized(
            "Missing or invalid authentication token. Provide a valid x-api-key or Authorization: Bearer token."
                .to_string(),
        ),
        ApiKeyAuthError::ConcurrentLimit
        | ApiKeyAuthError::RequestsPerMinute
        | ApiKeyAuthError::DailyQuota => BridgeError::RateLimited(error.to_string()),
        other => BridgeError::Forbidden(other.to_string()),
    }
}

fn openai_auth_error(error: ApiKeyAuthError) -> Response {
    match error {
        ApiKeyAuthError::Invalid => crate::handlers::openai_error_response(
            axum::http::StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            Some("invalid_api_key"),
            "Incorrect API key provided",
        ),
        ApiKeyAuthError::ConcurrentLimit
        | ApiKeyAuthError::RequestsPerMinute
        | ApiKeyAuthError::DailyQuota => crate::handlers::openai_error_response(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            Some("rate_limit_exceeded"),
            error.to_string(),
        ),
        other => crate::handlers::openai_error_response(
            axum::http::StatusCode::FORBIDDEN,
            "permission_error",
            Some("key_policy_denied"),
            other.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::BridgeConfig;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use axum::Router;
    use tower::util::ServiceExt;

    fn make_app(auth_tokens: Option<Vec<String>>) -> Router {
        let config = BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            auth_tokens: auth_tokens.map(|tokens| tokens.into_iter().map(Into::into).collect()),
            max_body_size: 1024,
            ..Default::default()
        };
        let state = AppState::new(config);

        Router::new()
            .route("/v1/messages", post(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::auth_middleware,
            ))
            .route("/health", get(|| async { "ok" }))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_auth_middleware_skips_health() {
        let app = make_app(Some(vec!["secret".to_string()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_auth_middleware_passes_valid_token() {
        let app = make_app(Some(vec!["secret".to_string()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_auth_middleware_passes_valid_anthropic_api_key() {
        let app = make_app(Some(vec!["secret".to_string()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_valid_anthropic_api_key_is_not_shadowed_by_invalid_bearer() {
        let app = make_app(Some(vec!["secret".to_string()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("Authorization", "Bearer wrong")
                    .header("x-api-key", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_auth_middleware_rejects_invalid() {
        let app = make_app(Some(vec!["secret".to_string()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("Authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn test_auth_middleware_no_auth_configured() {
        let app = make_app(None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn claude_code_key_survives_managed_key_lifecycle() {
        let mut config = BridgeConfig {
            auth_tokens: None,
            model: Some("opencode/deepseek-v4-flash-free".to_string()),
            ..Default::default()
        };
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-middleware-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp()
        ));
        let state = AppState::new(config);

        let (managed, managed_secret) = state
            .api_keys
            .write()
            .await
            .create(
                "Application key".to_string(),
                None,
                "production".to_string(),
                None,
                crate::api_key::ApiKeyPolicy::default(),
                32,
            )
            .unwrap();

        let claude = super::resolve_authenticated_client(
            &state,
            super::RequestAuthInput {
                path: "/v1/messages".to_string(),
                tokens: vec!["opencode-bridge".to_string()],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claude.client.key_id, "system_claude_code");
        for requested_model in [
            "claude-sonnet-4-6",
            "claude-sonnet-5",
            "claude-fable-5",
            "claude-opus-4-8",
            "claude-opus-5",
        ] {
            assert_eq!(
                claude
                    .client
                    .policy
                    .resolve_model(
                        Some(requested_model),
                        state.config.model.as_deref(),
                        crate::config::DEFAULT_MODEL,
                    )
                    .unwrap(),
                "opencode/deepseek-v4-flash-free"
            );
        }

        let application = super::resolve_authenticated_client(
            &state,
            super::RequestAuthInput {
                path: "/v1/messages".to_string(),
                tokens: vec![managed_secret],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(application.client.key_id, managed.id);

        state.api_keys.write().await.revoke(&managed.id).unwrap();

        let claude_after_revoke = super::resolve_authenticated_client(
            &state,
            super::RequestAuthInput {
                path: "/v1/messages".to_string(),
                tokens: vec!["opencode-bridge".to_string()],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claude_after_revoke.client.key_id, "system_claude_code");
    }
}
