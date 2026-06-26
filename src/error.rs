//! Bridge error types with proper HTTP response mapping.
//!
//! All errors are converted to Anthropic-compatible JSON error responses
//! so that Claude Code can understand and display them correctly.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Central error type for the OpenCode2Claude bridge.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("Failed to bind to address: {0}")]
    BindFailed(#[source] std::io::Error),

    #[error("Failed to spawn process: {0}")]
    ProcessSpawnFailed(#[source] std::io::Error),

    #[error("Shell commands are disabled by policy. Set BRIDGE_SHELL_POLICY=allowlist or unrestricted to enable.")]
    ShellDisabled,

    #[error("Shell command '{command}' is not in the allowlist. Allowed: {allowed}")]
    ShellBlocked { command: String, allowed: String },

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

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
