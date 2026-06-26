//! Integration tests for the OpenCode2Claude bridge API.
//!
//! These tests spawn a real bridge server and test the HTTP API
//! endpoints including streaming SSE, non-streaming JSON, shell
//! command interception, health checks, and error handling.
//!
//! Run with: `cargo build --release && cargo test --test integration -- --ignored`

mod common;

use common::{build_request, TestBridge};
use serde_json::Value;
use std::collections::HashMap;

/// Test that the bridge binary can be spawned and responds on /health.
#[tokio::test]
#[ignore]
async fn test_bridge_binary_health() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let resp = bridge.get_health().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
}

/// Test non-streaming shell command via `!echo`.
#[tokio::test]
#[ignore]
async fn test_shell_command_non_streaming() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let resp = bridge
        .post_messages(&build_request("!echo integration_test_123", false))
        .await
        .unwrap();

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
}

/// Test streaming shell command returns proper SSE events.
#[tokio::test]
#[ignore]
async fn test_shell_command_streaming_sse() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let resp = bridge
        .post_messages(&build_request("!echo sse_test_456", true))
        .await
        .unwrap();

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
}

/// Test that invalid routes return 404.
#[tokio::test]
#[ignore]
async fn test_invalid_route_404() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let client = reqwest::Client::new();
    let resp = client.get(bridge.url("/nonexistent")).send().await.unwrap();

    assert_eq!(resp.status(), 404);
}

/// Test that empty messages return 400 or 422 error.
#[tokio::test]
#[ignore]
async fn test_empty_messages_error() {
    let bridge = TestBridge::start(HashMap::new()).await;

    let resp = bridge
        .post_messages(&serde_json::json!({
            "model": "test",
            "messages": [],
            "stream": false
        }))
        .await
        .unwrap();

    // Should return an error
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error", "Empty messages should return error");
}

/// Test shell command with disabled policy returns 403.
#[tokio::test]
#[ignore]
async fn test_shell_disabled_policy() {
    let bridge = TestBridge::start(HashMap::from([("BRIDGE_SHELL_POLICY", "disabled")])).await;

    let resp = bridge
        .post_messages(&build_request("!ls", false))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        403,
        "Shell commands should be blocked when policy is disabled"
    );
}

/// Test shell allowlist policy — allowed command passes, blocked command fails.
#[tokio::test]
#[ignore]
async fn test_shell_allowlist_policy() {
    let bridge = TestBridge::start(HashMap::from([
        ("BRIDGE_SHELL_POLICY", "allowlist"),
        ("BRIDGE_SHELL_ALLOWLIST", "echo,pwd"),
    ]))
    .await;

    // Allowed command should succeed
    let resp = bridge
        .post_messages(&build_request("!echo allowed", false))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Allowed command should pass");

    // Blocked command should fail
    let resp = bridge
        .post_messages(&build_request("!rm -rf /", false))
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "Blocked command should return 403");
}

/// Test multi-content message format (array of content blocks).
#[tokio::test]
#[ignore]
async fn test_multi_content_format() {
    let bridge = TestBridge::start(HashMap::new()).await;

    let content: Vec<Value> = vec![
        serde_json::json!({"type": "text", "text": "!echo multi"}),
        serde_json::json!({"type": "text", "text": "content_test"}),
    ];

    let body = serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": content}],
        "stream": false
    });

    let resp = bridge.post_messages(&body).await.unwrap();
    assert_eq!(resp.status(), 200);

    let resp_body: Value = resp.json().await.unwrap();
    assert_eq!(resp_body["type"], "message");
}

/// Test authentication middleware checks.
#[tokio::test]
#[ignore]
async fn test_auth_flow() {
    let bridge = TestBridge::start(HashMap::from([
        ("BRIDGE_AUTH_TOKEN", "valid-secret-token"),
        ("BRIDGE_SHELL_POLICY", "unrestricted"),
    ]))
    .await;

    // 1. Health check should succeed WITHOUT token
    let resp = bridge.get_health().await.unwrap();
    assert_eq!(resp.status(), 200, "/health should skip auth");

    // 2. Messages API should fail WITHOUT token
    let resp = bridge
        .post_messages(&build_request("!echo test", false))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "Should return 401 Unauthorized when missing token"
    );

    // 3. Messages API should fail with WRONG token
    let resp = bridge
        .post_messages_auth(&build_request("!echo test", false), "invalid-token")
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "Should return 401 Unauthorized with wrong token"
    );

    // 4. Messages API should succeed with VALID token
    let resp = bridge
        .post_messages_auth(&build_request("!echo test", false), "valid-secret-token")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Should succeed with valid Bearer token");
}

// ── New tests ──

/// Health check trả về 200 NGAY CẢ KHI auth được bật.
#[tokio::test]
#[ignore]
async fn test_health_with_auth_enabled() {
    let bridge = TestBridge::start(HashMap::from([("BRIDGE_AUTH_TOKEN", "secret")])).await;
    let resp = bridge.get_health().await.unwrap();
    assert_eq!(resp.status(), 200);
}

/// /v1/models trả về đúng format.
#[tokio::test]
#[ignore]
async fn test_models_endpoint() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let resp = bridge.get_models().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "list");
    assert!(body["data"].is_array());
    assert!(!body["data"].as_array().unwrap().is_empty());
    assert!(body["data"][0]["id"].as_str().is_some());
}

/// Nhiều auth tokens đều hoạt động.
#[tokio::test]
#[ignore]
async fn test_multi_token_auth() {
    let bridge = TestBridge::start(HashMap::from([
        ("BRIDGE_AUTH_TOKEN", "token-a,token-b"),
        ("BRIDGE_SHELL_POLICY", "unrestricted"),
    ]))
    .await;

    // Token A hoạt động
    let resp = bridge
        .post_messages_auth(&build_request("!echo ok", false), "token-a")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Token A should work");

    // Token B hoạt động
    let resp = bridge
        .post_messages_auth(&build_request("!echo ok", false), "token-b")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Token B should work");

    // Token C bị từ chối
    let resp = bridge
        .post_messages_auth(&build_request("!echo ok", false), "token-c")
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "Token C should be rejected");
}

/// Request body quá lớn bị từ chối.
#[tokio::test]
#[ignore]
async fn test_large_body_rejection() {
    let bridge = TestBridge::start(HashMap::from([("BRIDGE_MAX_BODY_SIZE", "100")])).await;

    // Tạo prompt >100 bytes
    let big_prompt = "x".repeat(200);
    let body = serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": big_prompt}],
        "stream": false
    });

    let resp = bridge.post_messages(&body).await.unwrap();
    assert_eq!(resp.status(), 413, "Large body should be rejected");
}

/// Concurrent requests đều thành công.
#[tokio::test]
#[ignore]
async fn test_concurrent_requests() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let bridge = std::sync::Arc::new(bridge);
    let mut handles = Vec::new();

    for i in 0..5 {
        let bridge = std::sync::Arc::clone(&bridge);
        handles.push(tokio::spawn(async move {
            bridge
                .post_messages(&build_request(&format!("!echo concurrent_{}", i), false))
                .await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), 200);
    }
}

/// Streaming shell command với policy disabled => 403.
#[tokio::test]
#[ignore]
async fn test_shell_disabled_streaming() {
    let bridge = TestBridge::start(HashMap::from([("BRIDGE_SHELL_POLICY", "disabled")])).await;

    let resp = bridge
        .post_messages(&build_request("!echo hi", true))
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

/// Auth với streaming request.
#[tokio::test]
#[ignore]
async fn test_auth_with_streaming() {
    let bridge = TestBridge::start(HashMap::from([
        ("BRIDGE_AUTH_TOKEN", "stream-secret"),
        ("BRIDGE_SHELL_POLICY", "unrestricted"),
    ]))
    .await;

    // Without token -> 401
    let resp = bridge
        .post_messages(&build_request("!echo hi", true))
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With valid token -> 200 + SSE
    let resp = bridge
        .post_messages_auth(&build_request("!echo hi", true), "stream-secret")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/event-stream"));
}

/// Verify daemon status trong health check response.
#[tokio::test]
#[ignore]
async fn test_health_daemon_status() {
    let bridge = TestBridge::start(HashMap::new()).await;
    let resp = bridge.get_health().await.unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
    assert!(body["daemon"]["status"].as_str().is_some());
    assert!(body["daemon"]["port"].as_u64().is_some());
    assert!(body["config"]["shell_policy"].as_str().is_some());
    assert_eq!(body["config"]["auth_enabled"], false);
}
