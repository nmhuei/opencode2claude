//! Authentication helpers shared by management transports.

use crate::config::BridgeConfig;
use axum::http::{header, HeaderMap};

pub const SESSION_COOKIE: &str = "bridge_admin_session";

pub fn dashboard_token(config: &BridgeConfig) -> Option<&str> {
    config.management.dashboard_token()
}

pub fn rest_token(config: &BridgeConfig) -> Option<&str> {
    config.management.rest_token()
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token)
    } else {
        None
    }
}

pub fn dashboard_request_token<'a>(
    headers: &'a HeaderMap,
    query_token: Option<&'a str>,
) -> Option<String> {
    headers
        .get("X-Dashboard-Token")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .or_else(|| query_token.map(ToOwned::to_owned))
        .or_else(|| cookie_value(headers, SESSION_COOKIE))
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (key, value) = cookie.trim().split_once('=')?;
                (key == name).then(|| value.to_string())
            })
        })
}

/// Compare secret bytes without returning on the first mismatching byte.
pub fn token_eq(provided: &[u8], expected: &[u8]) -> bool {
    let max_len = provided.len().max(expected.len());
    let mut diff = provided.len() ^ expected.len();

    for index in 0..max_len {
        let left = provided.get(index).copied().unwrap_or_default();
        let right = expected.get(index).copied().unwrap_or_default();
        diff |= usize::from(left ^ right);
    }

    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BridgeConfig, ManagementConfig};
    use axum::http::HeaderValue;

    #[test]
    fn compares_tokens_without_length_shortcut() {
        assert!(token_eq(b"abc", b"abc"));
        assert!(!token_eq(b"abc", b"abd"));
        assert!(!token_eq(b"abc", b"abcd"));
    }

    #[test]
    fn reads_dashboard_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=x; bridge_admin_session=secret"),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn tokens_are_resolved_from_config_without_environment_reads() {
        let config = BridgeConfig {
            management: ManagementConfig {
                dashboard_token: Some("dashboard-secret".into()),
                rest_api_token: Some("rest-secret".into()),
                ..BridgeConfig::default().management
            },
            ..Default::default()
        };
        assert_eq!(dashboard_token(&config), Some("dashboard-secret"));
        assert_eq!(rest_token(&config), Some("rest-secret"));
    }
}
