//! Proxy reachability and safe bulk lifecycle operations.

use super::lifecycle::default_runtime;
use super::types::{ContainerRuntime, DockerResult, ProxySpec};
use crate::config::BridgeConfig;
use std::time::Duration;

pub async fn verify_proxy(port: u16) -> bool {
    let proxy_url = format!("socks5h://127.0.0.1:{port}");
    let Ok(proxy) = reqwest::Proxy::all(proxy_url) else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return false;
    };
    client
        .get("https://cloudflare.com/cdn-cgi/trace")
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// Stop or purge only managed primary containers. Protected standby ports are
/// not discovered by name and are never included, even when `purge=true`.
pub async fn stop_proxy_containers(purge: bool) -> DockerResult<()> {
    let runtime = default_runtime();
    stop_proxy_containers_with_runtime(&runtime, purge).await
}

pub async fn stop_proxy_containers_with_runtime(
    runtime: &dyn ContainerRuntime,
    purge: bool,
) -> DockerResult<()> {
    let config = BridgeConfig::from_env_and_cli(Default::default());
    for port in [40001_u16, 40002, 40003] {
        let spec = ProxySpec::new(port, config.runtime.warp_image.clone())?;
        let result = if purge {
            runtime.remove_managed(&spec).await
        } else {
            runtime.stop_managed(&spec).await
        };
        if let Err(error) = result {
            // Bulk shutdown is best-effort when a managed container is absent,
            // but all safety/policy errors are propagated.
            let message = error.to_string();
            if !message.contains("No such container") && !message.contains("No such object") {
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{ContainerState, ContainerSummary, DockerError};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeRuntime {
        mutated_ports: Arc<Mutex<Vec<u16>>>,
    }

    #[async_trait]
    impl ContainerRuntime for FakeRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".into())
        }
        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
            Ok(ContainerState {
                exists: true,
                running: true,
                has_expected_volume: true,
            })
        }
        async fn create_missing(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn recreate_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutated_ports.lock().unwrap().push(spec.port);
            Ok(())
        }
        async fn remove_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutated_ports.lock().unwrap().push(spec.port);
            Ok(())
        }
        async fn restart_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutated_ports.lock().unwrap().push(spec.port);
            Ok(())
        }
        async fn stop_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutated_ports.lock().unwrap().push(spec.port);
            Ok(())
        }
        async fn start_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn logs(&self, _spec: &ProxySpec, _tail: usize) -> DockerResult<String> {
            Ok(String::new())
        }
        async fn list(&self, _specs: &[ProxySpec]) -> DockerResult<Vec<ContainerSummary>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn bulk_stop_never_targets_protected_standby() {
        let runtime = FakeRuntime::default();
        stop_proxy_containers_with_runtime(&runtime, false)
            .await
            .expect("stop");
        assert_eq!(
            *runtime.mutated_ports.lock().unwrap(),
            vec![40001, 40002, 40003]
        );
    }

    #[tokio::test]
    async fn bulk_purge_never_targets_protected_standby() {
        let runtime = FakeRuntime::default();
        stop_proxy_containers_with_runtime(&runtime, true)
            .await
            .expect("purge");
        assert_eq!(
            *runtime.mutated_ports.lock().unwrap(),
            vec![40001, 40002, 40003]
        );
    }

    #[test]
    fn fake_error_type_is_available_for_adapter_tests() {
        let error = DockerError::CommandFailed("x".into());
        assert!(error.to_string().contains('x'));
    }
}
