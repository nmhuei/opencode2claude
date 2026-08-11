//! Docker/WARP container domain model and runtime interface.

use crate::proxy_pool::{is_managed_proxy_port, is_protected_proxy_port, LifecyclePolicy};
use async_trait::async_trait;
use std::fmt;

pub type DockerResult<T> = Result<T, DockerError>;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("Docker command failed: {0}")]
    CommandFailed(String),
    #[error("Protected proxy: {0}")]
    Protected(String),
    #[error("Proxy is busy: {0}")]
    Busy(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Port {0} is out of valid range (40001-40005)")]
    InvalidPort(u16),
    #[error("Container runtime returned invalid data: {0}")]
    InvalidResponse(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub port: u16,
    pub name: String,
    pub volume_name: String,
    pub image: String,
    pub host_port: u16,
    pub container_port: u16,
    pub lifecycle: LifecyclePolicy,
}

impl ProxySpec {
    pub fn new(port: u16, image: impl Into<String>) -> DockerResult<Self> {
        validate_known_port(port)?;
        let name = container_name(port);
        Ok(Self {
            port,
            volume_name: format!("{name}-config"),
            name,
            image: image.into(),
            host_port: port,
            container_port: 9091,
            lifecycle: if is_managed_proxy_port(port) {
                LifecyclePolicy::Managed
            } else {
                LifecyclePolicy::Protected
            },
        })
    }

    pub fn is_protected(&self) -> bool {
        self.lifecycle == LifecyclePolicy::Protected
    }

    pub fn run_args(&self) -> Vec<String> {
        const ENTRYPOINT: &str = "if [ -f /etc/sing-box/config.json ]; then exec sing-box -c /etc/sing-box/config.json run; else exec /run/entrypoint.sh rws-cli-v6; fi";
        vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            self.name.clone(),
            "--restart".into(),
            "always".into(),
            "--cap-add=NET_ADMIN".into(),
            "--sysctl".into(),
            "net.ipv4.conf.all.src_valid_mark=1".into(),
            "-v".into(),
            format!("{}:/etc/sing-box", self.volume_name),
            "-p".into(),
            format!("{}:{}", self.host_port, self.container_port),
            "--entrypoint".into(),
            "/bin/sh".into(),
            self.image.clone(),
            "-c".into(),
            ENTRYPOINT.into(),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerSetupState {
    New,
    Migrated,
    Resumed,
    Running,
    ProtectedLegacy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    pub exists: bool,
    pub running: bool,
    pub has_expected_volume: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    pub port: u16,
    pub name: String,
    pub running: bool,
}

#[async_trait]
pub trait ContainerRuntime: Send + Sync + fmt::Debug {
    async fn daemon_version(&self) -> DockerResult<String>;
    async fn inspect(&self, spec: &ProxySpec) -> DockerResult<ContainerState>;
    async fn create_missing(&self, spec: &ProxySpec) -> DockerResult<()>;
    async fn recreate_managed(&self, spec: &ProxySpec) -> DockerResult<()>;
    async fn rotate_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        self.recreate_managed(spec).await
    }
    async fn remove_managed(&self, spec: &ProxySpec) -> DockerResult<()>;
    async fn restart_managed(&self, spec: &ProxySpec) -> DockerResult<()>;
    async fn stop_managed(&self, spec: &ProxySpec) -> DockerResult<()>;
    async fn start_managed(&self, spec: &ProxySpec) -> DockerResult<()>;
    async fn logs(&self, spec: &ProxySpec, tail: usize) -> DockerResult<String>;
    async fn list(&self, specs: &[ProxySpec]) -> DockerResult<Vec<ContainerSummary>>;
    /// Probe proxy reachability. The default performs one network check;
    /// adapters may retry internally before reporting offline.
    async fn verify_online(&self, spec: &ProxySpec) -> bool {
        super::health::verify_proxy(spec.port).await
    }
}

pub fn container_name(port: u16) -> String {
    if (40001..=40099).contains(&port) {
        format!("opencode-warp-{}", port - 40000)
    } else {
        format!("opencode-proxy-{port}")
    }
}

pub fn validate_known_port(port: u16) -> DockerResult<()> {
    if !(40001..=40005).contains(&port) {
        return Err(DockerError::InvalidPort(port));
    }
    Ok(())
}

pub fn validate_managed_port(port: u16) -> DockerResult<()> {
    validate_known_port(port)?;
    if is_protected_proxy_port(port) {
        return Err(DockerError::Protected(format!(
            "port {port} is a protected warm-standby proxy; destructive lifecycle is forbidden"
        )));
    }
    Ok(())
}

/// Validation for non-destructive start/restart: allowed on every known port,
/// including protected standbys (starting a proxy never destroys it).
pub fn validate_startable_port(port: u16) -> DockerResult<()> {
    validate_known_port(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_proxy_spec_has_one_stable_run_contract() {
        let spec = ProxySpec::new(40001, "example/warp:1").expect("spec");
        assert_eq!(spec.name, "opencode-warp-1");
        assert_eq!(spec.volume_name, "opencode-warp-1-config");
        let args = spec.run_args();
        assert_eq!(args.iter().filter(|arg| *arg == "run").count(), 1);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "opencode-warp-1"]));
        assert!(args.windows(2).any(|pair| pair == ["-p", "40001:9091"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-v", "opencode-warp-1-config:/etc/sing-box"]));
        assert!(args.contains(&"example/warp:1".to_string()));
    }

    #[test]
    fn protected_spec_rejects_managed_validation() {
        let spec = ProxySpec::new(40004, "example/warp:1").expect("spec");
        assert!(spec.is_protected());
        assert!(matches!(
            validate_managed_port(40004),
            Err(DockerError::Protected(_))
        ));
    }

    #[test]
    fn protected_spec_allows_startable_validation() {
        for port in [40004_u16, 40005] {
            assert!(validate_startable_port(port).is_ok());
        }
        assert!(matches!(
            validate_startable_port(40099),
            Err(DockerError::InvalidPort(_))
        ));
    }
}
