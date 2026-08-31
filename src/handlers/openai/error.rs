//! OpenAI-format error response helpers.

use crate::error::BridgeError;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub fn openai_error_response(
    status: StatusCode,
    error_type: &str,
    code: Option<&str>,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message.into(),
                "type": error_type,
                "param": null,
                "code": code,
            }
        })),
    )
        .into_response()
}

pub(super) fn openai_bridge_error(error: BridgeError) -> Response {
    match error {
        BridgeError::InvalidRequest(message) => openai_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            None,
            message,
        ),
        BridgeError::Unauthorized(message) => openai_error_response(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            Some("invalid_api_key"),
            message,
        ),
        BridgeError::Forbidden(message) => openai_error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            Some("key_policy_denied"),
            message,
        ),
        BridgeError::RateLimited(message) => openai_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            Some("rate_limit_exceeded"),
            message,
        ),
        BridgeError::PaymentRequired(message) => openai_error_response(
            StatusCode::PAYMENT_REQUIRED,
            "billing_error",
            Some("payment_required"),
            message,
        ),
        BridgeError::EgressUnavailable(message) => openai_error_response(
            StatusCode::BAD_REQUEST,
            "api_error",
            Some("egress_unavailable"),
            message,
        ),
        other => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "api_error",
            None,
            other.to_string(),
        ),
    }
}
