//! Docker WARP container lifecycle management.
//!
//! All operations guard against protected auxiliary proxies (40004-40005).

use crate::proxy_pool::is_protected_proxy_port;

const WARP_IMAGE: &str = "ghcr.io/mon-ius/docker-warp-socks:latest";

/// Result of a Docker operation.
pub type DockerResult<T> = Result<T, DockerError>;

/// Errors from Docker operations.
#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("Docker command failed: {0}")]
    CommandFailed(String),
    #[error("Protected proxy: {0}")]
    Protected(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Port {0} is out of valid range (40001-40005)")]
    InvalidPort(u16),
}

/// Container name for a given port.
pub fn container_name(port: u16) -> String {
    if (40001..=40099).contains(&port) {
        format!("opencode-warp-{}", port - 40000)
    } else {
        format!("opencode-proxy-{}", port)
    }
}

/// Ensure the port is valid and not protected.
fn validate_port(port: u16) -> DockerResult<()> {
    if !(40001..=40005).contains(&port) {
        return Err(DockerError::InvalidPort(port));
    }
    if is_protected_proxy_port(port) {
        return Err(DockerError::Protected(format!(
            "Port {} is a protected auxiliary proxy (40004-40005). Refusing to modify.",
            port
        )));
    }
    Ok(())
}

/// Create or recreate a Docker WARP container.
pub async fn create_container(port: u16) -> DockerResult<()> {
    validate_port(port)?;
    let name = container_name(port);

    // docker rm -f (ignore error if not exists)
    let _ = tokio::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output()
        .await;

    // docker run -d --name ...
    let output = tokio::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            &name,
            "--restart",
            "always",
            "--cap-add=NET_ADMIN",
            "--sysctl",
            "net.ipv4.conf.all.src_valid_mark=1",
            "-p",
            &format!("{}:9091", port),
            WARP_IMAGE,
        ])
        .output()
        .await
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DockerError::CommandFailed(format!(
            "docker run failed for {}: {}",
            name, stderr
        )));
    }

    Ok(())
}

/// Remove a Docker WARP container (primary only).
pub async fn remove_container(port: u16) -> DockerResult<()> {
    validate_port(port)?;
    let name = container_name(port);

    let output = tokio::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output()
        .await
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Container might not exist — that's OK
        if !stderr.contains("No such container") {
            return Err(DockerError::CommandFailed(format!(
                "docker rm failed for {}: {}",
                name, stderr
            )));
        }
    }

    Ok(())
}

/// List Docker WARP containers known to the system.
pub async fn list_containers(ports: &[u16]) -> Vec<(u16, String, bool)> {
    let mut result = Vec::new();
    for &port in ports {
        let name = container_name(port);
        let output = tokio::process::Command::new("docker")
            .args([
                "ps",
                "--filter",
                &format!("name={}", name),
                "--format",
                "{{.Names}}",
            ])
            .output()
            .await;
        let running = match output {
            Ok(o) => !String::from_utf8_lossy(&o.stdout).is_empty(),
            Err(_) => false,
        };
        result.push((port, name, running));
    }
    result
}

/// Get logs from a Docker WARP container (primary only).
pub async fn container_logs(port: u16, tail: usize) -> DockerResult<String> {
    validate_port(port)?;
    let name = container_name(port);

    let output = tokio::process::Command::new("docker")
        .args(["logs", "--tail", &tail.to_string(), &name])
        .output()
        .await
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(if stderr.is_empty() {
        stdout
    } else {
        format!("{}\n{}", stdout, stderr)
    })
}
