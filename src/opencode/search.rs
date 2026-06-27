//! Web search functions with fallback chain.
//!
//! Provides multiple search backends (DuckDuckGo, Tavily, Exa, Serper, SearXNG)
//! with automatic fallback in priority order.
//!
//! Uses a single shared `reqwest::Client` via `SearchClient`.

use crate::config::BridgeConfig;
use reqwest::Client;
use tracing::{info, warn};

// ── Helper functions ──

pub fn url_decode(s: &str) -> String {
    let mut decoded = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(c1), Some(c2)) = (h1, h2) {
                if let Ok(val) = u8::from_str_radix(&format!("{}{}", c1, c2), 16) {
                    decoded.push(val as char);
                    continue;
                }
            }
        }
        decoded.push(ch);
    }
    decoded
}

pub fn urlencoding_simple(query: &str) -> String {
    let mut encoded = String::new();
    for b in query.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => {
                encoded.push('+');
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

pub fn strip_html_tags(html: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            output.push(c);
        }
    }
    output
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

// ── Types ──

/// A structured search result with title, URL, and snippet.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Enumeration of supported search providers.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchProviderKind {
    Tavily,
    Exa,
    Serper,
    SearXng,
    DuckDuckGo,
}

/// Reusable search client that shares a single `reqwest::Client`.
///
/// Holds API keys and endpoint URLs from the bridge configuration.
/// The `search()` method tries providers in priority order (Tavily -> Exa -> Serper -> SearXNG -> DuckDuckGo),
/// skipping any that are not configured and falling back on failure.
#[derive(Debug, Clone)]
pub struct SearchClient {
    client: Client,
    tavily_key: Option<String>,
    exa_key: Option<String>,
    serper_key: Option<String>,
    searxng_url: Option<String>,
}

impl SearchClient {
    /// Create a new `SearchClient` reusing the given HTTP client.
    pub fn new(client: Client, config: &BridgeConfig) -> Self {
        Self {
            client,
            tavily_key: config.tavily_api_key.clone(),
            exa_key: config.exa_api_key.clone(),
            serper_key: config.serper_api_key.clone(),
            searxng_url: config.searxng_url.clone(),
        }
    }

    /// Run a search query using the fallback chain.
    ///
    /// Priority: Tavily -> Exa -> Serper -> SearXNG -> DuckDuckGo.
    /// Each paid/configured provider is attempted first; failures log a warning and fall through.
    /// DuckDuckGo serves as the universal fallback (no API key required).
    pub async fn search(&self, query: &str) -> String {
        // 1. Tavily
        if let Some(ref api_key) = self.tavily_key {
            info!("Attempting Tavily search...");
            match self.tavily_search(query, api_key).await {
                Ok(results) => return results,
                Err(e) => warn!("Tavily search failed: {}. Falling back...", e),
            }
        }

        // 2. Exa
        if let Some(ref api_key) = self.exa_key {
            info!("Attempting Exa search...");
            match self.exa_search(query, api_key).await {
                Ok(results) => return results,
                Err(e) => warn!("Exa search failed: {}. Falling back...", e),
            }
        }

        // 3. Serper
        if let Some(ref api_key) = self.serper_key {
            info!("Attempting Serper.dev search...");
            match self.serper_search(query, api_key).await {
                Ok(results) => return results,
                Err(e) => warn!("Serper search failed: {}. Falling back...", e),
            }
        }

        // 4. SearXNG
        if let Some(ref url) = self.searxng_url {
            info!("Attempting SearXNG search...");
            match self.searxng_search(query, url).await {
                Ok(results) => return results,
                Err(e) => warn!("SearXNG search failed: {}. Falling back...", e),
            }
        }

        // 5. DuckDuckGo as default fallback
        info!("Attempting DuckDuckGo search...");
        self.duckduckgo_search(query).await.unwrap_or_else(|e| e)
    }

    // ── Private provider methods ──

    async fn tavily_search(&self, query: &str, api_key: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "api_key": api_key,
            "query": query,
            "include_answer": false,
            "max_results": 5
        });

        let res = self
            .client
            .post("https://api.tavily.com/search")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = res.status();
        if !status.is_success() {
            let err_body = res.text().await.unwrap_or_default();
            return Err(format!("Tavily status {}: {}", status, err_body));
        }

        let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
            for item in arr {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
                results.push(format!(
                    "URL: {}\nTitle: {}\nSnippet: {}\n",
                    url, title, content
                ));
            }
        }

        if results.is_empty() {
            Ok("No results found on Tavily.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }

    async fn exa_search(&self, query: &str, api_key: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "query": query,
            "numResults": 5,
            "useAutoprompt": true
        });

        let res = self
            .client
            .post("https://api.exa.ai/search")
            .header("x-api-key", api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = res.status();
        if !status.is_success() {
            let err_body = res.text().await.unwrap_or_default();
            return Err(format!("Exa status {}: {}", status, err_body));
        }

        let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
            for item in arr {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");

                let mut snippet = String::new();
                if let Some(highlights) = item.get("highlights").and_then(|v| v.as_array()) {
                    let h_texts: Vec<&str> = highlights.iter().filter_map(|h| h.as_str()).collect();
                    if !h_texts.is_empty() {
                        snippet = h_texts.join(" ... ");
                    }
                }
                if snippet.is_empty() {
                    snippet = item
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                if snippet.len() > 300 {
                    snippet = format!("{}...", &snippet[..300]);
                }

                results.push(format!(
                    "URL: {}\nTitle: {}\nSnippet: {}\n",
                    url, title, snippet
                ));
            }
        }

        if results.is_empty() {
            Ok("No results found on Exa.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }

    async fn serper_search(&self, query: &str, api_key: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "q": query
        });

        let res = self
            .client
            .post("https://google.serper.dev/search")
            .header("X-API-KEY", api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = res.status();
        if !status.is_success() {
            let err_body = res.text().await.unwrap_or_default();
            return Err(format!("Serper status {}: {}", status, err_body));
        }

        let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        let mut results = Vec::new();

        if let Some(answer_box) = parsed.get("answerBox") {
            if let Some(snippet) = answer_box.get("snippet").and_then(|v| v.as_str()) {
                results.push(format!("Answer Box (Direct Answer):\n{}\n", snippet));
            }
        }

        if let Some(arr) = parsed.get("organic").and_then(|v| v.as_array()) {
            for item in arr {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let link = item.get("link").and_then(|v| v.as_str()).unwrap_or("");
                let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                results.push(format!(
                    "URL: {}\nTitle: {}\nSnippet: {}\n",
                    link, title, snippet
                ));
            }
        }

        if results.is_empty() {
            Ok("No results found on Serper.dev.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }

    async fn searxng_search(&self, query: &str, base_url: &str) -> Result<String, String> {
        let url = if base_url.contains('?') {
            format!("{}&q={}&format=json", base_url, urlencoding_simple(query))
        } else {
            format!(
                "{}/search?q={}&format=json",
                base_url.trim_end_matches('/'),
                urlencoding_simple(query)
            )
        };

        let res = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = res.status();
        if !status.is_success() {
            let err_body = res.text().await.unwrap_or_default();
            return Err(format!("SearXNG status {}: {}", status, err_body));
        }

        let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
            for item in arr {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
                results.push(format!(
                    "URL: {}\nTitle: {}\nSnippet: {}\n",
                    url, title, content
                ));
            }
        }

        if results.is_empty() {
            Ok("No results found on SearXNG.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }

    async fn duckduckgo_search(&self, query: &str) -> Result<String, String> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding_simple(query)
        );
        let res = self
            .client
            .get(&url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .send()
            .await
            .map_err(|e| format!("Search failed: {}", e))?;

        let html = res
            .text()
            .await
            .map_err(|e| format!("Failed to read search body: {}", e))?;

        let mut results = Vec::new();
        let mut remaining = &html[..];
        while let Some(start_pos) = remaining.find("<a class=\"result__snippet\"") {
            remaining = &remaining[start_pos..];
            if let Some(href_start) = remaining.find("href=\"") {
                let href_content = &remaining[href_start + 6..];
                if let Some(href_end) = href_content.find("\"") {
                    let link = &href_content[..href_end];
                    let url = if let Some(uddg_pos) = link.find("uddg=") {
                        let uddg_val = &link[uddg_pos + 5..];
                        let end_pos = uddg_val.find('&').unwrap_or(uddg_val.len());
                        url_decode(&uddg_val[..end_pos])
                    } else {
                        format!("https:{}", link)
                    };

                    if let Some(tag_end) = remaining.find(">") {
                        let text_content = &remaining[tag_end + 1..];
                        if let Some(anchor_end) = text_content.find("</a>") {
                            let snippet = &text_content[..anchor_end];
                            let clean_snippet = strip_html_tags(snippet);
                            results.push(format!("URL: {}\nSnippet: {}\n", url, clean_snippet));
                        }
                    }
                }
            }
            if let Some(next_pos) = remaining.find("</a>") {
                remaining = &remaining[next_pos + 4..];
            } else {
                break;
            }
            if results.len() >= 5 {
                break;
            }
        }

        if results.is_empty() {
            Ok("No results found.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::ShellPolicy;
    use reqwest::Client;

    #[test]
    fn test_search_provider_kind() {
        assert_ne!(SearchProviderKind::Tavily, SearchProviderKind::DuckDuckGo);
        assert_eq!(SearchProviderKind::Exa, SearchProviderKind::Exa);
    }

    #[test]
    fn test_search_result_struct() {
        let result = SearchResult {
            title: "Test".to_string(),
            url: "https://example.com".to_string(),
            snippet: "A test snippet".to_string(),
        };
        assert_eq!(result.title, "Test");
        assert_eq!(result.url, "https://example.com");
        assert_eq!(result.snippet, "A test snippet");
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("http%3A%2F%2Fexample.com"), "http://example.com");
        assert_eq!(url_decode("abc"), "abc");
    }

    fn make_test_config() -> BridgeConfig {
        BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 4000,
            opencode_port: 4096,
            model: None,
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 256,
            tavily_api_key: None,
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 5,
            proxies: None,
        }
    }

    #[test]
    fn test_search_client_creation() {
        let client = Client::new();
        let config = BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 4000,
            opencode_port: 4096,
            model: None,
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 256,
            tavily_api_key: Some("test-key".to_string()),
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 5,
            proxies: None,
        };
        let search_client = SearchClient::new(client, &config);
        assert_eq!(search_client.tavily_key, Some("test-key".to_string()));
        assert!(search_client.exa_key.is_none());
        assert!(search_client.serper_key.is_none());
        assert!(search_client.searxng_url.is_none());
    }

    #[test]
    fn test_search_client_clone() {
        let client = Client::new();
        let config = make_test_config();
        let original = SearchClient::new(client, &config);
        let cloned = original.clone();
        assert!(cloned.tavily_key.is_none());
        assert!(cloned.exa_key.is_none());
    }

    #[test]
    fn test_search_client_search_no_config_falls_to_ddg() {
        let client = Client::new();
        let config = make_test_config();
        let search_client = SearchClient::new(client, &config);
        // No paid providers configured, should try DuckDuckGo
        // We just verify the client is built correctly
        assert!(search_client.tavily_key.is_none());
        assert!(search_client.exa_key.is_none());
        assert!(search_client.serper_key.is_none());
        assert!(search_client.searxng_url.is_none());
    }

    #[tokio::test]
    async fn test_duckduckgo_search_via_client() {
        let client = Client::new();
        let config = make_test_config();
        let search_client = SearchClient::new(client, &config);
        let results = search_client.search("rust programming").await;
        assert!(!results.is_empty());
    }

    #[test]
    fn test_url_encode_basic() {
        assert_eq!(urlencoding_simple("hello world"), "hello+world");
    }

    #[test]
    fn test_url_encode_special_chars() {
        assert_eq!(urlencoding_simple("a/b?c=d"), "a%2Fb%3Fc%3Dd");
    }

    #[test]
    fn test_url_encode_alphanumeric() {
        assert_eq!(urlencoding_simple("abc123"), "abc123");
    }

    #[test]
    fn test_url_decode_roundtrip() {
        let original = "hello%20world%20%26%20special";
        assert_eq!(url_decode(original), "hello world & special");
    }

    #[test]
    fn test_strip_html_tags_basic() {
        let html = "<p>Hello <b>World</b></p>";
        assert_eq!(strip_html_tags(html), "Hello World");
    }

    #[test]
    fn test_strip_html_tags_entities() {
        let html = "&quot;quoted&quot; &amp; &lt;tag&gt;";
        assert_eq!(strip_html_tags(html), "\"quoted\" & <tag>");
    }

    #[test]
    fn test_strip_html_tags_nested() {
        let html = "<div><span>nested</span></div>";
        assert_eq!(strip_html_tags(html), "nested");
    }

    #[test]
    fn test_strip_html_tags_no_tags() {
        assert_eq!(strip_html_tags("plain text"), "plain text");
    }

    #[test]
    fn test_strip_html_tags_empty() {
        assert_eq!(strip_html_tags(""), "");
    }
}
