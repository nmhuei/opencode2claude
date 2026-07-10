//! Docker domain types and port-safety rules.

use crate::proxy_pool::is_protected_proxy_port;

pub(super) const WARP_IMAGE: &str = "ghcr.io/mon-ius/docker-warp-socks:latest";

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
pub(super) fn validate_destructive_port(port: u16) -> DockerResult<()> {
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
pub(super) fn validate_read_only_port(port: u16) -> DockerResult<()> {
    if !(40001..=40005).contains(&port) {
        return Err(DockerError::InvalidPort(port));
    }
    Ok(())
}
