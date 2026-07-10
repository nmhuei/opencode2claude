//! Docker daemon and WARP container lifecycle operations.

use super::types::{
    container_name, validate_destructive_port, validate_read_only_port, DockerError, DockerResult,
    WARP_IMAGE,
};

pub async fn create_container(port: u16) -> DockerResult<()> {
    create_container_internal(port, false).await
}

/// Internal helper to create/run a container, optionally bypassing the protected port check during bootstrap.
pub async fn create_container_internal(port: u16, bypass_protect: bool) -> DockerResult<()> {
    if !bypass_protect {
        validate_destructive_port(port)?;
    } else if !(40001..=40005).contains(&port) {
        return Err(DockerError::InvalidPort(port));
    }

    let name = container_name(port);
    let volume_name = format!("{}-config", name);

    // docker rm -f (ignore error if not exists)
    let _ = tokio::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output()
        .await;

    // docker run -d --name ...
    let fast_entrypoint = "if [ -f /etc/sing-box/config.json ]; then exec sing-box -c /etc/sing-box/config.json run; else exec /run/entrypoint.sh rws-cli-v6; fi";
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
            "-v",
            &format!("{}:/etc/sing-box", volume_name),
            "-p",
            &format!("{}:9091", port),
            "--entrypoint",
            "/bin/sh",
            WARP_IMAGE,
            "-c",
            fast_entrypoint,
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
    validate_destructive_port(port)?;
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

/// Check if the Docker daemon is reachable and return its version string.
pub async fn check_daemon() -> DockerResult<String> {
    let output = tokio::process::Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .await
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if version.is_empty() {
            "unknown".into()
        } else {
            version
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(DockerError::CommandFailed(format!(
            "Docker daemon not reachable: {}",
            stderr.trim()
        )))
    }
}

/// Get logs from a Docker WARP container (primary only).
pub async fn container_logs(port: u16, tail: usize) -> DockerResult<String> {
    validate_read_only_port(port)?;
    let name = container_name(port);

    let output = tokio::process::Command::new("docker")
        .args(["logs", "--tail", &tail.to_string(), &name])
        .output()
        .await;

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            Ok(if stderr.is_empty() {
                stdout
            } else {
                format!("{}\n{}", stdout, stderr)
            })
        }
        Err(e) => Err(DockerError::CommandFailed(e.to_string())),
    }
}

pub async fn is_docker_available() -> bool {
    check_daemon().await.is_ok()
}

/// Helper to check if a container has a specific volume mount.
async fn has_volume(container_name: &str, volume_name: &str) -> bool {
    let output = tokio::process::Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{range .Mounts}}{{.Name}} {{end}}",
            container_name,
        ])
        .output()
        .await;
    match output {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains(volume_name)
        }
        Err(_) => false,
    }
}

/// Helper to get container status. Returns (exists, running).
async fn container_status(container_name: &str) -> (bool, bool) {
    let output = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name=^{}$", container_name),
            "--format",
            "{{.Names}} {{.State}}",
        ])
        .output()
        .await;
    match output {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            if s.trim().is_empty() {
                (false, false)
            } else {
                let running = s.contains("running");
                (true, running)
            }
        }
        Err(_) => (false, false),
    }
}

/// State enum for container setup.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ContainerSetupState {
    New,
    Migrated,
    Resumed,
    Running,
}

/// Ensures a WARP SOCKS5 container is set up correctly on the given port.
/// Returns the state of the setup operation (New, Migrated, Resumed, Running).
pub async fn ensure_container(port: u16) -> DockerResult<ContainerSetupState> {
    let name = container_name(port);
    let volume_name = format!("{}-config", name);

    let (exists, running) = container_status(&name).await;

    if running {
        if has_volume(&name, &volume_name).await {
            Ok(ContainerSetupState::Running)
        } else {
            // Old container without volume — stop, remove, and recreate with volume
            let _ = tokio::process::Command::new("docker")
                .args(["stop", &name])
                .output()
                .await;
            create_container_internal(port, true).await?;
            Ok(ContainerSetupState::Migrated)
        }
    } else if exists {
        if has_volume(&name, &volume_name).await {
            // Stopped with cached config — fast resume
            let output = tokio::process::Command::new("docker")
                .args(["start", &name])
                .output()
                .await
                .map_err(|e| DockerError::CommandFailed(e.to_string()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(DockerError::CommandFailed(format!(
                    "docker start failed for {}: {}",
                    name, stderr
                )));
            }
            Ok(ContainerSetupState::Resumed)
        } else {
            // Old container without volume — remove and recreate with volume
            let _ = tokio::process::Command::new("docker")
                .args(["rm", "-f", &name])
                .output()
                .await;
            create_container_internal(port, true).await?;
            Ok(ContainerSetupState::Migrated)
        }
    } else {
        // Brand new
        create_container_internal(port, true).await?;
        Ok(ContainerSetupState::New)
    }
}
