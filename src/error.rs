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
    #[allow(dead_code)]
    #[error("Failed to bind to address: {0}")]
    BindFailed(#[source] std::io::Error),

    #[allow(dead_code)]
    #[error("Failed to spawn process: {0}")]
    ProcessSpawnFailed(#[source] std::io::Error),

    #[allow(dead_code)]
    #[error("Shell commands are disabled by policy. Set BRIDGE_SHELL_POLICY=allowlist or unrestricted to enable.")]
    ShellDisabled,

    #[error("Shell command '{command}' is not in the allowlist. Allowed: {allowed}")]
    ShellBlocked { command: String, allowed: String },

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[allow(dead_code)]
    #[error("OpenCode daemon unavailable on port {0}")]
    DaemonUnavailable(u16),

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
            BridgeError::DaemonUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                self.to_string(),
            ),
            BridgeError::UpstreamError(_) => {
                (StatusCode::BAD_GATEWAY, "api_error", self.to_string())
            }
        };

        let body = json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": message,
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
