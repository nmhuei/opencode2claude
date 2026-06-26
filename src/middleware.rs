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
