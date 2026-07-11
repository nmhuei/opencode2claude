//! Docker-backed WARP proxy management through an injectable runtime boundary.

mod bootstrap;
mod health;
mod lifecycle;
mod types;

use crate::config::BridgeConfig;

pub use bootstrap::{bootstrap_proxy_pool, bootstrap_proxy_pool_with_runtime};
pub use health::{stop_proxy_containers, stop_proxy_containers_with_runtime, verify_proxy};
pub use lifecycle::{default_runtime, ensure_proxy, DockerCliRuntime};
pub use types::{
    container_name, validate_known_port, validate_managed_port, ContainerRuntime,
    ContainerSetupState, ContainerState, ContainerSummary, DockerError, DockerResult, ProxySpec,
};

pub async fn create_container(port: u16) -> DockerResult<()> {
    let runtime = default_runtime();
    let spec = runtime.proxy_spec(port)?;
    runtime.recreate_managed(&spec).await
}

pub async fn remove_container(port: u16) -> DockerResult<()> {
    let runtime = default_runtime();
    let spec = runtime.proxy_spec(port)?;
    runtime.remove_managed(&spec).await
}

pub async fn container_logs(port: u16, tail: usize) -> DockerResult<String> {
    let runtime = default_runtime();
    let spec = runtime.proxy_spec(port)?;
    runtime.logs(&spec, tail).await
}

pub async fn list_containers(ports: &[u16]) -> Vec<(u16, String, bool)> {
    let config = BridgeConfig::from_env_and_cli(Default::default());
    let runtime = DockerCliRuntime::from_config(&config);
    let specs = ports
        .iter()
        .filter_map(|port| ProxySpec::new(*port, config.runtime.warp_image.clone()).ok())
        .collect::<Vec<_>>();
    runtime
        .list(&specs)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|summary| (summary.port, summary.name, summary.running))
        .collect()
}

pub async fn check_daemon() -> DockerResult<String> {
    default_runtime().daemon_version().await
}

pub async fn is_docker_available() -> bool {
    check_daemon().await.is_ok()
}

pub async fn ensure_container(port: u16) -> DockerResult<ContainerSetupState> {
    let runtime = default_runtime();
    let spec = runtime.proxy_spec(port)?;
    ensure_proxy(&runtime, &spec).await
}
