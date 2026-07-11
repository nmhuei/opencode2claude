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
            let csrf_token = uuid::Uuid::new_v4().simple().to_string();
            let mut response = Json(json!({
                "status": "ok",
                "success": true,
                "csrf_token": csrf_token,
            }))
            .into_response();
            let session_cookie = HeaderValue::from_str(&format!(
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
            let csrf_cookie = HeaderValue::from_str(&format!(
                "{}={}; Path=/; SameSite=Strict; Max-Age=86400",
                management_auth::CSRF_COOKIE,
                csrf_token
            ))
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"status": "error", "message": "failed to create CSRF token"})),
                )
            })?;
            response
                .headers_mut()
                .append(header::SET_COOKIE, session_cookie);
            response
                .headers_mut()
                .append(header::SET_COOKIE, csrf_cookie);
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
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "bridge_admin_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("bridge_csrf_token=; Path=/; SameSite=Strict; Max-Age=0"),
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

/// Validate a state-changing dashboard request. Header/query credentials are
/// explicit automation credentials; cookie-authenticated browser requests must
/// additionally present a matching double-submit CSRF token.
pub(super) fn check_admin_mutation(
    state: &AppState,
    headers: &HeaderMap,
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
    let (provided, source) = management_auth::dashboard_request_credential(headers, None)
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"status":"error","message":"Please login first"})),
            )
        })?;
    if !management_auth::token_eq(provided.as_bytes(), admin_token.as_bytes()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"status":"error","message":"Invalid password"})),
        ));
    }
    if state.config.management.csrf_enabled
        && source == management_auth::DashboardAuthSource::Cookie
        && !management_auth::csrf_valid(headers)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "status":"error",
                "message":"Missing or invalid CSRF token",
            })),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod csrf_tests {
    use super::*;
    use crate::config::{BridgeConfig, ManagementConfig};
    use axum::http::HeaderValue;

    fn state() -> AppState {
        AppState::new(BridgeConfig {
            primary_proxies: None,
            warm_standby_proxies: None,
            management: ManagementConfig {
                dashboard_token: Some("dashboard-secret".into()),
                csrf_enabled: true,
                ..BridgeConfig::default().management
            },
            ..Default::default()
        })
    }

    #[test]
    fn cookie_authenticated_mutation_requires_double_submit_token() {
        let state = state();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("bridge_admin_session=dashboard-secret"),
        );
        let error = check_admin_mutation(&state, &headers).unwrap_err();
        assert_eq!(error.0, StatusCode::FORBIDDEN);

        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "bridge_admin_session=dashboard-secret; bridge_csrf_token=csrf-123",
            ),
        );
        headers.insert(
            management_auth::CSRF_HEADER,
            HeaderValue::from_static("csrf-123"),
        );
        assert!(check_admin_mutation(&state, &headers).is_ok());
    }

    #[test]
    fn explicit_dashboard_header_is_not_subject_to_browser_csrf() {
        let state = state();
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Dashboard-Token",
            HeaderValue::from_static("dashboard-secret"),
        );
        assert!(check_admin_mutation(&state, &headers).is_ok());
    }
}
