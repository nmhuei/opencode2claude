use super::util::truncate_chars;
use super::*;
use crate::config::BridgeConfig;
use crate::shell::ShellPolicy;
use reqwest::Client;
use std::time::Duration;

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

#[test]
fn debug_format_never_contains_api_key_material() {
    let mut config = make_test_config();
    config.tavily_api_key = Some("tavily-super-secret".into());
    config.exa_api_key = Some("exa-super-secret".into());
    config.serper_api_key = Some("serper-super-secret".into());
    config.searxng_api_key = Some("searxng-super-secret".into());
    config.searxng_url = Some("https://searx.example.com".into());
    let rendered = format!("{:?}", SearchClient::new(Client::new(), &config));
    assert!(
        !rendered.contains("super-secret"),
        "keys leaked via Debug: {rendered}"
    );
    assert_eq!(rendered.matches("[REDACTED]").count(), 4);
}

#[test]
fn test_url_encode_unicode_and_reserved_characters() {
    assert_eq!(
        urlencoding_simple("Tiếng Việt"),
        "Ti%E1%BA%BFng+Vi%E1%BB%87t"
    );
    assert_eq!(urlencoding_simple("a&b=c"), "a%26b%3Dc");
    assert_eq!(urlencoding_simple(""), "");
    assert_eq!(urlencoding_simple("~-._"), "~-._");
}

#[test]
fn test_url_decode_leaves_invalid_escapes_literal() {
    assert_eq!(url_decode("100%"), "100%");
    assert_eq!(url_decode("%ZZ"), "%ZZ");
    assert_eq!(url_decode("%4"), "%4");
    // '+' is form-encoding for space only in encode direction; uddg hrefs use
    // %20, so decode must keep '+' literal.
    assert_eq!(url_decode("a+b"), "a+b");
}

#[test]
fn query_bounds_are_char_counted_and_result_cap_clamped() {
    // Multi-byte text counts characters, not bytes: 1024 'ế' (3 bytes each) fits.
    assert!(SearchQuery::new("ế".repeat(1024), 5).is_ok());
    assert!(SearchQuery::new("ế".repeat(1025), 5).is_err());
    assert_eq!(SearchQuery::new("q", 0).unwrap().max_results, 1);
    assert_eq!(SearchQuery::new("q", 999).unwrap().max_results, 20);
}

#[test]
fn chain_budget_has_sane_default_and_typed_display() {
    let policy = SearchPolicy::default();
    assert!(
        policy.chain_budget >= Duration::from_secs(20)
            && policy.chain_budget <= Duration::from_secs(25),
        "chain budget default should sit in the 20-25s band, got {:?}",
        policy.chain_budget
    );
    assert_eq!(
        SearchErrorKind::BudgetExhausted.to_string(),
        "budget exhausted"
    );
}

#[test]
fn chain_budget_is_wired_from_config_into_client_policy() {
    let mut config = make_test_config();
    config.search.chain_budget = Duration::from_secs(17);
    let search_client = SearchClient::new(Client::new(), &config);
    assert_eq!(
        search_client.policy.chain_budget,
        Duration::from_secs(17),
        "SearchClient must take chain_budget from config.search, not SearchPolicy::default()"
    );
}

#[cfg(test)]
mod redirect_fixtures {
    use super::*;
    use axum::extract::State;
    use axum::http::{header, HeaderValue, StatusCode};
    use axum::response::{Html, IntoResponse};
    use axum::routing::get;
    use axum::Router;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    type Calls = Arc<Mutex<Vec<String>>>;

    #[derive(Clone)]
    struct Ctx {
        calls: Calls,
        evil_base: String,
    }

    async fn log(calls: &Calls, value: &str) {
        calls.lock().await.push(value.to_string());
    }

    async fn serve(app: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    async fn duck_html(State(ctx): State<Ctx>) -> Html<&'static str> {
        log(&ctx.calls, "duck-ok").await;
        Html(
            r#"<div class="result__body">
              <a class="result__a" href="https://example.com/duck">Duck fixture</a>
              <a class="result__snippet" href="https://example.com/duck">duck content</a>
            </div>"#,
        )
    }

    async fn yahoo_ok(State(ctx): State<Ctx>) -> Html<&'static str> {
        log(&ctx.calls, "yahoo").await;
        Html(
            r#"<ol><li><a data-matarget="algo" href="https://example.com/yahoo"><h3>Yahoo fixture</h3></a><p>yahoo content</p></li></ol>"#,
        )
    }

    async fn redirect_to(ctx: &Ctx, label: &str, location: String) -> impl IntoResponse {
        log(&ctx.calls, label).await;
        (
            StatusCode::FOUND,
            [(header::LOCATION, HeaderValue::from_str(&location).unwrap())],
        )
    }

    async fn duck_moved(State(ctx): State<Ctx>) -> impl IntoResponse {
        redirect_to(&ctx, "duck-moved", "/duck-ok".to_string()).await
    }

    async fn duck_evil(State(ctx): State<Ctx>) -> impl IntoResponse {
        let target = format!("{}/steal", ctx.evil_base);
        redirect_to(&ctx, "duck-evil", target).await
    }

    async fn hop_a(State(ctx): State<Ctx>) -> impl IntoResponse {
        redirect_to(&ctx, "hop-a", "/hop-b".to_string()).await
    }

    async fn hop_b(State(ctx): State<Ctx>) -> impl IntoResponse {
        redirect_to(&ctx, "hop-b", "/hop-c".to_string()).await
    }

    async fn hop_c(State(ctx): State<Ctx>) -> impl IntoResponse {
        redirect_to(&ctx, "hop-c", "/hop-d".to_string()).await
    }

    async fn stolen(State(calls): State<Calls>) -> &'static str {
        log(&calls, "stolen").await;
        "<html>pwned</html>"
    }

    /// Primary fixture server plus an "attacker" twin on a separate loopback
    /// port; the attacker must never receive a redirected request.
    async fn spawn_pair() -> (String, String, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));

        let evil = serve(
            Router::new()
                .route("/steal", get(stolen))
                .with_state(calls.clone()),
        )
        .await;

        let ctx = Ctx {
            calls: calls.clone(),
            evil_base: evil.clone(),
        };
        let primary = serve(
            Router::new()
                .route("/duck-ok", get(duck_html))
                .route("/yahoo-ok", get(yahoo_ok))
                .route("/duck-moved", get(duck_moved))
                .route("/duck-evil", get(duck_evil))
                .route("/hop-a", get(hop_a))
                .route("/hop-b", get(hop_b))
                .route("/hop-c", get(hop_c))
                .route("/hop-d", get(duck_html))
                .with_state(ctx),
        )
        .await;

        (primary, evil, calls)
    }

    fn ddg_config(base: &str, path: &str) -> BridgeConfig {
        let mut config = super::make_test_config();
        config.search.request_timeout = std::time::Duration::from_secs(5);
        config.search.duckduckgo_url = format!("{base}{path}");
        config.search.yahoo_url = format!("{base}/yahoo-ok");
        config
    }

    #[tokio::test]
    async fn same_host_redirect_is_followed() {
        let (primary, _evil, calls) = spawn_pair().await;
        let config = ddg_config(&primary, "/duck-moved");
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(results[0].title, "Duck fixture");
        assert_eq!(*calls.lock().await, vec!["duck-moved", "duck-ok"]);
    }

    #[tokio::test]
    async fn offhost_redirect_is_blocked_and_falls_through() {
        let (primary, _evil, calls) = spawn_pair().await;
        let config = ddg_config(&primary, "/duck-evil");
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        // Chain continues to Yahoo after the blocked bounce.
        assert_eq!(results[0].title, "Yahoo fixture");
        assert_eq!(*calls.lock().await, vec!["duck-evil", "yahoo"]);
        assert!(
            !calls.lock().await.iter().any(|entry| entry == "stolen"),
            "attacker host must never be contacted"
        );
    }

    #[tokio::test]
    async fn redirect_hop_limit_stops_long_chains() {
        let (primary, _evil, calls) = spawn_pair().await;
        let config = ddg_config(&primary, "/hop-a");
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        // Three hops reach the cap; hop-d is never requested and the chain
        // falls through to Yahoo.
        assert_eq!(results[0].title, "Yahoo fixture");
        assert_eq!(
            *calls.lock().await,
            vec!["hop-a", "hop-b", "hop-c", "yahoo"]
        );
    }
}

#[cfg(test)]
mod scraper_upgrade_port_fixtures {
    //! Port discipline for the http->https upgrade carve-out in
    //! `scraper_redirect_stays_on_origin`. `reqwest::redirect::Attempt` has
    //! no public constructor, so the rule is exercised through its pure
    //! predicate; the live-server tests above guard the end-to-end behavior.

    use super::providers::scraper_redirect_stays_on_origin;

    fn hop(from: &str, to: &str) -> bool {
        scraper_redirect_stays_on_origin(
            &reqwest::Url::parse(from).unwrap(),
            &reqwest::Url::parse(to).unwrap(),
        )
    }

    #[test]
    fn canonical_upgrade_moves_between_default_ports_only() {
        // 80 -> 443 (explicit or implied) is the one sanctioned port move.
        assert!(hop("http://search.example/a", "https://search.example/b"));
        assert!(hop(
            "http://search.example:80/a",
            "https://search.example/b"
        ));
        assert!(hop(
            "http://search.example/a",
            "https://search.example:443/b"
        ));
    }

    #[test]
    fn upgrade_with_unchanged_port_is_followed() {
        // Scheme upgrade only: the same explicit port survives the hop.
        assert!(hop(
            "http://search.example:8443/a",
            "https://search.example:8443/b"
        ));
    }

    #[test]
    fn upgrade_to_a_foreign_port_is_blocked() {
        // An upgrade must never relocate the scraper onto an arbitrary TLS
        // port of the same host — that was exactly the skipped-port bug.
        assert!(!hop(
            "http://search.example:80/a",
            "https://search.example:8443/b"
        ));
        assert!(!hop(
            "http://search.example:8080/a",
            "https://search.example:9443/b"
        ));
        // A non-default port may not masquerade as an upgrade to the default.
        assert!(!hop(
            "http://search.example:8080/a",
            "https://search.example/b"
        ));
    }
}

#[cfg(test)]
mod provider_http_fixtures {
    use super::*;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
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

    async fn searx_ok(State(calls): State<Calls>, headers: HeaderMap) -> Json<serde_json::Value> {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        log(&calls, &format!("searxng-auth:{auth:?}")).await;
        Json(serde_json::json!({"results":[{
            "title":"SearXNG fixture",
            "url":"https://example.com/searxng",
            "content":"searx content"
        }]}))
    }

    async fn tavily_truncated(State(calls): State<Calls>) -> &'static str {
        log(&calls, "tavily-truncated").await;
        r#"{"results":[{"title":"trun"#
    }

    async fn yahoo_blank(State(calls): State<Calls>) -> &'static str {
        log(&calls, "yahoo-blank").await;
        "<html>nothing useful here</html>"
    }

    async fn duck_blank(State(calls): State<Calls>) -> &'static str {
        log(&calls, "duck-blank").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        "<html>no anchors at all</html>"
    }

    async fn searx_slow(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "searx-slow").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Json(serde_json::json!({"results": []}))
    }

    async fn tavily_slow_empty(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "tavily-slow-empty").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Json(serde_json::json!({"results": []}))
    }

    async fn exa_slow_empty(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "exa-slow-empty").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Json(serde_json::json!({"results": []}))
    }

    async fn serper_slow_empty(State(calls): State<Calls>) -> Json<serde_json::Value> {
        log(&calls, "serper-slow-empty").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Json(serde_json::json!({"results": []}))
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

    async fn duck_captcha(State(calls): State<Calls>) -> Html<&'static str> {
        log(&calls, "duck-captcha").await;
        Html(r#"<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>"#)
    }

    async fn yahoo_ok(State(calls): State<Calls>) -> Html<&'static str> {
        log(&calls, "yahoo").await;
        Html(
            r#"<ol><li><div class="dd algo algo-sr"><div class="compTitle"><a data-matarget="algo" href="https://r.search.yahoo.com/x/RU=https%3a%2f%2fexample.com%2fyahoo/RK=2/RS=x"><h3><span>Yahoo fixture</span></h3></a></div><div class="compText aAbs"><p>yahoo content</p></div></div></li></ol>"#,
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
            .route("/tavily-truncated", post(tavily_truncated))
            .route("/tavily-slow-empty", post(tavily_slow_empty))
            .route("/exa-ok", post(exa_ok))
            .route("/exa-slow-empty", post(exa_slow_empty))
            .route("/serper-ok", post(serper_ok))
            .route("/serper-slow-empty", post(serper_slow_empty))
            .route("/searx/search", get(searx_ok))
            .route("/searx-slow/search", get(searx_slow))
            .route("/duck-ok", get(duck_ok))
            .route("/duck-captcha", get(duck_captcha))
            .route("/duck-blank", get(duck_blank))
            .route("/yahoo-ok", get(yahoo_ok))
            .route("/yahoo-blank", get(yahoo_blank))
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
        config.search.yahoo_url = format!("{base}/yahoo-ok");
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
    async fn duckduckgo_captcha_falls_back_to_yahoo() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.search.duckduckgo_url = format!("{base}/duck-captcha");
        let metrics = Arc::new(crate::observability::Metrics::default());
        let results = SearchClient::new_with_metrics(Client::new(), &config, metrics.clone())
            .search_results("claude security")
            .await
            .unwrap();
        assert_eq!(results[0].title, "Yahoo fixture");
        assert_eq!(results[0].url, "https://example.com/yahoo");
        assert_eq!(*calls.lock().await, vec!["duck-captcha", "yahoo"]);
        // Yahoo outcomes must be attributed to Yahoo, not folded into DuckDuckGo.
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.search_yahoo.successes, 1);
        assert_eq!(snapshot.search_duckduckgo.no_results, 1);
        assert_eq!(snapshot.search_duckduckgo.successes, 0);
    }

    #[tokio::test]
    async fn fallback_order_moves_from_failed_tavily_to_exa() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.tavily_api_key = Some("key".into());
        config.exa_api_key = Some("key".into());
        config.search.tavily_url = format!("{base}/tavily-fail");
        let metrics = Arc::new(crate::observability::Metrics::default());
        let results = SearchClient::new_with_metrics(Client::new(), &config, metrics.clone())
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(results[0].title, "Exa fixture");
        assert_eq!(*calls.lock().await, vec!["tavily-fail", "exa"]);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.search_tavily.failures, 1);
        assert_eq!(snapshot.search_exa.successes, 1);
    }

    #[tokio::test]
    async fn timeout_and_response_size_limits_are_typed() {
        let (base, _calls) = spawn().await;
        let mut timeout = base_config(&base);
        timeout.search.duckduckgo_url = format!("{base}/slow");
        timeout.search.yahoo_url = format!("{base}/slow");
        timeout.search.request_timeout = Duration::from_millis(20);
        let error = SearchClient::new(Client::new(), &timeout)
            .search_results("rust")
            .await
            .unwrap_err();
        assert_eq!(error.kind, SearchErrorKind::Timeout);

        let mut oversized_config = base_config(&base);
        oversized_config.search.duckduckgo_url = format!("{base}/oversized");
        oversized_config.search.yahoo_url = format!("{base}/oversized");
        oversized_config.search.max_response_bytes = 1024;
        let error = SearchClient::new(Client::new(), &oversized_config)
            .search_results("rust")
            .await
            .unwrap_err();
        assert_eq!(error.kind, SearchErrorKind::ResponseTooLarge);
    }

    #[tokio::test]
    async fn chain_budget_cuts_slow_providers_before_all_are_tried() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        // Five configured providers that each stall 200ms before answering
        // with empty results: without a budget this serialises to ~1s and
        // ends in NoResults.
        config.tavily_api_key = Some("key".into());
        config.exa_api_key = Some("key".into());
        config.serper_api_key = Some("key".into());
        config.searxng_url = Some(format!("{base}/searx-slow"));
        config.search.tavily_url = format!("{base}/tavily-slow-empty");
        config.search.exa_url = format!("{base}/exa-slow-empty");
        config.search.serper_url = format!("{base}/serper-slow-empty");
        config.search.duckduckgo_url = format!("{base}/duck-blank");
        config.search.yahoo_url = format!("{base}/yahoo-blank");
        config.search.request_timeout = Duration::from_secs(5);

        let mut search_client = SearchClient::new(Client::new(), &config);
        // Policy-level budget knob (not yet config-plumbed): 500ms against
        // five providers stalling ~200ms each.
        search_client.policy.chain_budget = Duration::from_millis(500);

        let started = std::time::Instant::now();
        let error = search_client.search_results("rust").await.unwrap_err();
        let elapsed = started.elapsed();

        // Budget exhaustion is its own outcome, distinct from NoResults.
        assert_eq!(error.kind, SearchErrorKind::BudgetExhausted);
        assert!(
            elapsed < Duration::from_millis(1100),
            "chain took {elapsed:?}; budget did not cut the serial walk"
        );
        let tried = calls.lock().await.len();
        assert!(
            tried < 6,
            "expected some providers to be skipped, tried {tried}"
        );
        assert!(
            tried >= 1,
            "at least one provider should have been attempted"
        );
    }

    #[tokio::test]
    async fn malformed_json_from_provider_falls_through_to_next() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.tavily_api_key = Some("key".into());
        config.exa_api_key = Some("key".into());
        config.search.tavily_url = format!("{base}/tavily-truncated");
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(results[0].title, "Exa fixture");
        assert_eq!(*calls.lock().await, vec!["tavily-truncated", "exa"]);
    }

    #[tokio::test]
    async fn keyless_chain_with_only_empty_results_reports_no_results_kind() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.search.duckduckgo_url = format!("{base}/duck-captcha");
        config.search.yahoo_url = format!("{base}/yahoo-blank");
        let error = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap_err();
        // Empty result sets are distinct from transport/HTTP failures.
        assert_eq!(error.kind, SearchErrorKind::NoResults);
        assert_eq!(*calls.lock().await, vec!["duck-captcha", "yahoo-blank"]);
    }

    #[tokio::test]
    async fn searxng_credential_is_sent_as_bearer_header_when_configured() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.searxng_url = Some(format!("{base}/searx"));
        config.searxng_api_key = Some("s3cr3t-key".into());
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(results[0].title, "SearXNG fixture");
        assert_eq!(
            *calls.lock().await,
            vec![r#"searxng-auth:Some("Bearer s3cr3t-key")"#]
        );
    }

    #[tokio::test]
    async fn searxng_without_credential_sends_no_authorization_header() {
        let (base, calls) = spawn().await;
        let mut config = base_config(&base);
        config.searxng_url = Some(format!("{base}/searx"));
        let results = SearchClient::new(Client::new(), &config)
            .search_results("rust")
            .await
            .unwrap();
        assert_eq!(results[0].title, "SearXNG fixture");
        assert_eq!(*calls.lock().await, vec!["searxng-auth:None"]);
    }

    #[tokio::test]
    async fn control_characters_in_api_keys_never_panic_the_request() {
        let (base, _calls) = spawn().await;

        let mut exa = base_config(&base);
        exa.exa_api_key = Some("bad\nkey".into());
        let mut serper = base_config(&base);
        serper.serper_api_key = Some("bad\rkey".into());
        let mut searxng = base_config(&base);
        searxng.searxng_url = Some(format!("{base}/searx"));
        searxng.searxng_api_key = Some("bad\nkey".into());

        for (label, config) in [("exa", exa), ("serper", serper), ("searxng", searxng)] {
            // The request must complete (invalid credential header skipped) —
            // never panic on a hostile/misconfigured credential value.
            let results = SearchClient::new(Client::new(), &config)
                .search_results("rust")
                .await
                .unwrap_or_else(|error| {
                    panic!("{label} failed instead of skipping header: {error}")
                });
            assert!(!results.is_empty(), "{label} returned no results");
        }
    }
}
