//! Interactive bootstrap of the local WARP proxy pool.

use super::lifecycle::{default_runtime, ensure_proxy};
use super::types::{ContainerRuntime, ContainerSetupState, DockerResult, ProxySpec};
use crate::config::BridgeConfig;
use futures_util::future::join_all;
use std::time::Duration;
use yansi::Paint;

pub async fn bootstrap_proxy_pool(quiet: bool) -> DockerResult<(String, String)> {
    let config = BridgeConfig::from_env_and_cli(Default::default());
    let runtime = default_runtime();
    bootstrap_proxy_pool_with_runtime(&runtime, &config, quiet).await
}

pub async fn bootstrap_proxy_pool_with_runtime(
    runtime: &dyn ContainerRuntime,
    config: &BridgeConfig,
    quiet: bool,
) -> DockerResult<(String, String)> {
    if runtime.daemon_version().await.is_err() {
        if !quiet {
            println!(
                "{} Docker is not available; proxy bootstrap skipped.",
                "ℹ".cyan()
            );
        }
        return Ok((String::new(), String::new()));
    }

    let primary_ports = [40001_u16, 40002, 40003];
    let standby_ports = [40004_u16, 40005];
    let specs = primary_ports
        .iter()
        .chain(standby_ports.iter())
        .map(|port| ProxySpec::new(*port, config.runtime.warp_image.clone()))
        .collect::<DockerResult<Vec<_>>>()?;

    let setup_results = join_all(specs.iter().map(|spec| async move {
        (
            spec.port,
            spec.is_protected(),
            ensure_proxy(runtime, spec).await,
        )
    }))
    .await;

    let mut registration_needed = 0usize;
    for (port, protected, result) in &setup_results {
        match result {
            Ok(ContainerSetupState::New | ContainerSetupState::Migrated) => {
                registration_needed += 1;
            }
            Ok(ContainerSetupState::ProtectedLegacy) => {
                if !quiet {
                    eprintln!(
                        "{} protected standby port {} has legacy configuration; no automatic migration was attempted",
                        "⚠".yellow(),
                        port
                    );
                }
            }
            Ok(_) => {}
            Err(error) => {
                if !quiet {
                    eprintln!("{} proxy port {} setup failed: {}", "✗".red(), port, error);
                }
                if *protected {
                    // Protected standby failures are informational; never recover
                    // them through restart/recreate from bootstrap.
                    continue;
                }
            }
        }
    }

    if registration_needed > 0 {
        if !quiet {
            println!(
                "{} waiting for {} new/migrated WARP registration(s)...",
                "ℹ".yellow(),
                registration_needed
            );
        }
        tokio::time::sleep(Duration::from_secs(20)).await;
    }

    let verification = join_all(
        specs
            .iter()
            .map(|spec| async move { (spec, runtime.verify_online(spec).await) }),
    )
    .await;

    for (spec, online) in verification {
        if online {
            if !quiet {
                println!("{} {} (port {}) online", "✓".green(), spec.name, spec.port);
            }
        } else if spec.is_protected() {
            if !quiet {
                eprintln!("{} restarting offline standby {}", "⚠".yellow(), spec.name);
            }
            if let Err(error) = runtime.restart_managed(spec).await {
                if !quiet {
                    eprintln!(
                        "{} standby {} restart failed: {}",
                        "✗".red(),
                        spec.name,
                        error
                    );
                }
            }
        } else {
            if !quiet {
                eprintln!(
                    "{} restarting failed managed primary {}",
                    "⚠".yellow(),
                    spec.name
                );
            }
            runtime.restart_managed(spec).await?;
        }
    }

    let primary = primary_ports
        .iter()
        .map(|port| format!("socks5h://127.0.0.1:{port}"))
        .collect::<Vec<_>>()
        .join(",");
    let standby = standby_ports
        .iter()
        .map(|port| format!("socks5h://127.0.0.1:{port}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok((primary, standby))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeRuntime {
        states: HashMap<u16, super::super::types::ContainerState>,
        mutations: Arc<Mutex<Vec<(u16, &'static str)>>>,
        offline_ports: Vec<u16>,
        restart_fails: bool,
    }

    #[async_trait]
    impl ContainerRuntime for FakeRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".into())
        }
        async fn inspect(
            &self,
            spec: &ProxySpec,
        ) -> DockerResult<super::super::types::ContainerState> {
            Ok(self.states.get(&spec.port).cloned().unwrap_or(
                super::super::types::ContainerState {
                    exists: true,
                    running: true,
                    has_expected_volume: true,
                },
            ))
        }
        async fn create_missing(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutations.lock().unwrap().push((spec.port, "create"));
            Ok(())
        }
        async fn recreate_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutations.lock().unwrap().push((spec.port, "recreate"));
            Ok(())
        }
        async fn remove_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutations.lock().unwrap().push((spec.port, "remove"));
            Ok(())
        }
        async fn restart_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutations.lock().unwrap().push((spec.port, "restart"));
            if self.restart_fails {
                return Err(super::super::types::DockerError::CommandFailed(
                    "boom".into(),
                ));
            }
            Ok(())
        }
        async fn stop_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutations.lock().unwrap().push((spec.port, "stop"));
            Ok(())
        }
        async fn start_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
            self.mutations.lock().unwrap().push((spec.port, "start"));
            Ok(())
        }
        async fn verify_online(&self, spec: &ProxySpec) -> bool {
            !self.offline_ports.contains(&spec.port)
        }
        async fn logs(&self, _spec: &ProxySpec, _tail: usize) -> DockerResult<String> {
            Ok(String::new())
        }
        async fn list(
            &self,
            _specs: &[ProxySpec],
        ) -> DockerResult<Vec<super::super::types::ContainerSummary>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn stopped_protected_node_is_started_but_legacy_is_never_mutated() {
        let mut runtime = FakeRuntime::default();
        runtime.states.insert(
            40004,
            super::super::types::ContainerState {
                exists: true,
                running: false,
                has_expected_volume: true,
            },
        );
        runtime.states.insert(
            40005,
            super::super::types::ContainerState {
                exists: true,
                running: true,
                has_expected_volume: false,
            },
        );
        let config = BridgeConfig::default();
        for port in [40004_u16, 40005] {
            let spec = ProxySpec::new(port, config.runtime.warp_image.clone()).unwrap();
            let _ = ensure_proxy(&runtime, &spec).await.unwrap();
        }
        let mutations = runtime.mutations.lock().unwrap();
        assert!(
            mutations.iter().any(|(p, a)| *p == 40004 && *a == "start"),
            "stopped protected node must be started"
        );
        assert!(
            !mutations.iter().any(|(p, _)| *p == 40005),
            "running legacy protected node must not be mutated"
        );
    }

    #[tokio::test]
    async fn offline_standbys_are_restarted_best_effort() {
        let mut runtime = FakeRuntime::default();
        for port in [40004_u16, 40005] {
            runtime.states.insert(
                port,
                super::super::types::ContainerState {
                    exists: true,
                    running: false,
                    has_expected_volume: true,
                },
            );
        }
        runtime.offline_ports = vec![40004, 40005];
        runtime.restart_fails = true;
        let config = BridgeConfig::default();
        let (primary, standby) = bootstrap_proxy_pool_with_runtime(&runtime, &config, true)
            .await
            .expect("bootstrap must not abort when standby restart fails");
        assert!(primary.contains("40001"));
        assert!(standby.contains("40004") && standby.contains("40005"));
        let mutations = runtime.mutations.lock().unwrap();
        assert!(
            mutations.iter().any(|(p, a)| *p == 40004 && *a == "start"),
            "stopped standby is resumed before verification"
        );
        assert!(
            mutations
                .iter()
                .any(|(p, a)| *p == 40005 && *a == "restart"),
            "offline standby restart is attempted despite expected failure"
        );
    }
}
