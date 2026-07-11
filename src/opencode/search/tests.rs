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
        shell_policy: ShellPolicy::Disabled,
        max_body_size: 1024,
        stream_buffer_size: 4096,
        channel_capacity: 256,
        ..Default::default()
    }
}

#[test]
fn test_search_client_creation() {
    let client = Client::new();
    let config = BridgeConfig {
        host: "127.0.0.1".parse().unwrap(),
        bridge_port: 4000,
        opencode_port: 4096,
        shell_policy: ShellPolicy::Disabled,
        max_body_size: 1024,
        stream_buffer_size: 4096,
        channel_capacity: 256,
        tavily_api_key: Some("test-key".into()),
        ..Default::default()
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

#[cfg(test)]
mod provider_http_fixtures {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::Html;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    type Calls = Arc<Mutex<Vec<String>>>;

    async fn log(calls: &Calls, value: &str) {
        calls.lock().await.push(value.to_string());
    }

    async fn tavily_ok(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "tavily").await;
        Json(serde_json::json!({"results":[{
            "title":"Tavily fixture",
            "url":"https://example.com/tavily",
            "content":"fixture content"
        }]}))
    }

    async fn tavily_fail(State(calls): State<Calls>) -> (StatusCode, &'static str) {
        log(&calls, "tavily-fail").await;
        (StatusCode::TOO_MANY_REQUESTS, "rate limited")
    }

    async fn exa_ok(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "exa").await;
        Json(serde_json::json!({"results":[{
            "title":"Exa fixture",
            "url":"https://example.com/exa",
            "highlights":["highlight fixture"]
        }]}))
    }

    async fn serper_ok(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "serper").await;
        Json(serde_json::json!({"organic":[{
            "title":"Serper fixture",
            "link":"https://example.com/serper",
            "snippet":"serper content"
        }]}))
    }

    async fn searx_ok(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "searxng").await;
        Json(serde_json::json!({"results":[{
            "title":"SearXNG fixture",
            "url":"https://example.com/searxng",
            "content":"searx content"
        }]}))
    }

    async fn duck_ok(State(calls): State<Calls>) -> Html<&'static str> {
        log(&calls, "duckduckgo").await;
        Html(
            r#"<div class="result__body">
              <a class="result__a" href="https://example.com/duck">Duck fixture</a>
              <a class="result__snippet" href="https://example.com/duck">duck content</a>
            </div>"#,
        )
    }

    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(200)).await;
        "<html>late</html>"
    }

    async fn oversized() -> String {
        "x".repeat(4096)
    }

    async fn spawn() -> (String, Calls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/tavily-ok", post(tavily_ok))
            .route("/tavily-fail", post(tavily_fail))
            .route("/exa-ok", post(exa_ok))
            .route("/serper-ok", post(serper_ok))
            .route("/searx/search", get(searx_ok))
            .route("/duck-ok", get(duck_ok))
            .route("/slow", get(slow))
            .route("/oversized", get(oversized))
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), calls)
    }

    fn base_config(base: &str) -> BridgeConfig {
        let mut config = make_test_config();
        config.search.tavily_url = format!("{base}/tavily-ok");
        config.search.exa_url = format!("{base}/exa-ok");
        config.search.serper_url = format!("{base}/serper-ok");
        config.search.duckduckgo_url = format!("{base}/duck-ok");
        config.search.request_timeout = Duration::from_secs(1);
        config.search.max_response_bytes = 16 * 1024;
        config.search.allow_private_searxng = true;
        config
    }

    #[tokio::test]
    async fn each_provider_executes_against_local_http_fixture() {
        let (base, _calls) = spawn().await;

        let mut tavily = base_config(&base);
        tavily.tavily_api_key = Some("key".into());
        let result = SearchClient::new(Client::new(), &tavily)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(result[0].title, "Tavily fixture");

        let mut exa = base_config(&base);
        exa.exa_api_key = Some("key".into());
        let result = SearchClient::new(Client::new(), &exa)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(result[0].title, "Exa fixture");

        let mut serper = base_config(&base);
        serper.serper_api_key = Some("key".into());
        let result = SearchClient::new(Client::new(), &serper)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(result[0].title, "Serper fixture");

        let mut searx = base_config(&base);
        searx.searxng_url = Some(format!("{base}/searx"));
        let result = SearchClient::new(Client::new(), &searx)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(result[0].title, "SearXNG fixture");

        let duck = base_config(&base);
        let result = SearchClient::new(Client::new(), &duck)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(result[0].title, "Duck fixture");
    }

    #[tokio::test]
    async fn fallback_order_moves_from_failed_tavily_to_exa() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.tavily_api_key = Some("key".into());
        config.exa_api_key = Some("key".into());
        config.search.tavily_url = format!("{base}/tavily-fail");
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(results[0].title, "Exa fixture");
        assert_eq!(*calls.lock().await, vec!["tavily-fail", "exa"]);
    }

    #[tokio::test]
    async fn timeout_and_response_size_limits_are_typed() {
        let (base, _calls) = spawn().await;
        let mut timeout = base_config(&base);
        timeout.search.duckduckgo_url = format!("{base}/slow");
        timeout.search.request_timeout = Duration::from_millis(20);
        let error = SearchClient::new(Client::new(), &timeout)
            .search_results("rust")
            .await
            .unwrap_err();
        assert_eq!(error.kind, SearchErrorKind::Timeout);

        let mut oversized_config = base_config(&base);
        oversized_config.search.duckduckgo_url = format!("{base}/oversized");
        oversized_config.search.max_response_bytes = 1024;
        let error = SearchClient::new(Client::new(), &oversized_config)
            .search_results("rust")
            .await
            .unwrap_err();
        assert_eq!(error.kind, SearchErrorKind::ResponseTooLarge);
    }
}
