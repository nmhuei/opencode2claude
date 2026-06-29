//! Auth middleware for the bridge API.
//!
//! Provides Bearer token authentication for all endpoints except the
//! health check. When `BRIDGE_AUTH_TOKEN` is configured, every request
//! must include a valid `Authorization: Bearer <token>` header.

use crate::error::BridgeError;
use crate::state::AppState;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

/// Auth middleware that validates Bearer tokens from the Authorization header.
///
/// Skips authentication for `/health` so monitoring tools can always reach it.
/// When `config.auth_tokens` is `None` (auth disabled), all requests pass through.
/// When auth is enabled, the middleware extracts the `Authorization: Bearer <token>`
/// header and validates it against `config.is_valid_token()`.
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, BridgeError> {
    if request.uri().path() == "/health" {
        return Ok(next.run(request).await);
    }

    if !state.config.auth_enabled() {
        return Ok(next.run(request).await);
    }

    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");

    if !state.config.is_valid_token(token) {
        return Err(BridgeError::Unauthorized(
            "Missing or invalid authentication token. Provide a valid Bearer token in the Authorization header."
                .to_string(),
        ));
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use crate::config::BridgeConfig;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::routing::post;
    use axum::Router;
    use tower::util::ServiceExt;

    fn make_app(auth_tokens: Option<Vec<String>>) -> Router {
        let config = BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            model: None,
            shell_policy: crate::shell::ShellPolicy::Disabled,
            auth_tokens,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 256,
            tavily_api_key: None,
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 5,
            proxies: None,
            primary_proxies: None,
            auxiliary_proxies: None,
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
}
