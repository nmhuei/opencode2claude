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
    /// A credential-bearing header was presented, even if its value carried
    /// no usable token (empty or whitespace-only). Presentation without a
    /// match must never degrade into anonymous admission.
    credential_presented: bool,
}

fn request_auth_input(request: &Request) -> RequestAuthInput {
    // RFC 7235: the authentication scheme is case-insensitive. This mirrors
    // `management::auth::bearer_token` so LLM routes and management routes
    // parse the same credential header identically.
    let bearer = request
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| token.to_string())
        });
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    // An empty header value carries no usable credential candidate; keeping
    // it would let byte-for-byte comparison against an empty configured
    // secret admit the request. The presentation itself is still recorded so
    // a presented-but-empty credential can never degrade into anonymous
    // admission downstream.
    let credential_presented = bearer.is_some() || api_key.is_some();
    RequestAuthInput {
        path: request.uri().path().to_string(),
        tokens: [bearer, api_key]
            .into_iter()
            .flatten()
            // Mirrors the non-empty filter each branch previously applied.
            .filter(|token| !token.is_empty())
            .collect(),
        credential_presented,
    }
}

async fn resolve_authenticated_client(
    state: &AppState,
    mut auth: RequestAuthInput,
) -> Result<Option<ApiKeyAdmission>, ApiKeyAuthError> {
    // A presented credential is meaningful even when its value is empty or
    // fails every match: presentation must never degrade into anonymous
    // admission. Recorded before the empty-candidate drop below, and also
    // derived from the candidate list so directly constructed inputs stay
    // consistent even if they bypass header extraction.
    let credential_presented = auth.credential_presented || !auth.tokens.is_empty();
    // An empty string is not a credential. Drop empty candidates before any
    // matching so neither the integration-key comparison nor the registry
    // lookup can be satisfied by byte-for-byte equality against an empty
    // configured or imported secret.
    auth.tokens.retain(|token| !token.is_empty());
    // Claude Code uses a bridge-owned integration credential that is separate
    // from dashboard-managed application keys. The dashboard may freely add,
    // rotate, disable, or revoke application credentials without invalidating
    // the long-lived local Claude Code connection.
    let claude_code_key = crate::application::integration::api_key(&state.config);
    // The compile-time fallback constant carries no configured authority: it
    // may only authenticate on a loopback bind while no application credential
    // exists at all (no auth_tokens, unconfigured registry). A genuinely
    // configured legacy token (`auth_tokens[0]`) keeps its historical
    // behavior regardless of registry state.
    let fallback_constant = claude_code_key == crate::application::integration::FALLBACK_API_KEY;
    // An empty integration credential is never authoritative: token_eq(b"",
    // b"") matches any empty presented candidate, which would otherwise
    // admit unauthenticated requests as system_claude_code regardless of
    // bind host or registry state. Directly constructed configs (in-process
    // embedding) can bypass the loader's empty-token filtering, so the
    // enforcement point must not depend on it.
    let integration_authoritative = !claude_code_key.is_empty();
    let mut fallback_denied = false;
    if integration_authoritative
        && auth
            .tokens
            .iter()
            .any(|token| token_eq(token.as_bytes(), claude_code_key.as_bytes()))
    {
        let admissible = !fallback_constant || {
            let registry_configured = state.api_keys.read().await.configured();
            !registry_configured && state.config.host.is_loopback()
        };
        if admissible {
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
        // Fall through to normal registry matching so a second, genuinely
        // valid credential presented in the same request can still
        // authenticate.
        fallback_denied = true;
    }

    let configured = state.api_keys.read().await.configured();
    if !configured {
        if fallback_denied {
            // Fail closed: the caller presented only the non-authoritative
            // fallback constant where it must never substitute.
            return Err(ApiKeyAuthError::Invalid);
        }
        // Fail closed whenever a credential was presented but matched
        // nothing — including empty or whitespace-only values — regardless
        // of bind host. Anonymous admission is only correct when no
        // credential header was presented at all; this stays independent of
        // which values the registry happens to turn into records.
        if credential_presented {
            return Err(ApiKeyAuthError::Invalid);
        }
        // Fail closed on public binds: an unconfigured registry means no
        // enforceable LLM-route credential exists (management-only tokens
        // such as REST_API_TOKEN never reach this registry). Anonymous
        // admission stays a loopback-only convenience; startup validation
        // normally rejects this shape, but the enforcement point must not
        // depend on every construction path having validated first.
        if !state.config.host.is_loopback() {
            return Err(ApiKeyAuthError::Invalid);
        }
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
    async fn test_auth_middleware_accepts_case_insensitive_bearer_scheme() {
        let app = make_app(Some(vec!["secret".to_string()]));
        for scheme in ["bearer", "BEARER", "BeArEr"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/messages")
                        .header("Authorization", format!("{scheme} secret"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "scheme `{scheme}` must authenticate");
        }
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

    fn integration_state(host: &str, auth_tokens: Option<Vec<String>>) -> AppState {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut config = BridgeConfig {
            host: host.parse().unwrap(),
            auth_tokens: auth_tokens.map(|tokens| tokens.into_iter().map(Into::into).collect()),
            model: Some("opencode/deepseek-v4-flash-free".to_string()),
            ..Default::default()
        };
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-middleware-{}-{}-{sequence}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        AppState::new(config)
    }

    async fn resolve(
        state: &AppState,
        tokens: Vec<&str>,
    ) -> Result<Option<crate::api_key::ApiKeyAdmission>, crate::api_key::ApiKeyAuthError> {
        // Each entry models one presented credential header; an empty entry
        // models a presented header with an empty value.
        let credential_presented = !tokens.is_empty();
        super::resolve_authenticated_client(
            state,
            super::RequestAuthInput {
                path: "/v1/messages".to_string(),
                tokens: tokens.into_iter().map(String::from).collect(),
                credential_presented,
            },
        )
        .await
    }

    #[tokio::test]
    async fn fallback_constant_rejected_once_registry_is_configured() {
        let state = integration_state("127.0.0.1", None);

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

        // Managed keys exist: the compile-time fallback constant must never
        // substitute for them and the request must fail closed.
        let fallback = resolve(&state, vec!["opencode-bridge"]).await;
        assert!(matches!(
            fallback,
            Err(crate::api_key::ApiKeyAuthError::Invalid)
        ));

        // The genuinely issued managed credential still authenticates.
        let application = resolve(&state, vec![&managed_secret])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(application.client.key_id, managed.id);

        // Revocation does not resurrect the fallback constant: the registry
        // remains configured (revoked keys are retained by design).
        state.api_keys.write().await.revoke(&managed.id).unwrap();
        let after_revoke = resolve(&state, vec!["opencode-bridge"]).await;
        assert!(matches!(
            after_revoke,
            Err(crate::api_key::ApiKeyAuthError::Invalid)
        ));
    }

    #[tokio::test]
    async fn fallback_constant_admitted_only_on_unconfigured_loopback_bind() {
        let state = integration_state("127.0.0.1", None);
        assert!(!state.api_keys.read().await.configured());

        let claude = resolve(&state, vec!["opencode-bridge"])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claude.client.key_id, "system_claude_code");
        assert_eq!(
            claude
                .client
                .policy
                .resolve_model(
                    Some("claude-sonnet-4-6"),
                    state.config.model.as_deref(),
                    crate::config::DEFAULT_MODEL,
                )
                .unwrap(),
            "opencode/deepseek-v4-flash-free"
        );
    }

    #[tokio::test]
    async fn fallback_constant_fails_closed_on_non_loopback_bind() {
        let state = integration_state("192.168.1.50", None);
        assert!(!state.api_keys.read().await.configured());

        let result = resolve(&state, vec!["opencode-bridge"]).await;
        assert!(matches!(
            result,
            Err(crate::api_key::ApiKeyAuthError::Invalid)
        ));
    }

    #[tokio::test]
    async fn anonymous_requests_fail_closed_on_public_bind_with_empty_registry() {
        // No BRIDGE_AUTH_TOKEN, no registry entries: on a non-loopback bind
        // there is no enforceable LLM-route credential at all, so the
        // resolver must never fall back to anonymous admission.
        let state = integration_state("192.168.1.50", None);
        assert!(!state.api_keys.read().await.configured());
        assert!(!state.config.auth_enabled());

        let result = resolve(&state, vec![]).await;
        assert!(
            matches!(result, Err(crate::api_key::ApiKeyAuthError::Invalid)),
            "anonymous requests must fail closed off-loopback, got {result:?}"
        );

        // A wrong credential is equally rejected.
        let wrong = resolve(&state, vec!["not-a-key"]).await;
        assert!(matches!(
            wrong,
            Err(crate::api_key::ApiKeyAuthError::Invalid)
        ));

        // The loopback equivalent stays an allowed anonymous convenience.
        let local = integration_state("127.0.0.1", None);
        let anonymous = resolve(&local, vec![]).await;
        assert!(
            matches!(anonymous, Ok(None)),
            "loopback + empty registry must keep admitting anonymously"
        );
    }

    #[tokio::test]
    async fn empty_configured_integration_credential_is_never_authoritative() {
        // Directly constructed configs (in-process embedding) bypass the
        // loader's empty-token filtering because SecretString's From impls
        // do not reject empties. An empty integration credential must never
        // be authoritative: byte-for-byte equality against an empty
        // presented candidate would otherwise admit unauthenticated
        // requests as system_claude_code on any bind host.
        let public = integration_state("192.168.1.50", Some(vec!["".to_string()]));
        let result = resolve(&public, vec![""]).await;
        assert!(
            matches!(result, Err(crate::api_key::ApiKeyAuthError::Invalid)),
            "empty credential must not claim the integration identity off-loopback, got {result:?}"
        );

        let local = integration_state("127.0.0.1", Some(vec!["".to_string()]));
        let loopback_result = resolve(&local, vec![""]).await;
        assert!(
            matches!(loopback_result, Err(crate::api_key::ApiKeyAuthError::Invalid)),
            "empty credential must not claim the integration identity on loopback either, got {loopback_result:?}"
        );
    }

    #[tokio::test]
    async fn empty_api_key_header_is_rejected_end_to_end() {
        // The full HTTP stack must reject an empty x-api-key credential even
        // when a directly constructed config carries an empty first auth
        // token: the request must fail closed instead of being admitted as
        // the Claude Code integration identity.
        let mut config = BridgeConfig {
            host: "192.168.1.50".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            auth_tokens: Some(vec!["".to_string()].into_iter().map(Into::into).collect()),
            max_body_size: 1024,
            ..Default::default()
        };
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-middleware-empty-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);
        let app = Router::new()
            .route("/v1/messages", post(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::auth_middleware,
            ))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "empty credential must fail closed");
    }

    #[tokio::test]
    async fn wrong_credential_on_unconfigured_loopback_fails_closed() {
        // Presentation without a match must never degrade into anonymous
        // admission — even on a loopback bind where genuinely anonymous
        // requests (no credential header at all) stay admitted.
        let state = integration_state("127.0.0.1", None);
        assert!(!state.api_keys.read().await.configured());
        assert!(!state.config.auth_enabled());

        let result = resolve(&state, vec!["not-a-key"]).await;
        assert!(
            matches!(result, Err(crate::api_key::ApiKeyAuthError::Invalid)),
            "presented-but-nonmatching credential must fail closed on loopback too, got {result:?}"
        );

        // The no-header case keeps its loopback anonymous convenience.
        let anonymous = resolve(&state, vec![]).await;
        assert!(
            matches!(anonymous, Ok(None)),
            "no-header requests must keep loopback anonymous admission, got {anonymous:?}"
        );
    }

    #[tokio::test]
    async fn empty_credential_header_on_unconfigured_loopback_fails_closed_end_to_end() {
        // Through the full HTTP stack on a loopback bind: an empty x-api-key
        // header IS a presented credential, so the request must fail closed
        // instead of being admitted anonymously.
        let mut config = BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            max_body_size: 1024,
            ..Default::default()
        };
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-middleware-loopback-empty-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);
        let app = Router::new()
            .route("/v1/messages", post(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::auth_middleware,
            ))
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            401,
            "presented-but-empty credential must fail closed even on loopback"
        );
    }

    #[tokio::test]
    async fn legacy_first_auth_token_remains_integration_key_alongside_managed_keys() {
        let state = integration_state("127.0.0.1", Some(vec!["legacy-secret".to_string()]));

        // Registry also holds a managed key: the legacy token must still act
        // as the Claude Code integration credential.
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
        assert!(state.api_keys.read().await.configured());

        let claude = resolve(&state, vec!["legacy-secret"])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claude.client.key_id, "system_claude_code");

        // A non-integration token that is not in auth_tokens falls through to
        // normal registry matching.
        let application = resolve(&state, vec![&managed_secret])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(application.client.key_id, managed.id);
    }
}
