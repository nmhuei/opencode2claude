//! Bridge error types with proper HTTP response mapping.
//!
//! All errors are converted to Anthropic-compatible JSON error responses
//! so that Claude Code can understand and display them correctly.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Central error type for the OpenCode2Claude bridge.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[allow(dead_code)] // kept for planned use in CLI & daemon startup
    #[error("Failed to bind to address: {0}")]
    BindFailed(#[source] std::io::Error),

    #[allow(dead_code)] // kept for planned use in supervisor
    #[error("Failed to spawn process: {0}")]
    ProcessSpawnFailed(#[source] std::io::Error),

    #[allow(dead_code)] // kept for structured error responses
    #[error("Shell commands are disabled by policy. Set BRIDGE_SHELL_POLICY=allowlist or unrestricted to enable.")]
    ShellDisabled,

    #[error("Shell command '{command}' is not in the allowlist. Allowed: {allowed}")]
    ShellBlocked { command: String, allowed: String },

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Provider capacity exhausted")]
    ProviderCapacityExhausted {
        retry_after: Option<std::time::Duration>,
    },

    #[error("Egress temporarily unavailable: {0}")]
    EgressUnavailable(String),

    #[allow(dead_code)] // kept for health-check error paths
    #[error("OpenCode daemon unavailable on port {0}")]
    DaemonUnavailable(u16),

    #[error("Payment required: {0}")]
    PaymentRequired(String),

    #[error("Upstream API error: {0}")]
    UpstreamError(String),
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match &self {
            BridgeError::BindFailed(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                self.to_string(),
            ),
            BridgeError::ProcessSpawnFailed(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                self.to_string(),
            ),
            BridgeError::ShellDisabled => {
                (StatusCode::FORBIDDEN, "permission_error", self.to_string())
            }
            BridgeError::ShellBlocked { .. } => {
                (StatusCode::FORBIDDEN, "permission_error", self.to_string())
            }
            BridgeError::InvalidRequest(_) => (
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                self.to_string(),
            ),
            BridgeError::Unauthorized(_) => (
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                self.to_string(),
            ),
            BridgeError::Forbidden(_) => {
                (StatusCode::FORBIDDEN, "permission_error", self.to_string())
            }
            BridgeError::RateLimited(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                self.to_string(),
            ),
            BridgeError::ProviderCapacityExhausted { .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                self.to_string(),
            ),
            BridgeError::EgressUnavailable(_) => {
                (StatusCode::BAD_REQUEST, "api_error", self.to_string())
            }
            BridgeError::DaemonUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                self.to_string(),
            ),
            BridgeError::PaymentRequired(_) => (
                StatusCode::PAYMENT_REQUIRED,
                "billing_error",
                self.to_string(),
            ),
            BridgeError::UpstreamError(_) => {
                (StatusCode::BAD_GATEWAY, "api_error", self.to_string())
            }
        };

        let body = if matches!(self, BridgeError::ProviderCapacityExhausted { .. }) {
            json!({
                "type": "error",
                "error": {
                    "type": error_type,
                    "code": "provider_capacity_exhausted",
                    "message": message,
                }
            })
        } else {
            json!({
                "type": "error",
                "error": {
                    "type": error_type,
                    "message": message,
                }
            })
        };
        let retry_after = match &self {
            BridgeError::ProviderCapacityExhausted { retry_after } => *retry_after,
            _ => None,
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(retry_after) = retry_after {
            if let Ok(value) = axum::http::HeaderValue::from_str(
                &retry_after
                    .as_secs()
                    .clamp(1, u64::from(u32::MAX))
                    .to_string(),
            ) {
                response.headers_mut().insert("retry-after", value);
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn test_bridge_error_into_response_unauthorized() {
        let err = BridgeError::Unauthorized("bad token".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_bridge_error_into_response_shell_blocked() {
        let err = BridgeError::ShellBlocked {
            command: "rm".to_string(),
            allowed: "git,ls,pwd".to_string(),
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_bridge_error_into_response_upstream() {
        let err = BridgeError::UpstreamError("timeout".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn test_bridge_error_into_response_invalid_request() {
        let err = BridgeError::InvalidRequest("bad input".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_bridge_error_into_response_payment_required() {
        let err = BridgeError::PaymentRequired("no credits".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn egress_unavailable_is_non_retryable_for_claude_code() {
        let err = BridgeError::EgressUnavailable("proxy recovery running".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn provider_capacity_exhaustion_is_a_retryable_sanitized_429() {
        let err = BridgeError::ProviderCapacityExhausted {
            retry_after: Some(std::time::Duration::from_secs(12)),
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers().get("retry-after").unwrap(), "12");
    }
}
