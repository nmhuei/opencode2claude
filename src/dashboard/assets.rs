//! Embedded dashboard assets and browser security headers.

use crate::management::auth;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use tracing::warn;

#[derive(rust_embed::RustEmbed)]
#[folder = "src/webui/"]
struct WebAssets;

/// Add baseline browser security headers to the response
fn add_security_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src 'self' https://fonts.gstatic.com; \
             img-src 'self' data: https://raw.githubusercontent.com; \
             connect-src 'self' ws: wss:; \
             frame-ancestors 'none';",
        ),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

/// Serve the web UI SPA — serves embedded assets with fallback to `index.html`.
pub async fn serve_webui(headers: HeaderMap, uri: axum::http::Uri) -> Result<Response, StatusCode> {
    let path = uri.path();
    let path = path.strip_prefix("/dashboard").unwrap_or(path);
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let resolved_path = if WebAssets::get(path).is_some() {
        path
    } else {
        "index.html"
    };

    // If requesting the SPA main page (or falling back to it), enforce auth via cookie
    if resolved_path == "index.html" {
        let admin_token = auth::dashboard_token().unwrap_or_default();
        let authenticated =
            auth::cookie_value(&headers, auth::SESSION_COOKIE).is_some_and(|cookie_token| {
                auth::token_eq(cookie_token.as_bytes(), admin_token.as_bytes())
            });

        if !authenticated {
            // Redirect to root page (login portal)
            return Ok(Redirect::temporary("/").into_response());
        }
    }

    let asset = WebAssets::get(resolved_path);

    match asset {
        Some(content) => {
            let mime = mime_guess::from_path(resolved_path).first_or_octet_stream();
            let mut res_headers = HeaderMap::new();
            res_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            res_headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            );
            res_headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
            add_security_headers(&mut res_headers);
            Ok((res_headers, content.data.to_vec()).into_response())
        }
        None => {
            warn!("Dashboard index.html not found in embedded assets");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

/// Serve the beautiful landing page at the root URL (/)
pub async fn serve_landing(headers: HeaderMap) -> Result<Response, StatusCode> {
    // If they already have a valid cookie, redirect them straight to the dashboard
    let admin_token = auth::dashboard_token().unwrap_or_default();
    let authenticated =
        auth::cookie_value(&headers, auth::SESSION_COOKIE).is_some_and(|cookie_token| {
            auth::token_eq(cookie_token.as_bytes(), admin_token.as_bytes())
        });
    if authenticated {
        return Ok(Redirect::temporary("/dashboard/").into_response());
    }

    let asset = WebAssets::get("landing.html");
    match asset {
        Some(content) => {
            let mut res_headers = HeaderMap::new();
            res_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            res_headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            );
            res_headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
            add_security_headers(&mut res_headers);
            Ok((res_headers, content.data.to_vec()).into_response())
        }
        None => {
            warn!("Dashboard landing.html not found in embedded assets");
            Err(StatusCode::NOT_FOUND)
        }
    }
}
