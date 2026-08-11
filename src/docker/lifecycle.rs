//! Docker CLI implementation of the container runtime boundary.

use super::types::*;
use crate::config::BridgeConfig;
use crate::infrastructure::command::{CommandRequest, CommandRunner, SystemCommandRunner};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
pub struct DockerCliRuntime {
    binary: String,
    image: String,
    runner: Arc<dyn CommandRunner>,
}

impl DockerCliRuntime {
    pub fn from_config(config: &BridgeConfig) -> Self {
        Self {
            binary: config.runtime.docker_binary.clone(),
            image: config.runtime.warp_image.clone(),
            runner: Arc::new(SystemCommandRunner),
        }
    }

    pub fn with_runner(
        binary: impl Into<String>,
        image: impl Into<String>,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            binary: binary.into(),
            image: image.into(),
            runner,
        }
    }

    pub fn proxy_spec(&self, port: u16) -> DockerResult<ProxySpec> {
        ProxySpec::new(port, self.image.clone())
    }

    async fn command(
        &self,
        args: Vec<String>,
    ) -> DockerResult<crate::infrastructure::command::CommandOutput> {
        self.runner
            .output(CommandRequest::new(self.binary.clone(), args))
            .await
            .map_err(DockerError::Io)
    }

    async fn require_success(&self, args: Vec<String>, operation: &str) -> DockerResult<String> {
        let output = self.command(args).await?;
        if output.success {
            Ok(output.stdout_text())
        } else {
            Err(DockerError::CommandFailed(format!(
                "{operation}: {}",
                output.stderr_text()
            )))
        }
    }
}

#[async_trait]
impl ContainerRuntime for DockerCliRuntime {
    async fn daemon_version(&self) -> DockerResult<String> {
        let version = self
            .require_success(
                vec![
                    "version".into(),
                    "--format".into(),
                    "{{.Server.Version}}".into(),
                ],
                "Docker daemon not reachable",
            )
            .await?;
        Ok(if version.is_empty() {
            "unknown".to_string()
        } else {
            version
        })
    }

    async fn inspect(&self, spec: &ProxySpec) -> DockerResult<ContainerState> {
        let output = self
            .command(vec![
                "inspect".into(),
                "--format".into(),
                "{{.State.Running}}|{{range .Mounts}}{{.Name}} {{end}}".into(),
                spec.name.clone(),
            ])
            .await?;
        if !output.success {
            let stderr = output.stderr_text();
            if stderr.contains("No such object") || stderr.contains("No such container") {
                return Ok(ContainerState {
                    exists: false,
                    running: false,
                    has_expected_volume: false,
                });
            }
            return Err(DockerError::CommandFailed(format!(
                "docker inspect {}: {stderr}",
                spec.name
            )));
        }
        let text = output.stdout_text();
        let (running, mounts) = text.split_once('|').ok_or_else(|| {
            DockerError::InvalidResponse(format!(
                "docker inspect {} omitted the state/mount delimiter",
                spec.name
            ))
        })?;
        let running = match running.trim() {
            "true" => true,
            "false" => false,
            value => {
                return Err(DockerError::InvalidResponse(format!(
                    "docker inspect {} returned invalid running state '{value}'",
                    spec.name
                )))
            }
        };
        Ok(ContainerState {
            exists: true,
            running,
            has_expected_volume: mounts
                .split_whitespace()
                .any(|name| name == spec.volume_name),
        })
    }

    async fn create_missing(&self, spec: &ProxySpec) -> DockerResult<()> {
        let state = self.inspect(spec).await?;
        if state.exists {
            return Err(if spec.is_protected() {
                DockerError::Protected(format!(
                    "protected node {} already exists; create_missing will not mutate it",
                    spec.name
                ))
            } else {
                DockerError::CommandFailed(format!("container {} already exists", spec.name))
            });
        }
        self.require_success(spec.run_args(), &format!("docker run {}", spec.name))
            .await?;
        Ok(())
    }

    async fn recreate_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        validate_managed_port(spec.port)?;
        let _ = self
            .command(vec!["rm".into(), "-f".into(), spec.name.clone()])
            .await?;
        self.require_success(spec.run_args(), &format!("docker run {}", spec.name))
            .await?;
        Ok(())
    }

    async fn rotate_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        validate_managed_port(spec.port)?;

        let removed_container = self
            .command(vec!["rm".into(), "-f".into(), spec.name.clone()])
            .await?;
        let container_missing = {
            let stderr = removed_container.stderr_text().to_ascii_lowercase();
            stderr.contains("no such container") || stderr.contains("no such object")
        };
        if !removed_container.success && !container_missing {
            return Err(DockerError::CommandFailed(format!(
                "docker rm {}: {}",
                spec.name,
                removed_container.stderr_text()
            )));
        }

        let removed_volume = self
            .command(vec![
                "volume".into(),
                "rm".into(),
                "-f".into(),
                spec.volume_name.clone(),
            ])
            .await?;
        let volume_missing = removed_volume
            .stderr_text()
            .to_ascii_lowercase()
            .contains("no such volume");
        if !removed_volume.success && !volume_missing {
            return Err(DockerError::CommandFailed(format!(
                "docker volume rm {}: {}",
                spec.volume_name,
                removed_volume.stderr_text()
            )));
        }

        self.require_success(spec.run_args(), &format!("docker run {}", spec.name))
            .await?;
        Ok(())
    }

    async fn remove_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        validate_managed_port(spec.port)?;
        let output = self
            .command(vec!["rm".into(), "-f".into(), spec.name.clone()])
            .await?;
        if output.success
            || output.stderr_text().contains("No such container")
            || output.stderr_text().contains("No such object")
        {
            Ok(())
        } else {
            Err(DockerError::CommandFailed(format!(
                "docker rm {}: {}",
                spec.name,
                output.stderr_text()
            )))
        }
    }

    async fn restart_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        validate_startable_port(spec.port)?;
        self.require_success(
            vec!["restart".into(), spec.name.clone()],
            &format!("docker restart {}", spec.name),
        )
        .await?;
        Ok(())
    }

    async fn stop_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        validate_managed_port(spec.port)?;
        self.require_success(
            vec!["stop".into(), "-t".into(), "5".into(), spec.name.clone()],
            &format!("docker stop {}", spec.name),
        )
        .await?;
        Ok(())
    }

    async fn start_managed(&self, spec: &ProxySpec) -> DockerResult<()> {
        validate_startable_port(spec.port)?;
        self.require_success(
            vec!["start".into(), spec.name.clone()],
            &format!("docker start {}", spec.name),
        )
        .await?;
        Ok(())
    }

    async fn verify_online(&self, spec: &ProxySpec) -> bool {
        for _ in 0..15 {
            if super::health::verify_proxy(spec.port).await {
                return true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        false
    }

    async fn logs(&self, spec: &ProxySpec, tail: usize) -> DockerResult<String> {
        validate_known_port(spec.port)?;
        let output = self
            .command(vec![
                "logs".into(),
                "--tail".into(),
                tail.to_string(),
                spec.name.clone(),
            ])
            .await?;
        if output.success {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            Ok(match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
                (false, false) => format!("{}\n{}", stdout.trim_end(), stderr.trim_end()),
                (false, true) => stdout.to_string(),
                (true, false) => stderr.to_string(),
                (true, true) => String::new(),
            })
        } else {
            Err(DockerError::CommandFailed(format!(
                "docker logs {}: {}",
                spec.name,
                output.stderr_text()
            )))
        }
    }

    async fn list(&self, specs: &[ProxySpec]) -> DockerResult<Vec<ContainerSummary>> {
        let mut summaries = Vec::with_capacity(specs.len());
        for spec in specs {
            let state = self.inspect(spec).await?;
            summaries.push(ContainerSummary {
                port: spec.port,
                name: spec.name.clone(),
                running: state.running,
            });
        }
        Ok(summaries)
    }
}

pub async fn ensure_proxy(
    runtime: &dyn ContainerRuntime,
    spec: &ProxySpec,
) -> DockerResult<ContainerSetupState> {
    let state = runtime.inspect(spec).await?;
    if spec.is_protected() {
        return match state {
            ContainerState { exists: false, .. } => {
                runtime.create_missing(spec).await?;
                Ok(ContainerSetupState::New)
            }
            ContainerState {
                running: true,
                has_expected_volume: true,
                ..
            } => Ok(ContainerSetupState::Running),
            ContainerState { running: false, .. } => {
                runtime.start_managed(spec).await?;
                Ok(ContainerSetupState::Resumed)
            }
            _ => Ok(ContainerSetupState::ProtectedLegacy),
        };
    }

    match state {
        ContainerState { exists: false, .. } => {
            runtime.create_missing(spec).await?;
            Ok(ContainerSetupState::New)
        }
        ContainerState {
            running: true,
            has_expected_volume: true,
            ..
        } => Ok(ContainerSetupState::Running),
        ContainerState {
            running: false,
            has_expected_volume: true,
            ..
        } => {
            runtime.start_managed(spec).await?;
            Ok(ContainerSetupState::Resumed)
        }
        _ => {
            runtime.recreate_managed(spec).await?;
            Ok(ContainerSetupState::Migrated)
        }
    }
}

pub fn default_runtime() -> DockerCliRuntime {
    let config = BridgeConfig::from_env_and_cli(Default::default());
    DockerCliRuntime::from_config(&config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::command::testing::RecordingCommandRunner;
    use crate::infrastructure::command::CommandOutput;

    #[tokio::test]
    async fn recreate_uses_canonical_proxy_spec_once() {
        let runner = RecordingCommandRunner::with_outputs(vec![
            CommandOutput::successful(Vec::new()),
            CommandOutput::successful(b"container-id\n".to_vec()),
        ]);
        let runtime = DockerCliRuntime::with_runner(
            "docker-test",
            "example/warp:1",
            Arc::new(runner.clone()),
        );
        let spec = runtime.proxy_spec(40001).expect("spec");
        runtime.recreate_managed(&spec).await.expect("recreate");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, vec!["rm", "-f", "opencode-warp-1"]);
        assert_eq!(calls[1].args, spec.run_args());
        assert_eq!(calls[1].program, "docker-test");
    }

    #[tokio::test]
    async fn rotate_managed_replaces_container_and_registration_volume() {
        let runner = RecordingCommandRunner::with_outputs(vec![
            CommandOutput::successful(Vec::new()),
            CommandOutput::successful(Vec::new()),
            CommandOutput::successful(b"container-id\n".to_vec()),
        ]);
        let runtime = DockerCliRuntime::with_runner(
            "docker-test",
            "example/warp:1",
            Arc::new(runner.clone()),
        );
        let spec = runtime.proxy_spec(40001).expect("spec");

        runtime.rotate_managed(&spec).await.expect("rotate");

        let calls = runner.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].args, vec!["rm", "-f", "opencode-warp-1"]);
        assert_eq!(
            calls[1].args,
            vec!["volume", "rm", "-f", "opencode-warp-1-config"]
        );
        assert_eq!(calls[2].args, spec.run_args());
    }

    #[tokio::test]
    async fn protected_destructive_actions_do_not_reach_runner() {
        let runner = RecordingCommandRunner::default();
        let runtime = DockerCliRuntime::with_runner(
            "docker-test",
            "example/warp:1",
            Arc::new(runner.clone()),
        );
        let spec = runtime.proxy_spec(40004).expect("spec");
        assert!(matches!(
            runtime.recreate_managed(&spec).await,
            Err(DockerError::Protected(_))
        ));
        assert!(matches!(
            runtime.rotate_managed(&spec).await,
            Err(DockerError::Protected(_))
        ));
        assert!(matches!(
            runtime.remove_managed(&spec).await,
            Err(DockerError::Protected(_))
        ));
        assert!(matches!(
            runtime.stop_managed(&spec).await,
            Err(DockerError::Protected(_))
        ));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn protected_start_and_restart_reach_runner() {
        let runner = RecordingCommandRunner::with_outputs(vec![
            CommandOutput::successful(Vec::new()),
            CommandOutput::successful(Vec::new()),
        ]);
        let runtime = DockerCliRuntime::with_runner(
            "docker-test",
            "example/warp:1",
            Arc::new(runner.clone()),
        );
        let spec = runtime.proxy_spec(40004).expect("spec");
        runtime.start_managed(&spec).await.expect("start");
        runtime.restart_managed(&spec).await.expect("restart");
        let calls = runner.calls();
        assert_eq!(calls[0].args, vec!["start", "opencode-warp-4"]);
        assert_eq!(calls[1].args, vec!["restart", "opencode-warp-4"]);
    }

    #[tokio::test]
    async fn protected_stopped_node_is_resumed_by_ensure() {
        let runner = RecordingCommandRunner::with_outputs(vec![
            CommandOutput::successful(b"false|opencode-warp-4-config\n".to_vec()),
            CommandOutput::successful(Vec::new()),
        ]);
        let runtime = DockerCliRuntime::with_runner(
            "docker-test",
            "example/warp:1",
            Arc::new(runner.clone()),
        );
        let spec = runtime.proxy_spec(40004).expect("spec");
        assert_eq!(
            ensure_proxy(&runtime, &spec).await.expect("ensure"),
            ContainerSetupState::Resumed
        );
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].args, vec!["start", "opencode-warp-4"]);
    }

    #[tokio::test]
    async fn protected_stopped_legacy_volume_is_still_started() {
        let runner = RecordingCommandRunner::with_outputs(vec![
            CommandOutput::successful(b"false|legacy-volume\n".to_vec()),
            CommandOutput::successful(Vec::new()),
        ]);
        let runtime = DockerCliRuntime::with_runner(
            "docker-test",
            "example/warp:1",
            Arc::new(runner.clone()),
        );
        let spec = runtime.proxy_spec(40005).expect("spec");
        assert_eq!(
            ensure_proxy(&runtime, &spec).await.expect("ensure"),
            ContainerSetupState::Resumed
        );
        let calls = runner.calls();
        assert_eq!(calls[1].args, vec!["start", "opencode-warp-5"]);
    }
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct FakeRuntime {
        state: ContainerState,
        actions: Mutex<Vec<&'static str>>,
    }

    impl FakeRuntime {
        fn new(state: ContainerState) -> Self {
            Self {
                state,
                actions: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ContainerRuntime for FakeRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".into())
        }
        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
            Ok(self.state.clone())
        }
        async fn create_missing(&self, _spec: &ProxySpec) -> DockerResult<()> {
            self.actions.lock().unwrap().push("create");
            Ok(())
        }
        async fn recreate_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            self.actions.lock().unwrap().push("recreate");
            Ok(())
        }
        async fn remove_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn restart_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn stop_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }
        async fn start_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            self.actions.lock().unwrap().push("start");
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
    async fn reconcile_creates_only_when_missing() {
        let runtime = FakeRuntime::new(ContainerState {
            exists: false,
            running: false,
            has_expected_volume: false,
        });
        let spec = ProxySpec::new(40001, "warp:test").unwrap();
        assert_eq!(
            ensure_proxy(&runtime, &spec).await.unwrap(),
            ContainerSetupState::New
        );
        assert_eq!(*runtime.actions.lock().unwrap(), vec!["create"]);
    }

    #[tokio::test]
    async fn reconcile_resumes_stopped_volume_cached_primary() {
        let runtime = FakeRuntime::new(ContainerState {
            exists: true,
            running: false,
            has_expected_volume: true,
        });
        let spec = ProxySpec::new(40001, "warp:test").unwrap();
        assert_eq!(
            ensure_proxy(&runtime, &spec).await.unwrap(),
            ContainerSetupState::Resumed
        );
        assert_eq!(*runtime.actions.lock().unwrap(), vec!["start"]);
    }

    #[tokio::test]
    async fn reconcile_migrates_managed_legacy_container() {
        let runtime = FakeRuntime::new(ContainerState {
            exists: true,
            running: true,
            has_expected_volume: false,
        });
        let spec = ProxySpec::new(40001, "warp:test").unwrap();
        assert_eq!(
            ensure_proxy(&runtime, &spec).await.unwrap(),
            ContainerSetupState::Migrated
        );
        assert_eq!(*runtime.actions.lock().unwrap(), vec!["recreate"]);
    }

    #[tokio::test]
    async fn malformed_inspect_output_is_rejected() {
        use crate::infrastructure::command::testing::RecordingCommandRunner;
        use crate::infrastructure::command::CommandOutput;
        let runner = RecordingCommandRunner::with_outputs(vec![CommandOutput::successful(
            b"not-a-valid-inspect-record".to_vec(),
        )]);
        let runtime = DockerCliRuntime::with_runner("docker-test", "warp:test", Arc::new(runner));
        let error = runtime
            .inspect(&runtime.proxy_spec(40001).unwrap())
            .await
            .expect_err("malformed output must fail");
        assert!(matches!(error, DockerError::InvalidResponse(_)));
    }
}
