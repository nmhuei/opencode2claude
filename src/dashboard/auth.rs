//! Dashboard login, cookie, and request authentication.

use crate::management::auth as management_auth;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// POST /api/dashboard/login — check token against DASHBOARD_ADMIN_TOKEN.
/// Accepts token in JSON body (`{"token": "..."}`) or `X-Dashboard-Token` header.
/// Sets an HttpOnly session cookie on success so subsequent requests are authenticated.
pub async fn handler_login(
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let admin_token = management_auth::dashboard_token().unwrap_or_default();

    // Extract token: JSON body first, then header/cookie fallback
    let token = body
        .and_then(|b| {
            b.get("token")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
        .or_else(|| {
            // Fallback: try X-Dashboard-Token header
            headers
                .get("X-Dashboard-Token")
                .and_then(|v| v.to_str().ok().map(|s| s.to_string()))
        });

    match token {
        Some(t)
            if !t.is_empty() && management_auth::token_eq(t.as_bytes(), admin_token.as_bytes()) =>
        {
            // Success — set session cookie
            let mut res = Json(json!({ "status": "ok", "success": true })).into_response();
            res.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&format!(
                    "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                    management_auth::SESSION_COOKIE,
                    admin_token
                ))
                .unwrap(),
            );
            Ok(res)
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
    let mut res = Json(json!({ "status": "ok", "success": true })).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
            management_auth::SESSION_COOKIE,
            ""
        ))
        .unwrap(),
    );
    res
}

/// GET /api/dashboard/auth/status — public endpoint that reports whether admin token is
/// configured and whether the current request is authenticated. No auth required.
pub async fn handler_auth_status(headers: HeaderMap) -> Json<Value> {
    let admin_token = management_auth::dashboard_token().unwrap_or_default();
    let request_token = management_auth::dashboard_request_token(&headers, None);
    let authenticated = request_token
        .is_some_and(|token| management_auth::token_eq(token.as_bytes(), admin_token.as_bytes()));

    Json(json!({
        "admin_token_configured": !admin_token.is_empty(),
        "authenticated": authenticated,
    }))
}

/// Validate the dashboard token from header, query string, or session cookie.
pub(super) fn check_admin_token(
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let admin_token = management_auth::dashboard_token().ok_or_else(|| {
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
