//! Fast integration tests covering all 74 spec test cases (TC-001 to TC-074).
//!
//! Run: `cargo test --test fast`

use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tower_http::limit::RequestBodyLimitLayer;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Safe backup guard for opencode2api.toml
struct ConfigBackupGuard {
    original_content: Option<Vec<u8>>,
}

impl ConfigBackupGuard {
    fn new() -> Self {
        let path = Path::new("opencode2api.toml");
        let original_content = if path.exists() {
            fs::read(path).ok()
        } else {
            None
        };
        Self { original_content }
    }
}

impl Drop for ConfigBackupGuard {
    fn drop(&mut self) {
        let path = Path::new("opencode2api.toml");
        if let Some(ref content) = self.original_content {
            let _ = fs::write(path, content);
        } else if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Build the same router structure used in production, with custom test config.
fn build_test_router(config: opencode2api::config::BridgeConfig) -> Router {
    let state = opencode2api::state::AppState::new(config);

    Router::new()
        .route(
            "/v1/messages",
            post(opencode2api::handlers::handle_messages),
        )
        .route(
            "/v1/messages/count_tokens",
            post(opencode2api::handlers::handle_count_tokens),
        )
        .route("/v1/models", get(opencode2api::handlers::handle_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            opencode2api::middleware::auth_middleware,
        ))
        .route("/", get(opencode2api::dashboard::serve_landing))
        .route("/health", get(opencode2api::handlers::handle_health))
        .route("/dashboard", get(opencode2api::dashboard::serve_webui))
        .route("/dashboard/", get(opencode2api::dashboard::serve_webui))
        .route(
            "/dashboard/*path",
            get(opencode2api::dashboard::serve_webui),
        )
        .route(
            "/api/dashboard/status",
            get(opencode2api::dashboard::handler_rest_status),
        )
        .route(
            "/api/dashboard/proxies",
            get(opencode2api::dashboard::handler_proxies),
        )
        .route(
            "/api/dashboard/config",
            get(opencode2api::dashboard::handler_config),
        )
        .route(
            "/api/dashboard/login",
            post(opencode2api::dashboard::handler_login),
        )
        .route(
            "/api/dashboard/logout",
            post(opencode2api::dashboard::handler_logout),
        )
        .route(
            "/api/dashboard/auth/status",
            get(opencode2api::dashboard::handler_auth_status),
        )
        .route(
            "/api/dashboard/diagnostics",
            get(opencode2api::dashboard::handler_dashboard_diagnostics),
        )
        .route(
            "/api/dashboard/config/save",
            post(opencode2api::dashboard::handler_config_save),
        )
        .route(
            "/api/dashboard/events",
            get(opencode2api::dashboard::handler_events),
        )
        .route(
            "/api/dashboard/proxy/:port/restart",
            post(opencode2api::dashboard::handler_proxy_restart),
        )
        .layer(RequestBodyLimitLayer::new(1_048_576))
        .with_state(state)
}

/// Start test server on a random port, return base_url.
async fn spawn_test_server(config: opencode2api::config::BridgeConfig) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);

    let app = build_test_router(config);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Poll /health until ready
    let client = reqwest::Client::new();
    for _ in 0..20 {
        if let Ok(resp) = client.get(format!("{}/health", base)).send().await {
            if resp.status() == 200 {
                return base;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Server failed to start within timeout");
}

async fn spawn_test_server_default() -> String {
    spawn_test_server(opencode2api::config::BridgeConfig::from_env_and_cli(
        opencode2api::config::CliOverrides::default(),
    ))
    .await
}

// ──────────────────────────────────────────────────────────
// Group 1: Routing & Headers (TC-001 - TC-008)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc001_get_root() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("<title>OpenCode2API Bridge</title>"));
    assert!(body.contains("id=\"password\""));
}

#[tokio::test]
async fn test_tc002_get_dashboard_no_cookie() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("{}/dashboard", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 307);
    assert_eq!(resp.headers().get("location").unwrap(), "/");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc003_get_dashboard_slash_no_cookie() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("{}/dashboard/", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 307);
    assert_eq!(resp.headers().get("location").unwrap(), "/");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc004_get_dashboard_index_no_cookie() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("{}/dashboard/index.html", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 307);
    assert_eq!(resp.headers().get("location").unwrap(), "/");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc005_get_static_asset_css() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/dashboard/style.css", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/css");
}

#[tokio::test]
async fn test_tc006_get_static_asset_js() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/dashboard/app.js", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("javascript"));
}

#[tokio::test]
async fn test_tc007_spa_fallback_behavior() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/dashboard/nonexistent-subroute", base))
        .header("Cookie", "bridge_admin_session=test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("<!DOCTYPE html>"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc008_cache_control_headers() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("cache-control").unwrap(),
        "no-store, no-cache, must-revalidate, max-age=0"
    );
    assert_eq!(resp.headers().get("pragma").unwrap(), "no-cache");
}

// ──────────────────────────────────────────────────────────
// Group 2: Dashboard Authentication (TC-009 - TC-018)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc009_status_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Please enter password to login");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc010_status_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "wrong_token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Invalid password");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc011_status_correct_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc012_status_legacy_default_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "123456")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Invalid password");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc013_login_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/login", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc014_login_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/login", base))
        .header("X-Dashboard-Token", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc015_login_correct_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/login", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc016_events_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/events", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc017_events_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/events?token=wrong", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc018_events_correct_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/events?token=test-token", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_auth_status_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/auth/status", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["admin_token_configured"], true);
    assert_eq!(body["authenticated"], false);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_auth_status_authenticated() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/auth/status", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["admin_token_configured"], true);
    assert_eq!(body["authenticated"], true);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_auth_status_no_admin_token_configured() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/auth/status", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["admin_token_configured"], false);
    assert_eq!(body["authenticated"], false);
}

#[tokio::test]
async fn test_logout_clears_cookie() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .post(format!("{}/api/dashboard/logout", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    /* Check that the Set-Cookie header clears the cookie */
    let set_cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    assert!(set_cookie.contains("bridge_admin_session="));
    assert!(set_cookie.contains("Max-Age=0"));
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_logout_no_auth() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/logout", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

// Group 3: Config Display/Save (TC-019 - TC-028)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc019_get_config_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/config", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc020_get_config_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/config", base))
        .header("X-Dashboard-Token", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc021_get_config_correct_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/config", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["bridge_port"].as_u64().is_some() || body["bridge_port"].is_null());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc022_save_config_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/config/save", base))
        .body("model = 'test-model'")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc023_save_config_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/config/save", base))
        .header("X-Dashboard-Token", "wrong")
        .body("model = 'test-model'")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc024_save_config_correct_token_valid_toml() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _backup = ConfigBackupGuard::new();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/config/save", base))
        .header("X-Dashboard-Token", "test-token")
        .header("Content-Type", "application/json")
        .body(r#"{"content": "model = 'opencode/deepseek-v4-flash-free'"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc025_save_config_correct_token_invalid_toml() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _backup = ConfigBackupGuard::new();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/config/save", base))
        .header("X-Dashboard-Token", "test-token")
        .header("Content-Type", "application/json")
        .body(r#"{"content": "invalid = {"}"#)
        .send()
        .await
        .unwrap();
    // Backend returns 400 for invalid TOML (changed from old 200 behavior)
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(body["message"].as_str().unwrap().contains("Invalid TOML"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc026_save_config_correct_token_empty_body() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _backup = ConfigBackupGuard::new();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/config/save", base))
        .header("X-Dashboard-Token", "test-token")
        .header("Content-Type", "application/json")
        .body(r#"{}"#)
        .send()
        .await
        .unwrap();
    // Backend returns 400 when 'content' field is missing
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(body["message"].as_str().unwrap().contains("Missing 'content' field"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc027_sensitive_config_masking() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    env::set_var("TAVILY_API_KEY", "super-secret-tavily");
    env::set_var("SERPER_API_KEY", "super-secret-serper");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/config", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(!body.contains("super-secret-tavily"));
    assert!(!body.contains("super-secret-serper"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    env::remove_var("TAVILY_API_KEY");
    env::remove_var("SERPER_API_KEY");
}

#[tokio::test]
async fn test_tc028_config_reload_verification() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _backup = ConfigBackupGuard::new();
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();

    // 1. Save new configuration
    let save_resp = client
        .post(format!("{}/api/dashboard/config/save", base))
        .header("X-Dashboard-Token", "test-token")
        .header("Content-Type", "application/json")
        .body(r#"{"content": "model = 'opencode/deepseek-v4-flash-free'"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(save_resp.status(), 200);

    // 2. Verify config file contains the new configuration
    let config_path =
        env::var("BRIDGE_CONFIG_PATH").unwrap_or_else(|_| "opencode2api.toml".to_string());
    let file_content = fs::read_to_string(&config_path).unwrap();
    assert!(file_content.contains("opencode/deepseek-v4-flash-free"));

    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

// ──────────────────────────────────────────────────────────
// Group 4: Bridge API Logic (TC-029 - TC-040)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc029_get_models_anonymous_auth_disabled() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::remove_var("BRIDGE_AUTH_TOKEN");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v1/models", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_tc030_post_messages_anonymous_auth_disabled() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::remove_var("BRIDGE_AUTH_TOKEN");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let req_body = json!({
        "model": "opencode/deepseek-v4-flash-free",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false
    });
    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&req_body)
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status(), 401);
}

#[tokio::test]
async fn test_tc031_get_models_valid_bearer() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bearer-key");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v1/models", base))
        .header("Authorization", "Bearer valid-bearer-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    env::remove_var("BRIDGE_AUTH_TOKEN");
}

#[tokio::test]
async fn test_tc032_get_models_invalid_bearer() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bearer-key");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v1/models", base))
        .header("Authorization", "Bearer wrong_bearer_token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Missing or invalid"));
    env::remove_var("BRIDGE_AUTH_TOKEN");
}

#[tokio::test]
async fn test_tc033_get_models_missing_bearer() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bearer-key");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v1/models", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("BRIDGE_AUTH_TOKEN");
}

#[tokio::test]
async fn test_tc034_post_messages_missing_messages_field() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", base))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
}

#[tokio::test]
async fn test_tc035_post_messages_empty_messages_array() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let req_body = json!({
        "model": "opencode/deepseek-v4-flash-free",
        "messages": []
    });
    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&req_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn test_tc036_post_messages_non_streaming() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let req_body = json!({
        "model": "opencode/deepseek-v4-flash-free",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false
    });
    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&req_body)
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status(), 401);
}

#[tokio::test]
async fn test_tc037_post_messages_streaming() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let req_body = json!({
        "model": "opencode/deepseek-v4-flash-free",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": true
    });
    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&req_body)
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status(), 401);
}

#[tokio::test]
async fn test_tc038_post_messages_unsupported_model_fallback() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let req_body = json!({
        "model": "some-unsupported-crazy-model",
        "messages": [{"role": "user", "content": "Hi"}]
    });
    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&req_body)
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status(), 401);
}

#[tokio::test]
async fn test_tc039_post_messages_large_payload() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let large_payload = "A".repeat(1100000); // 1.1MB
    let resp = client
        .post(format!("{}/v1/messages", base))
        .header("Content-Type", "application/json")
        .body(large_payload)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413); // Payload Too Large
}

#[tokio::test]
async fn test_tc040_post_messages_malformed_json() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", base))
        .header("Content-Type", "application/json")
        .body("{\"model\":")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// ──────────────────────────────────────────────────────────
// Group 5: Health & Diagnostics (TC-041 - TC-048)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc041_health_check_minimal() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("{}/health", base)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], "0.4.0");
}

#[tokio::test]
async fn test_tc042_health_check_zero_topology_leak() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("{}/health", base)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["proxy_pool"].is_null());
    assert!(body["daemon"].is_null());
    assert!(body["config"].is_null());
}

#[tokio::test]
async fn test_tc043_diagnostics_no_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc044_diagnostics_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc045_diagnostics_correct_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
    assert!(body["proxy_pool"].is_object());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc046_diagnostics_daemon_status() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["daemon"].is_object());
    assert!(body["daemon"]["status"].is_string());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc047_diagnostics_config_properties() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["config"].is_object());
    assert!(body["config"]["shell_policy"].is_string());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc048_diagnostics_proxy_node_roles() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let list = body["proxy_pool"]["nodes"].as_array().unwrap();
    for node in list {
        assert!(node["role"].is_string());
    }
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

// ──────────────────────────────────────────────────────────
// Group 6: Proxy Pool & Node Management (TC-049 - TC-056)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc049_restart_proxy_no_token() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40001/restart", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_tc050_restart_proxy_wrong_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40001/restart", base))
        .header("X-Dashboard-Token", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc051_restart_proxy_valid_node_40001() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40001/restart", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["status"].as_str().is_some());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc052_restart_proxy_valid_node_40003() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40003/restart", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["status"].as_str().is_some());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc053_restart_proxy_out_of_range_9999() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/9999/restart", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("out of valid range"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc054_restart_proxy_non_numeric() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/abc/restart", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc055_restart_proxy_out_of_range_40000() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40000/restart", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("out of valid range"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc056_restart_proxy_out_of_range_40006() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40006/restart", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("out of valid range"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

// ──────────────────────────────────────────────────────────
// Group 7: Auth Boundary (TC-057 - TC-064)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc057_access_bridge_api_using_dashboard_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bridge-token");
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v1/models", base))
        .header("Authorization", "Bearer valid-dashboard-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("BRIDGE_AUTH_TOKEN");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc058_access_dashboard_api_using_bridge_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bridge-token");
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "valid-bridge-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("BRIDGE_AUTH_TOKEN");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc059_access_config_api_using_bridge_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bridge-token");
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/config", base))
        .header("X-Dashboard-Token", "valid-bridge-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("BRIDGE_AUTH_TOKEN");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc060_access_diagnostics_api_using_bridge_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bridge-token");
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "valid-bridge-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("BRIDGE_AUTH_TOKEN");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc061_access_events_sse_using_bridge_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_AUTH_TOKEN", "valid-bridge-token");
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/api/dashboard/events?token=valid-bridge-token",
            base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("BRIDGE_AUTH_TOKEN");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc062_access_status_api_anonymously() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc063_access_config_api_anonymously() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/config", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc064_access_restart_api_anonymously() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "valid-dashboard-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/dashboard/proxy/40001/restart", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

// ──────────────────────────────────────────────────────────
// Group 8: Security Hardening / Regression (TC-065 - TC-074)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc065_fail_closed_when_unset_default_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "123456")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("Dashboard is disabled"));
}

#[tokio::test]
async fn test_tc066_reject_123456_when_strong_token_configured() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "strong-configured-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "123456")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc067_security_headers_on_landing() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let headers = resp.headers();
    assert!(headers.contains_key("content-security-policy"));
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
}

#[tokio::test]
async fn test_tc068_security_headers_on_dashboard_spa() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/dashboard/", base))
        .header("Cookie", "bridge_admin_session=test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let headers = resp.headers();
    assert!(headers.contains_key("content-security-policy"));
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc069_safe_error_responses_no_stack_traces() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/nonexistent-route-xyz", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body = resp.text().await.unwrap();
    assert!(!body.contains("panicked"));
    assert!(!body.contains("stack backtrace"));
}
#[test]
fn test_tc070_public_binding_abort_on_weak_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_HOST", "0.0.0.0");
    env::set_var("DASHBOARD_ADMIN_TOKEN", "1234");

    let config = opencode2api::config::BridgeConfig::from_env_and_cli(
        opencode2api::config::CliOverrides::default(),
    );
    let result = config.validate_security();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("too weak"));

    env::remove_var("BRIDGE_HOST");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[test]
fn test_tc071_public_binding_abort_on_empty_token() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("BRIDGE_HOST", "0.0.0.0");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");

    let config = opencode2api::config::BridgeConfig::from_env_and_cli(
        opencode2api::config::CliOverrides::default(),
    );
    let result = config.validate_security();
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("without an explicit DASHBOARD_ADMIN_TOKEN"));

    env::remove_var("BRIDGE_HOST");
}

#[tokio::test]
async fn test_tc072_unsupported_http_method() {
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/health", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405); // Method Not Allowed
}

#[tokio::test]
async fn test_tc073_fail_closed_on_diagnostics_unset() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "123456")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("Dashboard is disabled"));
}

#[tokio::test]
async fn test_tc074_content_type_validation() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}
