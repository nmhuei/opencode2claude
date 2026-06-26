//! Web search functions with fallback chain.
//!
//! Provides multiple search backends (DuckDuckGo, Tavily, Exa, Serper, SearXNG)
//! with automatic fallback in priority order.

use crate::config::BridgeConfig;
use tracing::{info, warn};

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

pub async fn perform_duckduckgo_search(query: &str) -> String {
    let client = reqwest::Client::new();
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding_simple(query)
    );
    let res = match client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await {
            Ok(r) => r,
            Err(e) => return format!("Search failed: {}", e),
        };

    let html = match res.text().await {
        Ok(t) => t,
        Err(e) => return format!("Failed to read search body: {}", e),
    };

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
        "No results found.".to_string()
    } else {
        results.join("\n")
    }
}

pub async fn perform_tavily_search(query: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "include_answer": false,
        "max_results": 5
    });

    let res = client
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

pub async fn perform_exa_search(query: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "query": query,
        "numResults": 5,
        "useAutoprompt": true
    });

    let res = client
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

pub async fn perform_serper_search(query: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "q": query
    });

    let res = client
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
            let url = item.get("link").and_then(|v| v.as_str()).unwrap_or("");
            let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                url, title, snippet
            ));
        }
    }

    if results.is_empty() {
        Ok("No results found on Serper.dev.".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

pub async fn perform_searxng_search(query: &str, base_url: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let url = if base_url.contains('?') {
        format!("{}&q={}&format=json", base_url, urlencoding_simple(query))
    } else {
        format!(
            "{}/search?q={}&format=json",
            base_url.trim_end_matches('/'),
            urlencoding_simple(query)
        )
    };

    let res = client.get(&url).send().await.map_err(|e| e.to_string())?;

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

pub async fn perform_search_fallback(query: &str, config: &BridgeConfig) -> String {
    // 1. Tavily
    if let Some(ref api_key) = config.tavily_api_key {
        info!("Attempting Tavily search...");
        match perform_tavily_search(query, api_key).await {
            Ok(results) => return results,
            Err(e) => warn!("Tavily search failed: {}. Falling back...", e),
        }
    }

    // 2. Exa
    if let Some(ref api_key) = config.exa_api_key {
        info!("Attempting Exa search...");
        match perform_exa_search(query, api_key).await {
            Ok(results) => return results,
            Err(e) => warn!("Exa search failed: {}. Falling back...", e),
        }
    }

    // 3. Serper
    if let Some(ref api_key) = config.serper_api_key {
        info!("Attempting Serper.dev search...");
        match perform_serper_search(query, api_key).await {
            Ok(results) => return results,
            Err(e) => warn!("Serper search failed: {}. Falling back...", e),
        }
    }

    // 4. SearXNG
    if let Some(ref url) = config.searxng_url {
        info!("Attempting SearXNG search...");
        match perform_searxng_search(query, url).await {
            Ok(results) => return results,
            Err(e) => warn!("SearXNG search failed: {}. Falling back...", e),
        }
    }

    // 5. DuckDuckGo as default fallback
    info!("Attempting DuckDuckGo search...");
    perform_duckduckgo_search(query).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("http%3A%2F%2Fexample.com"), "http://example.com");
        assert_eq!(url_decode("abc"), "abc");
    }

    #[tokio::test]
    async fn test_perform_duckduckgo_search() {
        let results = perform_duckduckgo_search("rust programming").await;
        assert!(!results.is_empty());
    }
}
