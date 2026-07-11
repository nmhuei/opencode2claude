use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{sleep, Instant};

/// A spawned bridge instance for black-box integration testing.
pub struct TestBridge {
    pub child: tokio::process::Child,
    pub port: u16,
    pub client: Client,
    runtime_dir: PathBuf,
}

impl TestBridge {
    /// Start the Cargo-built bridge process with custom environment overrides.
    pub async fn start(env_overrides: HashMap<&str, &str>) -> Self {
        let port = Self::get_free_port();
        let runtime_dir = std::env::temp_dir().join(format!(
            "opencode2api-integration-{}-{}",
            std::process::id(),
            port
        ));
        let binary = env!("CARGO_BIN_EXE_opencode2api");

        let mut cmd = tokio::process::Command::new(binary);
        cmd.arg("serve")
            .env("BRIDGE_PORT", port.to_string())
            .env("BRIDGE_HOST", "127.0.0.1")
            .env("RUNTIME_DIR", &runtime_dir)
            .env("BRIDGE_CONFIG_PATH", runtime_dir.join("config.toml"))
            .env("RUST_LOG", "warn")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Make the child hermetic even when the repository contains a local .env.
        // dotenvy does not overwrite explicitly supplied environment values.
        cmd.env("BRIDGE_AUTH_TOKEN", "");
        cmd.env("DASHBOARD_ADMIN_TOKEN", "");
        cmd.env("REST_API_TOKEN", "");
        cmd.env("OPENCODE_MODEL", "");
        cmd.env("BRIDGE_PRIMARY_PROXIES", "");
        cmd.env("BRIDGE_WARM_STANDBY_PROXIES", "");
        cmd.env("BRIDGE_EGRESS_MODE", "direct");

        if !env_overrides.contains_key("BRIDGE_SHELL_POLICY") {
            cmd.env("BRIDGE_SHELL_POLICY", "unrestricted");
        }
        for (key, value) in &env_overrides {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .expect("failed to spawn Cargo-built bridge binary");
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("integration HTTP client");
        let base = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = child.stderr.take().map(|mut stream| async move {
                    use tokio::io::AsyncReadExt;
                    let mut output = String::new();
                    let _ = stream.read_to_string(&mut output).await;
                    output
                });
                let details = match stderr {
                    Some(read) => read.await,
                    None => String::new(),
                };
                panic!("bridge exited before readiness: {status}; stderr={details}");
            }
            if let Ok(response) = client.get(format!("{base}/health/live")).send().await {
                if response.status().is_success() {
                    break;
                }
            }
            if Instant::now() >= deadline {
                let _ = child.start_kill();
                let _ = child.wait().await;
                panic!("bridge did not become ready within 10 seconds");
            }
            sleep(Duration::from_millis(50)).await;
        }

        Self {
            child,
            port,
            client,
            runtime_dir,
        }
    }

    fn get_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener.local_addr().expect("ephemeral address").port()
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    pub async fn post_messages(&self, body: &Value) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(self.url("/v1/messages"))
            .json(body)
            .send()
            .await
    }

    pub async fn post_messages_auth(
        &self,
        body: &Value,
        token: &str,
    ) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(self.url("/v1/messages"))
            .header("Authorization", format!("Bearer {token}"))
            .json(body)
            .send()
            .await
    }

    pub async fn get_health(&self) -> reqwest::Result<reqwest::Response> {
        self.client.get(self.url("/health")).send().await
    }

    pub async fn get_models(&self) -> reqwest::Result<reqwest::Response> {
        self.client.get(self.url("/v1/models")).send().await
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

pub fn build_request(prompt: &str, stream: bool) -> Value {
    serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": prompt}],
        "stream": stream
    })
}
