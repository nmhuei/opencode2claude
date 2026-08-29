//! Individual typed search-provider adapters.

mod duckduckgo;
mod exa;
mod searxng;
mod serper;
mod tavily;
mod yahoo;

use super::types::{SearchError, SearchErrorKind, SearchProviderKind};
use futures_util::StreamExt;
use reqwest::Response;
use serde_json::Value;

pub(super) use duckduckgo::search as duckduckgo;
pub(super) use exa::search as exa;
pub(super) use searxng::search as searxng;
pub(super) use serper::search as serper;
pub(super) use tavily::search as tavily;
pub(super) use yahoo::search as yahoo;

/// Redirect hops a keyless scraper may follow before being cut off.
const MAX_SCRAPER_REDIRECTS: usize = 3;

/// Whether one redirect hop keeps the keyless scraper on its current origin:
/// identical host plus either an unchanged scheme/port pair or an explicit
/// http->https upgrade whose port stays unchanged or makes the canonical
/// move between the schemes' default ports (80 -> 443). The port is part of
/// the identity — loopback-mock attacks differ only by port — so even an
/// upgrade must never relocate the scraper onto an arbitrary TLS port of
/// the same host.
pub(super) fn scraper_redirect_stays_on_origin(last: &reqwest::Url, target: &reqwest::Url) -> bool {
    if last.host_str() != target.host_str() {
        return false;
    }
    if last.scheme() == target.scheme() {
        return last.port_or_known_default() == target.port_or_known_default();
    }
    // Sole carve-out: explicit http->https upgrade, with port discipline.
    last.scheme() == "http"
        && target.scheme() == "https"
        && (last.port_or_known_default() == target.port_or_known_default()
            || (last.port_or_known_default() == Some(80)
                && target.port_or_known_default() == Some(443)))
}

/// Redirect policy for the keyless HTML/JSON scrapers (DuckDuckGo, Yahoo,
/// SearXNG): same host only, bounded hops, never a scheme downgrade — and an
/// http->https upgrade may keep or canonically move the port (80 -> 443),
/// never land on an arbitrary one.
///
/// Off-host bounces are the classic way an open redirect turns a search
/// integration into an SSRF gadget; blocked bounces surface as transport
/// errors and simply fall through to the next provider. Same-host redirects
/// stay enabled because search endpoints legitimately relocate between paths
/// on their own origin.
pub(super) fn scraper_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_SCRAPER_REDIRECTS {
            return attempt.error("search scraper exceeded redirect hop limit");
        }
        // Every hop must stay on one origin (see
        // scraper_redirect_stays_on_origin). Comparing against the most
        // recent visited URL is sufficient — any single off-origin jump is
        // rejected at its own hop.
        let Some(last) = attempt.previous().last() else {
            return attempt.error("redirect without recorded origin");
        };
        if !scraper_redirect_stays_on_origin(last, attempt.url()) {
            return attempt.error("redirect to a different origin blocked");
        }
        attempt.follow()
    })
}

async fn read_json_response(
    provider: SearchProviderKind,
    response: Response,
    max_bytes: usize,
) -> Result<Value, SearchError> {
    let status = response.status();
    let bytes = read_bounded(provider, response, max_bytes).await?;
    if !status.is_success() {
        return Err(SearchError::new(
            provider,
            SearchErrorKind::HttpStatus,
            format!(
                "status {status}: {}",
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        SearchError::new(
            provider,
            SearchErrorKind::MalformedResponse,
            format!("invalid JSON response: {error}"),
        )
    })
}

async fn read_text_response(
    provider: SearchProviderKind,
    response: Response,
    max_bytes: usize,
) -> Result<String, SearchError> {
    let status = response.status();
    let bytes = read_bounded(provider, response, max_bytes).await?;
    if !status.is_success() {
        return Err(SearchError::new(
            provider,
            SearchErrorKind::HttpStatus,
            format!(
                "status {status}: {}",
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        SearchError::new(
            provider,
            SearchErrorKind::MalformedResponse,
            format!("response is not valid UTF-8: {error}"),
        )
    })
}

async fn read_bounded(
    provider: SearchProviderKind,
    response: Response,
    max_bytes: usize,
) -> Result<Vec<u8>, SearchError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(SearchError::new(
            provider,
            SearchErrorKind::ResponseTooLarge,
            format!("response content-length exceeds {max_bytes} bytes"),
        ));
    }
    let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| map_reqwest_error(provider, error))?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(SearchError::new(
                provider,
                SearchErrorKind::ResponseTooLarge,
                format!("response exceeded {max_bytes} bytes"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_reqwest_error(provider: SearchProviderKind, error: reqwest::Error) -> SearchError {
    let kind = if error.is_timeout() {
        SearchErrorKind::Timeout
    } else {
        SearchErrorKind::Transport
    };
    SearchError::new(provider, kind, error.to_string())
}
