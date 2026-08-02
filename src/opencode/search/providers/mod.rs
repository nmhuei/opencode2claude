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
