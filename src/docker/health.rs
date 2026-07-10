//! Proxy reachability and bulk stop operations.

use super::lifecycle::is_docker_available;
use super::types::{DockerError, DockerResult};

pub async fn verify_proxy(port: u16) -> bool {
    let output = tokio::process::Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "",
            "-x",
            &format!("socks5h://127.0.0.1:{}", port),
            "--max-time",
            "5",
            "https://cloudflare.com/cdn-cgi/trace",
        ])
        .output()
        .await;
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Stop and optionally remove proxy containers.
/// Always protects warm-standby proxies unless they are being purged explicitly.
pub async fn stop_proxy_containers(purge: bool) -> DockerResult<()> {
    if !is_docker_available().await {
        return Ok(());
    }

    // List all containers starting with opencode-warp-
    let output = tokio::process::Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Names}}"])
        .output()
        .await
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    let names = String::from_utf8_lossy(&output.stdout);
    let mut targets = Vec::new();
    for line in names.lines() {
        let name = line.trim();
        if name.starts_with("opencode-warp-") {
            let is_standby = name.ends_with("-4") || name.ends_with("-5");
            if !is_standby || purge {
                targets.push(name.to_string());
            }
        }
    }

    if targets.is_empty() {
        return Ok(());
    }

    let mut handles = Vec::new();
    for name in targets {
        let handle = tokio::spawn(async move {
            if purge {
                let _ = tokio::process::Command::new("docker")
                    .args(["rm", "-f", &name])
                    .output()
                    .await;
            } else {
                let _ = tokio::process::Command::new("docker")
                    .args(["stop", "-t", "5", &name])
                    .output()
                    .await;
            }
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.await;
    }

    Ok(())
}
