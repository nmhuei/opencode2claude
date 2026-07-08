//! Docker WARP container lifecycle management.
//!
//! All operations guard against protected warm-standby proxies (40004-40005).

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

/// Ensure the port is valid for destructive operations (create/remove/restart).
fn validate_destructive_port(port: u16) -> DockerResult<()> {
    if !(40001..=40005).contains(&port) {
        return Err(DockerError::InvalidPort(port));
    }
    if is_protected_proxy_port(port) {
        return Err(DockerError::Protected(format!(
            "Port {} is a protected warm-standby proxy (40004-40005). Refusing to modify.",
            port
        )));
    }
    Ok(())
}

/// Ensure the port is valid for read-only operations (logs/status/health-check).
/// Allows all known proxy ports including warm-standby.
fn validate_read_only_port(port: u16) -> DockerResult<()> {
    if !(40001..=40005).contains(&port) {
        return Err(DockerError::InvalidPort(port));
    }
    Ok(())
}

/// Create or recreate a Docker WARP container.
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

use yansi::Paint;

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

/// Verify if a proxy is online by trying to establish a connection.
/// Internally uses a SOCKS5 connect to cloudflare.com.
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

pub async fn bootstrap_proxy_pool(quiet: bool) -> DockerResult<(String, String)> {
    if !is_docker_available().await {
        if !quiet {
            println!(
                "{} Docker is not available. Skipping proxy pool bootstrap.",
                "ℹ".cyan()
            );
        }
        return Ok((String::new(), String::new()));
    }

    if !quiet {
        println!(
            "{} Docker is running. Automating SOCKS5 proxy pool setup for multi-agent support...",
            "✓".green().bold()
        );
    }

    let primary_ports = [40001, 40002, 40003];
    let standby_ports = [40004, 40005];
    let all_ports = [&primary_ports[..], &standby_ports[..]].concat();

    let mut setup_handles = Vec::new();
    for &port in &all_ports {
        setup_handles.push(tokio::spawn(
            async move { (port, ensure_container(port).await) },
        ));
    }

    let mut setup_results = Vec::new();
    for handle in setup_handles {
        if let Ok(res) = handle.await {
            setup_results.push(res);
        }
    }

    let mut new_count = 0;
    let mut migrated_count = 0;
    let mut resumed_count = 0;
    let mut running_count = 0;

    for (port, res) in &setup_results {
        match res {
            Ok(state) => match state {
                ContainerSetupState::New => new_count += 1,
                ContainerSetupState::Migrated => migrated_count += 1,
                ContainerSetupState::Resumed => resumed_count += 1,
                ContainerSetupState::Running => running_count += 1,
            },
            Err(e) => {
                if !quiet {
                    eprintln!(
                        "{} Failed to setup container on port {}: {}",
                        "✗".red().bold(),
                        port,
                        e
                    );
                }
            }
        }
    }

    if !quiet {
        if running_count > 0 {
            println!(
                "  {} {} container(s) already running",
                "✓".green(),
                running_count
            );
        }
        if resumed_count > 0 {
            println!(
                "  {} Resumed {} stopped container(s) (WARP cached — instant start)",
                "✓".green(),
                resumed_count
            );
        }
        if migrated_count > 0 {
            println!(
                "  {} Migrated {} container(s) to volume-cached mode (one-time WARP registration)",
                "ℹ".yellow(),
                migrated_count
            );
        }
        if new_count > 0 {
            println!(
                "  {} Created {} new container(s) (WARP registration required)",
                "ℹ".yellow(),
                new_count
            );
        }
    }

    let needs_registration = new_count + migrated_count;
    if needs_registration > 0 {
        if !quiet {
            println!(
                "  {} Waiting 20 seconds for Cloudflare WARP registration ({} new/migrated)...",
                "ℹ".yellow(),
                needs_registration
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    } else if resumed_count > 0 && !quiet {
        println!("  Cached WARP config detected — skipping wait...");
    }

    // Helper closure to verify proxies in parallel
    let verify_all = |ports_to_verify: Vec<u16>,
                      max_attempts: usize,
                      sleep_secs: u64,
                      label: &'static str| async move {
        if !quiet {
            println!(
                "  {} Verifying {} proxy(ies) in parallel{}...",
                "::".blue(),
                ports_to_verify.len(),
                label
            );
        }

        let mut verify_handles = Vec::new();
        for port in ports_to_verify {
            verify_handles.push(tokio::spawn(async move {
                let mut ok = false;
                for _ in 0..max_attempts {
                    if verify_proxy(port).await {
                        ok = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
                }
                (port, ok)
            }));
        }

        let mut failed = Vec::new();
        for handle in verify_handles {
            if let Ok((port, ok)) = handle.await {
                let c_name = container_name(port);
                if ok {
                    if !quiet {
                        println!("  {} {} (port {}) — Online", "✓".green(), c_name, port);
                    }
                } else {
                    if !quiet {
                        println!("  {} {} (port {}) — Failed", "✗".red(), c_name, port);
                    }
                    failed.push(port);
                }
            }
        }
        failed
    };

    let failed_ports = verify_all(all_ports.clone(), 15, 2, "").await;

    let final_failed_ports = if !failed_ports.is_empty() {
        if !quiet {
            println!(
                "\n  {} Recovering {} failed proxy(ies) — restarting containers...",
                "ℹ".yellow(),
                failed_ports.len()
            );
        }

        let mut restart_handles = Vec::new();
        for &port in &failed_ports {
            let name = container_name(port);
            restart_handles.push(tokio::spawn(async move {
                let _ = tokio::process::Command::new("docker")
                    .args(["restart", &name])
                    .output()
                    .await;
            }));
        }
        for h in restart_handles {
            let _ = h.await;
        }

        if !quiet {
            println!("  Waiting 15 seconds for WARP reconnection...");
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        let retry_ports = failed_ports.clone();
        verify_all(retry_ports, 10, 3, " (retry)").await
    } else {
        failed_ports
    };

    if !quiet && final_failed_ports.is_empty() {
        println!("  {} All proxies verified and online!", "✓".green());
    } else if !quiet && !final_failed_ports.is_empty() {
        println!(
            "  {} {} proxy(ies) still offline. Bridge will route around them.",
            "⚠".yellow(),
            final_failed_ports.len()
        );
    }

    let primary_str = primary_ports
        .iter()
        .map(|p| format!("socks5://127.0.0.1:{}", p))
        .collect::<Vec<_>>()
        .join(",");
    let standby_str = standby_ports
        .iter()
        .map(|p| format!("socks5://127.0.0.1:{}", p))
        .collect::<Vec<_>>()
        .join(",");

    Ok((primary_str, standby_str))
}
