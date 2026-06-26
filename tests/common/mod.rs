use std::collections::HashMap;
use std::net::TcpListener;
use std::time::Duration;
use tokio::time::sleep;
use serde_json::Value;
use reqwest::Client;

/// A spawned bridge instance for testing.
pub struct TestBridge {
    pub child: tokio::process::Child,
    pub port: u16,
    pub client: Client,
}

impl TestBridge {
    /// Start a bridge process with custom env overrides.
    /// Default env: BRIDGE_HOST=127.0.0.1, BRIDGE_SHELL_POLICY=unrestricted, no auth.
    pub async fn start(env_overrides: HashMap<&str, &str>) -> Self {
        let port = Self::get_free_port();

        let mut cmd = tokio::process::Command::new("./target/release/opencode2claude");
        cmd.env("BRIDGE_PORT", port.to_string())
            .env("BRIDGE_HOST", "127.0.0.1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // Apply defaults
        cmd.env_remove("BRIDGE_AUTH_TOKEN");
        cmd.env_remove("OPENCODE_MODEL");
        if !env_overrides.contains_key("BRIDGE_SHELL_POLICY") {
            cmd.env("BRIDGE_SHELL_POLICY", "unrestricted");
        }

        // Apply overrides
        for (k, v) in &env_overrides {
            cmd.env(k, v);
        }

        let child = cmd
            .spawn()
            .expect("Failed to spawn bridge binary. Run `cargo build --release` first.");

        sleep(Duration::from_millis(500)).await;

        Self {
            child,
            port,
            client: Client::new(),
        }
    }

    fn get_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    /// Build URL for an endpoint.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// Send a POST to /v1/messages with a prompt (unauthenticated).
    pub async fn post_messages(&self, body: &Value) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(self.url("/v1/messages"))
            .json(body)
            .send()
            .await
    }

    /// Send a POST to /v1/messages with auth header.
    pub async fn post_messages_auth(
        &self,
        body: &Value,
        token: &str,
    ) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(self.url("/v1/messages"))
            .header("Authorization", format!("Bearer {}", token))
            .json(body)
            .send()
            .await
    }

    /// GET /health.
    pub async fn get_health(&self) -> reqwest::Result<reqwest::Response> {
        self.client.get(self.url("/health")).send().await
    }

    /// GET /v1/models.
    pub async fn get_models(&self) -> reqwest::Result<reqwest::Response> {
        self.client.get(self.url("/v1/models")).send().await
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Build a basic request body.
pub fn build_request(prompt: &str, stream: bool) -> Value {
    serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": prompt}],
        "stream": stream
    })
}
