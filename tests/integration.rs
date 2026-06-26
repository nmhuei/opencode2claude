//! Integration tests for the OpenCode2Claude bridge API.
//!
//! These tests spawn a real bridge server and test the HTTP API
//! endpoints including streaming SSE, non-streaming JSON, shell
//! command interception, health checks, and error handling.

use reqwest::Client;
use serde_json::Value;
use std::net::TcpListener;
use std::time::Duration;
use tokio::time::sleep;

/// Find a random available port for testing.
fn get_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Helper to build an Anthropic Messages API request body.
fn build_request(prompt: &str, stream: bool) -> Value {
    serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": prompt}],
        "stream": stream
    })
}

/// Helper to build a multi-content request body.
fn build_multi_content_request(parts: Vec<(&str, &str)>, stream: bool) -> Value {
    let content: Vec<Value> = parts
        .into_iter()
        .map(|(t, text)| serde_json::json!({"type": t, "text": text}))
        .collect();

    serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": content}],
        "stream": stream
    })
}

// ── Integration tests using the running bridge ──
// These tests require the bridge to be running on port 4000.
// Run with: cargo test --test integration -- --ignored

/// Test that the bridge binary can be spawned and responds on /health.
#[tokio::test]
async fn test_bridge_binary_health() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "unrestricted")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .env_remove("OPENCODE_MODEL")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary. Run `cargo build --release` first.");

    // Wait for server to start
    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/health", port);

    let resp = client.get(&url).send().await;
    assert!(resp.is_ok(), "Health endpoint should respond");

    let resp = resp.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");

    child.kill().await.ok();
}

/// Test non-streaming shell command via `!echo`.
#[tokio::test]
async fn test_shell_command_non_streaming() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "unrestricted")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .env_remove("OPENCODE_MODEL")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/messages", port);

    let resp = client
        .post(&url)
        .json(&build_request("!echo integration_test_123", false))
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();

    // Verify Anthropic response format
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["id"], "msg_local_shell");
    assert!(body["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("integration_test_123"));

    child.kill().await.ok();
}

/// Test streaming shell command returns proper SSE events.
#[tokio::test]
async fn test_shell_command_streaming_sse() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "unrestricted")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .env_remove("OPENCODE_MODEL")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/messages", port);

    let resp = client
        .post(&url)
        .json(&build_request("!echo sse_test_456", true))
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/event-stream"));

    let body = resp.text().await.unwrap();

    // Verify SSE event order
    assert!(
        body.contains("event: message_start"),
        "Should have message_start event"
    );
    assert!(
        body.contains("event: content_block_start"),
        "Should have content_block_start"
    );
    assert!(
        body.contains("event: content_block_delta"),
        "Should have content_block_delta"
    );
    assert!(
        body.contains("event: content_block_stop"),
        "Should have content_block_stop"
    );
    assert!(
        body.contains("event: message_delta"),
        "Should have message_delta"
    );
    assert!(
        body.contains("event: message_stop"),
        "Should have message_stop"
    );

    // Verify output content is in the stream
    assert!(
        body.contains("sse_test_456"),
        "Should contain command output"
    );

    // Verify event order (message_start before content, content before stop)
    let start_pos = body.find("event: message_start").unwrap();
    let block_start_pos = body.find("event: content_block_start").unwrap();
    let delta_pos = body.find("event: content_block_delta").unwrap();
    let block_stop_pos = body.find("event: content_block_stop").unwrap();
    let msg_delta_pos = body.find("event: message_delta").unwrap();
    let msg_stop_pos = body.find("event: message_stop").unwrap();

    assert!(
        start_pos < block_start_pos,
        "message_start should come before content_block_start"
    );
    assert!(
        block_start_pos < delta_pos,
        "content_block_start should come before delta"
    );
    assert!(
        delta_pos < block_stop_pos,
        "delta should come before content_block_stop"
    );
    assert!(
        block_stop_pos < msg_delta_pos,
        "content_block_stop should come before message_delta"
    );
    assert!(
        msg_delta_pos < msg_stop_pos,
        "message_delta should come before message_stop"
    );

    child.kill().await.ok();
}

/// Test that invalid routes return 404.
#[tokio::test]
async fn test_invalid_route_404() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/nonexistent", port))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);

    child.kill().await.ok();
}

/// Test that empty messages return 400 or 422 error.
#[tokio::test]
async fn test_empty_messages_error() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/messages", port);

    // Empty messages array — should be rejected
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "model": "test",
            "messages": [],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    // Should return an error (400 or similar)
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error", "Empty messages should return error");

    child.kill().await.ok();
}

/// Test shell command with disabled policy returns 403.
#[tokio::test]
async fn test_shell_disabled_policy() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "disabled")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/messages", port);

    let resp = client
        .post(&url)
        .json(&build_request("!ls", false))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        403,
        "Shell commands should be blocked when policy is disabled"
    );

    child.kill().await.ok();
}

/// Test shell allowlist policy — allowed command passes, blocked command fails.
#[tokio::test]
async fn test_shell_allowlist_policy() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "allowlist")
        .env("BRIDGE_SHELL_ALLOWLIST", "echo,pwd")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/messages", port);

    // Allowed command should succeed
    let resp = client
        .post(&url)
        .json(&build_request("!echo allowed", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Allowed command should pass");

    // Blocked command should fail
    let resp = client
        .post(&url)
        .json(&build_request("!rm -rf /", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "Blocked command should return 403");

    child.kill().await.ok();
}

/// Test multi-content message format (array of content blocks).
#[tokio::test]
async fn test_multi_content_format() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "unrestricted")
        .env_remove("BRIDGE_AUTH_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/messages", port);

    let resp = client
        .post(&url)
        .json(&build_multi_content_request(
            vec![("text", "!echo multi"), ("text", "content_test")],
            false,
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let _text = body["content"][0]["text"].as_str().unwrap();
    // The prompt should be "!echo multi\ncontent_test" but since it starts with !
    // it will try to run "echo multi\ncontent_test" as a shell command
    // The important thing is it doesn't crash
    assert_eq!(body["type"], "message");

    child.kill().await.ok();
}

/// Test authentication middleware checks.
#[tokio::test]
async fn test_auth_flow() {
    let port = get_free_port();
    let mut child = tokio::process::Command::new("./target/release/opencode2claude")
        .env("BRIDGE_PORT", port.to_string())
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_SHELL_POLICY", "unrestricted")
        .env("BRIDGE_AUTH_TOKEN", "valid-secret-token")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn bridge binary");

    sleep(Duration::from_millis(500)).await;

    let client = Client::new();
    let health_url = format!("http://127.0.0.1:{}/health", port);
    let messages_url = format!("http://127.0.0.1:{}/v1/messages", port);

    // 1. Health check should succeed WITHOUT token
    let resp = client.get(&health_url).send().await.unwrap();
    assert_eq!(resp.status(), 200, "/health should skip auth");

    // 2. Messages API should fail WITHOUT token
    let resp = client
        .post(&messages_url)
        .json(&build_request("!echo test", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "Should return 401 Unauthorized when missing token"
    );

    // 3. Messages API should fail with WRONG token
    let resp = client
        .post(&messages_url)
        .header("Authorization", "Bearer invalid-token")
        .json(&build_request("!echo test", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "Should return 401 Unauthorized with wrong token"
    );

    // 4. Messages API should succeed with VALID token
    let resp = client
        .post(&messages_url)
        .header("Authorization", "Bearer valid-secret-token")
        .json(&build_request("!echo test", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Should succeed with valid Bearer token");

    child.kill().await.ok();
}
