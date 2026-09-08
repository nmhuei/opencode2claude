use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tracing::{info, warn};

pub const OPENCODE_DEFAULT_PORT: u16 = 4096;
pub const OPENCODE_DEFAULT_URL: &str = "http://127.0.0.1:4096";

pub fn find_opencode_binary() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        let default_path = PathBuf::from(home).join(".opencode/bin/opencode");
        if default_path.exists() {
            return Some(default_path);
        }
    }

    if let Ok(output) = Command::new("which").arg("opencode").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                let path = PathBuf::from(path_str);
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }

    None
}

pub async fn check_opencode_server_health(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(600))
        .build()
        .unwrap_or_default();

    let check_url = format!("{url}/session");
    if let Ok(res) = client.get(&check_url).send().await {
        return res.status().is_success();
    }
    false
}

pub async fn ensure_opencode_server_running() -> Result<String, String> {
    if check_opencode_server_health(OPENCODE_DEFAULT_URL).await {
        return Ok(OPENCODE_DEFAULT_URL.to_string());
    }

    let bin_path = find_opencode_binary()
        .ok_or_else(|| "OpenCode CLI binary not found. Please install opencode or run 'opencode serve' manually.".to_string())?;

    info!(
        "Starting local OpenCode headless server using {:?}",
        bin_path
    );

    let child = Command::new(&bin_path)
        .args([
            "serve",
            "--port",
            &OPENCODE_DEFAULT_PORT.to_string(),
            "--hostname",
            "127.0.0.1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    if let Err(e) = child {
        return Err(format!("Failed to spawn OpenCode server: {e}"));
    }

    // Poll for up to 3 seconds until server responds
    for _ in 0..15 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if check_opencode_server_health(OPENCODE_DEFAULT_URL).await {
            info!("OpenCode headless server is now online at {OPENCODE_DEFAULT_URL}");
            return Ok(OPENCODE_DEFAULT_URL.to_string());
        }
    }

    warn!("OpenCode server process spawned, but health check timed out");
    Ok(OPENCODE_DEFAULT_URL.to_string())
}
