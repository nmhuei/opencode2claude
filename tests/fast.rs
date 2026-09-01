//! Fast integration tests covering dashboard/API regression cases.
//!
//! Run: `cargo test --test fast`

use axum::Router;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::sleep;

static ENV_MUTEX: Mutex<()> = Mutex::const_new(());

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

/// Build the production router with custom test configuration.
fn build_test_router(config: opencode2api::config::BridgeConfig) -> Router {
    let state = opencode2api::state::AppState::new(config);
    opencode2api::server::build_router(state)
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
    // Isolate in-process config resolution from the surrounding machine.
    // Shells (and the repository `.env`) frequently export BRIDGE_CONFIG_PATH
    // pointing at a real deployment; its sibling `<stem>.api-keys.json`
    // registry would then report configured()==true and every anonymous
    // request would 401. Point discovery at a unique throwaway path and keep
    // request-history out of $HOME/.opencode2api. The snapshot window is tiny
    // and every possible interleaved value is equally hermetic, so racing
    // with concurrent spawns is benign.
    let isolation_dir = std::env::temp_dir().join(format!(
        "opencode2api-fast-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let config_path = isolation_dir.join("config.toml");
    let previous = [
        ("BRIDGE_CONFIG_PATH", env::var("BRIDGE_CONFIG_PATH").ok()),
        ("RUNTIME_DIR", env::var("RUNTIME_DIR").ok()),
        ("OPENCODE_MODEL", env::var("OPENCODE_MODEL").ok()),
    ];
    env::set_var("BRIDGE_CONFIG_PATH", &config_path);
    env::set_var("RUNTIME_DIR", &isolation_dir);
    env::remove_var("OPENCODE_MODEL");
    let mut config = opencode2api::config::BridgeConfig::from_env_and_cli(
        opencode2api::config::CliOverrides::default(),
    );
    for (name, value) in previous {
        match value {
            Some(restored) => env::set_var(name, restored),
            None => env::remove_var(name),
        }
    }
    config.max_body_size = 1_048_576;
    spawn_test_server(config).await
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.contains("bridge_admin_session="));
    assert!(set_cookie.contains("Max-Age=0"));
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_logout_no_auth() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    assert!(body["max_body_size"].as_u64().is_some());
    assert!(body["stream_buffer_size"].as_u64().is_some());
    assert!(body["channel_capacity"].as_u64().is_some());
    assert!(body["max_search_loops"].as_u64().is_some());
    assert!(body["primary_proxies"].is_array());
    assert!(body["warm_standby_proxies"].is_array());
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc022_save_config_no_token() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("Missing 'content' field"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc027_sensitive_config_masking() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let save_body: Value = save_resp.json().await.unwrap();

    // 2. Verify the exact config path reported by the save operation.
    // This keeps the test aligned with config resolution, where an empty
    // BRIDGE_CONFIG_PATH is intentionally treated as unset.
    let config_path = save_body["path"]
        .as_str()
        .expect("config save response should include the written path");
    let file_content = fs::read_to_string(config_path).unwrap();
    assert!(file_content.contains("opencode/deepseek-v4-flash-free"));

    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

// ──────────────────────────────────────────────────────────
// Group 4: Bridge API Logic (TC-029 - TC-040)
// ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tc029_get_models_anonymous_auth_disabled() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", base))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    // Anthropic error shape (BridgeError::InvalidRequest), not axum's default 422.
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
}

#[tokio::test]
async fn test_tc035_post_messages_empty_messages_array() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
async fn test_dashboard_test_stream_no_token() {
    let _lock = ENV_MUTEX.lock().await;
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/dashboard/test/stream", base))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_dashboard_test_stream_emits_thinking_and_text() {
    let _lock = ENV_MUTEX.lock().await;
    env::set_var("DASHBOARD_ADMIN_TOKEN", "test-token");
    let base = spawn_test_server_default().await;
    let client = reqwest::Client::new();

    let body = client
        .get(format!(
            "{}/api/dashboard/test/stream?token=test-token&delay_ms=0",
            base
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("event: message_start"));
    assert!(body.contains("event: content_block_start"));
    assert!(body.contains("thinking_delta"));
    assert!(body.contains("text_delta"));
    assert!(body.contains("event: message_stop"));
    env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_tc062_access_status_api_anonymously() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
#[tokio::test]
async fn test_tc070_public_binding_abort_on_weak_token() {
    let _lock = ENV_MUTEX.lock().await;
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

#[tokio::test]
async fn test_tc071_public_binding_abort_on_empty_token() {
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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
    let _lock = ENV_MUTEX.lock().await;
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

// ──────────────────────────────────────────────────────────
// Dashboard redesign control-plane contract
// ──────────────────────────────────────────────────────────

fn dashboard_control_config(name: &str) -> opencode2api::config::BridgeConfig {
    let mut config = opencode2api::config::BridgeConfig {
        host: "127.0.0.1".parse().unwrap(),
        bridge_port: 0,
        primary_proxies: None,
        warm_standby_proxies: None,
        ..Default::default()
    };
    config.egress.identity_endpoints.clear();
    // Keep the request-history database in a throwaway directory instead of
    // the machine-wide $HOME/.opencode2api used by the deployed bridge.
    config.runtime.runtime_dir = Some(std::env::temp_dir().join(format!(
        "opencode2api-fast-runtime-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )));
    config.management.dashboard_token = Some("dashboard-control-secret".into());
    config.management.rest_api_token = Some("dashboard-control-rest".into());
    config.management.config_path = std::env::temp_dir().join(format!(
        "opencode2api-dashboard-control-{name}-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    config
}

fn dashboard_cookie() -> &'static str {
    "bridge_admin_session=dashboard-control-secret"
}

fn dashboard_mutation_cookie() -> &'static str {
    "bridge_admin_session=dashboard-control-secret; bridge_csrf_token=csrf-control"
}

#[tokio::test]
async fn dashboard_control_models_requires_session_and_returns_free_catalog() {
    let base = spawn_test_server(dashboard_control_config("models")).await;
    let client = reqwest::Client::new();
    let unauthorized = client
        .get(format!("{base}/api/dashboard/control/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);

    let response = client
        .get(format!("{base}/api/dashboard/control/models"))
        .header("Cookie", dashboard_cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert!(body["models"].as_array().unwrap().len() >= 6);
    assert!(body["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == "opencode/x-preview-f-free"));
}

#[tokio::test]
async fn dashboard_control_completion_and_template_are_text_responses() {
    let base = spawn_test_server(dashboard_control_config("text")).await;
    let client = reqwest::Client::new();
    let completion = client
        .get(format!("{base}/api/dashboard/control/completions/bash"))
        .header("Cookie", dashboard_cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(completion.status(), 200);
    assert!(completion.text().await.unwrap().contains("opencode2api"));

    let template = client
        .get(format!("{base}/api/dashboard/control/config/template"))
        .header("Cookie", dashboard_cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(template.status(), 200);
    let text = template.text().await.unwrap();
    assert!(text.contains("schema_version = 1"));
    assert!(text.contains("auth_tokens"));
}

#[tokio::test]
async fn dashboard_control_key_generation_requires_csrf_and_can_avoid_persistence() {
    let config = dashboard_control_config("keys");
    let path = config.management.config_path.clone();
    let base = spawn_test_server(config).await;
    let client = reqwest::Client::new();
    let payload = json!({
        "count": 2,
        "bytes": 16,
        "prefix": "sk-oc2-",
        "save": false,
        "replace": false
    });

    let forbidden = client
        .post(format!("{base}/api/dashboard/control/api-keys"))
        .header("Cookie", dashboard_cookie())
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);

    let response = client
        .post(format!("{base}/api/dashboard/control/api-keys"))
        .header("Cookie", dashboard_mutation_cookie())
        .header("X-CSRF-Token", "csrf-control")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let keys = body["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys
        .iter()
        .all(|key| key.as_str().unwrap().starts_with("sk-oc2-")));
    assert!(!path.exists(), "save=false must leave config untouched");
}

#[tokio::test]
async fn dashboard_config_preview_validates_without_writing() {
    let config = dashboard_control_config("preview");
    let path = config.management.config_path.clone();
    let base = spawn_test_server(config).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/api/dashboard/config/preview"))
        .header("Cookie", dashboard_cookie())
        .json(&json!({
            "content": "schema_version = 1\nmodel = \"opencode/deepseek-v4-flash-free\"\n"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["valid"], true);
    assert_eq!(body["restart_required"], true);
    assert!(!path.exists(), "preview must never create the config file");
}

#[tokio::test]
async fn dashboard_api_key_inventory_is_fingerprinted_and_revoke_preserves_one_key() {
    let mut config = dashboard_control_config("api-key-inventory");
    let path = config.management.config_path.clone();
    let first = "sk-oc2-11111111111111111111111111111111"; // EXAMPLE_SECRET_SCAN_ALLOW
    let second = "sk-oc2-22222222222222222222222222222222"; // EXAMPLE_SECRET_SCAN_ALLOW
    config.auth_tokens = Some(vec![first.into(), second.into()]);
    std::fs::write(
        &path,
        format!(
            "# preserve inventory comment\nschema_version = 1\nauth_tokens = [\"{first}\", \"{second}\"]\n"
        ),
    )
    .unwrap();
    let base = spawn_test_server(config).await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/api/dashboard/control/api-keys"))
        .header("Cookie", dashboard_cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body_text = response.text().await.unwrap();
    assert!(!body_text.contains(first));
    assert!(!body_text.contains(second));
    let body: Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["keys"].as_array().unwrap().len(), 2);
    assert_eq!(body["keys"][0]["active"], true);

    let revoked = client
        .post(format!("{base}/api/dashboard/control/api-keys/revoke"))
        .header("Cookie", dashboard_mutation_cookie())
        .header("X-CSRF-Token", "csrf-control")
        .json(&json!({"indices":[1]}))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 200);
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("# preserve inventory comment"));
    assert!(updated.contains(first));
    assert!(!updated.contains(second));

    let last_key = client
        .post(format!("{base}/api/dashboard/control/api-keys/revoke"))
        .header("Cookie", dashboard_mutation_cookie())
        .header("X-CSRF-Token", "csrf-control")
        .json(&json!({"indices":[0]}))
        .send()
        .await
        .unwrap();
    assert_eq!(last_key.status(), 409);
}

#[tokio::test]
async fn dashboard_client_config_defaults_to_placeholder_and_supports_explicit_active_key() {
    let mut config = dashboard_control_config("client-config");
    let active = "sk-oc2-active-secret-that-must-be-explicit"; // EXAMPLE_SECRET_SCAN_ALLOW
    config.auth_tokens = Some(vec![active.into()]);
    config.bridge_port = 4567;
    config.model = Some("opencode/deepseek-v4-flash-free".to_string());
    let base = spawn_test_server(config).await;
    let client = reqwest::Client::new();

    let placeholder = client
        .post(format!("{base}/api/dashboard/control/client-config"))
        .header("Cookie", dashboard_mutation_cookie())
        .header("X-CSRF-Token", "csrf-control")
        .json(&json!({"format":"claude-code","key_source":"placeholder"}))
        .send()
        .await
        .unwrap();
    assert_eq!(placeholder.status(), 200);
    let body: Value = placeholder.json().await.unwrap();
    assert_eq!(body["contains_secret"], false);
    assert_eq!(body["filename"], "claude-code-settings.json");
    assert!(!body["content"].as_str().unwrap().contains(active));
    let settings: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        settings["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:4567"
    );

    let explicit = client
        .post(format!("{base}/api/dashboard/control/client-config"))
        .header("Cookie", dashboard_mutation_cookie())
        .header("X-CSRF-Token", "csrf-control")
        .json(&json!({"format":"env","key_source":"active"}))
        .send()
        .await
        .unwrap();
    assert_eq!(explicit.status(), 200);
    let body: Value = explicit.json().await.unwrap();
    assert_eq!(body["contains_secret"], true);
    assert!(body["content"].as_str().unwrap().contains(active));
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("OPENAI_BASE_URL=\"http://127.0.0.1:4567/v1\""));
}
