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
    #[error("Shell commands are disabled by policy. Set BRIDGE_SHELL_POLICY=allowlist or unrestricted to enable.")]
    ShellDisabled,

    #[error("Shell command '{command}' is not in the allowlist. Allowed: {allowed}")]
    ShellBlocked { command: String, allowed: String },

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Upstream API error: {0}")]
    UpstreamError(String),
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        let (status, error_type) = match &self {
            BridgeError::ShellDisabled => (StatusCode::FORBIDDEN, "permission_error"),
            BridgeError::ShellBlocked { .. } => (StatusCode::FORBIDDEN, "permission_error"),
            BridgeError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            BridgeError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
            BridgeError::UpstreamError(_) => (StatusCode::BAD_GATEWAY, "api_error"),
        };

        let body = json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": self.to_string(),
            }
        });

        (status, Json(body)).into_response()
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
}
