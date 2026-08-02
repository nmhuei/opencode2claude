//! Search-domain types and bounded formatting policy.

use super::util::truncate_chars;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub max_results: usize,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>, max_results: usize) -> Result<Self, SearchError> {
        let text = text.into();
        let text = text.trim();
        if text.is_empty() {
            return Err(SearchError::new(
                SearchProviderKind::DuckDuckGo,
                SearchErrorKind::InvalidQuery,
                "search query cannot be empty",
            ));
        }
        if text.chars().count() > 1024 {
            return Err(SearchError::new(
                SearchProviderKind::DuckDuckGo,
                SearchErrorKind::InvalidQuery,
                "search query exceeds 1024 characters",
            ));
        }
        Ok(Self {
            text: text.to_string(),
            max_results: max_results.clamp(1, 20),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl SearchResult {
    pub fn normalized(
        title: impl Into<String>,
        url: impl Into<String>,
        snippet: impl Into<String>,
        max_snippet_chars: usize,
    ) -> Option<Self> {
        let title = compact_text(&title.into(), 240);
        let url = url.into().trim().to_string();
        let snippet = compact_text(&snippet.into(), max_snippet_chars);
        if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            return None;
        }
        Some(Self {
            title,
            url,
            snippet,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProviderKind {
    Tavily,
    Exa,
    Serper,
    SearXng,
    DuckDuckGo,
    Yahoo,
}

impl fmt::Display for SearchProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Tavily => "Tavily",
            Self::Exa => "Exa",
            Self::Serper => "Serper",
            Self::SearXng => "SearXNG",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Yahoo => "Yahoo",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchErrorKind {
    InvalidQuery,
    UnsafeEndpoint,
    Timeout,
    Transport,
    HttpStatus,
    ResponseTooLarge,
    MalformedResponse,
    NoResults,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchError {
    pub provider: SearchProviderKind,
    pub kind: SearchErrorKind,
    pub message: String,
}

impl SearchError {
    pub fn new(
        provider: SearchProviderKind,
        kind: SearchErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}: {}",
            self.provider, self.kind, self.message
        )
    }
}

impl fmt::Display for SearchErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidQuery => "invalid query",
            Self::UnsafeEndpoint => "unsafe endpoint",
            Self::Timeout => "timeout",
            Self::Transport => "transport error",
            Self::HttpStatus => "HTTP error",
            Self::ResponseTooLarge => "response too large",
            Self::MalformedResponse => "malformed response",
            Self::NoResults => "no results",
        })
    }
}

impl std::error::Error for SearchError {}

#[derive(Debug, Clone)]
pub struct SearchPolicy {
    pub max_results: usize,
    pub max_snippet_chars: usize,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
    pub allow_private_searxng: bool,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            max_results: 5,
            max_snippet_chars: 500,
            max_response_bytes: 1024 * 1024,
            request_timeout: Duration::from_secs(15),
            allow_private_searxng: false,
        }
    }
}

pub fn format_search_context(results: &[SearchResult], empty_message: &str) -> String {
    if results.is_empty() {
        return empty_message.to_string();
    }
    results
        .iter()
        .map(|result| {
            format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                result.url, result.title, result.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else if max_chars <= 3 {
        normalized.chars().take(max_chars).collect()
    } else {
        truncate_chars(&normalized, max_chars - 3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_validation_is_bounded() {
        assert!(SearchQuery::new("  rust async  ", 5).is_ok());
        assert!(SearchQuery::new("   ", 5).is_err());
        assert!(SearchQuery::new("x".repeat(1025), 5).is_err());
    }

    #[test]
    fn result_normalization_rejects_non_http_urls_and_bounds_text() {
        assert!(SearchResult::normalized("x", "file:///etc/passwd", "body", 10).is_none());
        let result = SearchResult::normalized(
            "  A   title ",
            "https://example.com",
            "một   đoạn   mô tả rất dài",
            8,
        )
        .unwrap();
        assert_eq!(result.title, "A title");
        assert_eq!(result.snippet.chars().count(), 8);
    }
}
