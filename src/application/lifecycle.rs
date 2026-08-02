//! Deferred fixed-command lifecycle actions for the running dashboard process.

use crate::infrastructure::process::{ProcessManager, ProcessSpec, SystemProcessManager};
use crate::pidfile::PidFile;
use crate::runtime::RuntimePaths;
use crate::state::AppState;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerAction {
    Restart,
    Stop,
}

pub fn schedule_server_action(state: &AppState, action: ServerAction) -> Result<String, String> {
    // The dashboard may have been launched directly through `opencode2api-serve`
    // instead of the supervisor CLI. Adopt only this exact running process into
    // the PID file before asking the detached CLI to stop/restart it. The
    // process identity marker prevents a stale or reused PID from being killed.
    ensure_current_server_is_supervisor_managed(state)?;

    let (label, args) = match action {
        ServerAction::Restart => (
            "server-restart",
            vec!["server".to_string(), "restart".to_string()],
        ),
        ServerAction::Stop => (
            "server-stop",
            vec!["server".to_string(), "stop".to_string()],
        ),
    };
    schedule_cli_command(state, label, args)
}

pub fn schedule_cli_command(
    state: &AppState,
    label: &str,
    args: Vec<String>,
) -> Result<String, String> {
    let executable = cli_executable()?;
    let log = RuntimePaths::from_config(&state.config).bridge_log();
    let task = state.workers.spawn_ephemeral(label, async move {
        tokio::time::sleep(Duration::from_millis(350)).await;
        let manager = SystemProcessManager;
        let _ = manager.spawn_detached(&ProcessSpec {
            executable,
            args,
            stdout_path: log.clone(),
            stderr_path: log,
        });
    });
    Ok(task)
}

fn ensure_current_server_is_supervisor_managed(state: &AppState) -> Result<(), String> {
    let paths = RuntimePaths::from_config(&state.config);
    paths
        .ensure_dirs()
        .map_err(|error| format!("Failed to prepare runtime directory: {error}"))?;

    let manager = SystemProcessManager;
    let current_pid = std::process::id();
    let current_identity = manager
        .identity(current_pid)
        .map_err(|error| format!("Failed to identify running server process: {error}"))?
        .ok_or_else(|| "Running server process identity is unavailable".to_string())?;
    let pid_path = paths.pid_file();

    if pid_path.exists() {
        match PidFile::read(&pid_path) {
            Ok(existing) => {
                if let Some(actual) = manager
                    .identity(existing.pid)
                    .map_err(|error| format!("Failed to validate supervisor PID file: {error}"))?
                {
                    if existing.owns(&actual) {
                        if existing.pid == current_pid {
                            return Ok(());
                        }
                        return Err(format!(
                            "Refusing to replace an active supervisor PID file owned by process {}",
                            existing.pid
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "Cannot safely read existing supervisor PID file {}: {error}",
                    pid_path.display()
                ));
            }
        }
    }

    let mut pidfile = PidFile::with_identity(
        current_identity,
        state.config.bridge_port,
        state.config.host.to_string(),
    );
    pidfile.started_at = state
        .started_at
        .load(Ordering::Relaxed)
        .saturating_mul(1_000);
    pidfile
        .write(&pid_path)
        .map_err(|error| format!("Failed to adopt running server into supervisor state: {error}"))
}

fn cli_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = current
        .parent()
        .ok_or_else(|| "Cannot locate binary directory".to_string())?;
    let candidate = if current.file_name().and_then(|name| name.to_str()) == Some("opencode2api") {
        current
    } else {
        parent.join("opencode2api")
    };
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(format!("CLI binary not found at {}", candidate.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_runtime_root() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "opencode2api-lifecycle-adoption-{}-{suffix}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn adopts_the_current_server_with_verifiable_identity() {
        let root = temp_runtime_root();
        let mut config = BridgeConfig::default();
        config.runtime.runtime_dir = Some(root.clone());
        config.history.enabled = false;
        let state = AppState::new(config);

        ensure_current_server_is_supervisor_managed(&state).expect("adopt current process");

        let paths = RuntimePaths::from_root(&root);
        let pidfile = PidFile::read(&paths.pid_file()).expect("read adopted PID file");
        let actual = SystemProcessManager
            .identity(std::process::id())
            .expect("inspect current process")
            .expect("current process exists");
        assert_eq!(pidfile.pid, std::process::id());
        assert!(pidfile.owns(&actual));
        assert_eq!(pidfile.port, state.config.bridge_port);

        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }
}
