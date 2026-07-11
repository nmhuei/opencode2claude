//! Dashboard login, cookie, and request authentication.

use crate::management::auth as management_auth;
use crate::state::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// POST /api/dashboard/login — check token against resolved management config.
pub async fn handler_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let admin_token = management_auth::dashboard_token(&state.config).unwrap_or_default();

    let token = body
        .and_then(|b| {
            b.get("token")
                .and_then(|v| v.as_str().map(ToOwned::to_owned))
        })
        .or_else(|| {
            headers
                .get("X-Dashboard-Token")
                .and_then(|v| v.to_str().ok().map(ToOwned::to_owned))
        });

    match token {
        Some(token)
            if !token.is_empty()
                && management_auth::token_eq(token.as_bytes(), admin_token.as_bytes()) =>
        {
            let mut response = Json(json!({ "status": "ok", "success": true })).into_response();
            let cookie = HeaderValue::from_str(&format!(
                "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                management_auth::SESSION_COOKIE,
                admin_token
            ))
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"status": "error", "message": "failed to create session"})),
                )
            })?;
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            Ok(response)
        }
        Some(_) => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "error",
                "message": "Invalid password",
            })),
        )),
        None => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "error",
                "message": "Please enter password to login",
            })),
        )),
    }
}

/// POST /api/dashboard/logout — clear the session cookie.
pub async fn handler_logout() -> Response {
    let mut response = Json(json!({ "status": "ok", "success": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "bridge_admin_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

/// GET /api/dashboard/auth/status — public authentication status.
pub async fn handler_auth_status(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let admin_token = management_auth::dashboard_token(&state.config).unwrap_or_default();
    let request_token = management_auth::dashboard_request_token(&headers, None);
    let authenticated = !admin_token.is_empty()
        && request_token.is_some_and(|token| {
            management_auth::token_eq(token.as_bytes(), admin_token.as_bytes())
        });

    Json(json!({
        "admin_token_configured": !admin_token.is_empty(),
        "authenticated": authenticated,
    }))
}

/// Validate dashboard token from header, query string, or session cookie.
pub(super) fn check_admin_token(
    state: &AppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let admin_token = management_auth::dashboard_token(&state.config).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "error",
                "message": "Dashboard is disabled: admin token is not configured on the server",
            })),
        )
    })?;

    let request_token =
        management_auth::dashboard_request_token(headers, query_token).ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "error",
                    "message": "Please enter password to login",
                })),
            )
        })?;

    if management_auth::token_eq(request_token.as_bytes(), admin_token.as_bytes()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "error",
                "message": "Invalid password",
            })),
        ))
    }
}
