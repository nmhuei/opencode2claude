use super::util::truncate_chars;
use super::*;
use crate::config::BridgeConfig;
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
        primary_proxies: None,
        warm_standby_proxies: None,
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
        primary_proxies: None,
        warm_standby_proxies: None,
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
#[ignore = "live network call (60s+) — run with --include-ignored or PROFILE=heavy"]
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

#[test]
fn test_url_decode_utf8() {
    assert_eq!(url_decode("Ti%E1%BA%BFng%20Vi%E1%BB%87t"), "Tiếng Việt");
}

#[test]
fn test_truncate_chars_is_utf8_safe() {
    let value = "ế".repeat(301);
    let truncated = truncate_chars(&value, 300);
    assert_eq!(truncated.chars().count(), 303);
    assert!(truncated.ends_with("..."));
}
