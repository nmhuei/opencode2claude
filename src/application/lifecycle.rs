//! Deferred fixed-command lifecycle actions for the running dashboard process.

use crate::infrastructure::process::{
    ProcessIdentity, ProcessManager, ProcessSpec, SystemProcessManager,
};
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
    let manager = SystemProcessManager;
    let current_pid = std::process::id();
    let current_identity = manager
        .identity(current_pid)
        .map_err(|error| format!("Failed to identify running server process: {error}"))?
        .ok_or_else(|| "Running server process identity is unavailable".to_string())?;
    let started_at_millis = state
        .started_at
        .load(Ordering::Relaxed)
        .saturating_mul(1_000);
    adopt_identity_into_pid_file(
        &paths,
        &current_identity,
        state.config.bridge_port,
        state.config.host.to_string().as_str(),
        Some(started_at_millis),
    )
}

/// Adopt `identity` into the supervisor PID file at `paths` so later
/// stop/restart flows can prove ownership (executable + start marker) before
/// signaling. Refuses to replace an active PID file owned by a different,
/// still-verified process. `started_at_override` preserves the original boot
/// timestamp when adopting an already-running server.
fn adopt_identity_into_pid_file(
    paths: &RuntimePaths,
    identity: &ProcessIdentity,
    port: u16,
    host: &str,
    started_at_override: Option<u64>,
) -> Result<(), String> {
    paths
        .ensure_dirs()
        .map_err(|error| format!("Failed to prepare runtime directory: {error}"))?;

    let pid_path = paths.pid_file();

    if pid_path.exists() {
        match PidFile::read(&pid_path) {
            Ok(existing) => {
                if let Some(actual) = SystemProcessManager
                    .identity(existing.pid)
                    .map_err(|error| format!("Failed to validate supervisor PID file: {error}"))?
                {
                    if existing.owns(&actual) {
                        if existing.pid == identity.pid {
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

    let mut pidfile = PidFile::with_identity(identity.clone(), port, host);
    if let Some(started_at) = started_at_override {
        pidfile.started_at = started_at;
    }
    pidfile
        .write(&pid_path)
        .map_err(|error| format!("Failed to adopt running server into supervisor state: {error}"))
}

/// Adopt the process currently listening on `port` into the supervisor PID
/// file at `paths`. The listener's real PID is discovered by cross-referencing
/// LISTEN socket inodes in `/proc/net/tcp{,6}` with `/proc/<pid>/fd` links,
/// and its executable + start marker are captured so subsequent verified
/// signal batches can never hit a reused PID. Returns the adopted PID.
pub fn adopt_listening_process(paths: &RuntimePaths, port: u16, host: &str) -> Result<u32, String> {
    let candidates = discover_port_listener_pids(port)?;
    let mut identities = Vec::new();
    for pid in candidates {
        // Never adopt this process itself: a CLI invocation does not own the
        // listening socket, and self-adoption would poison the PID file.
        if pid == std::process::id() {
            continue;
        }
        if let Some(identity) = SystemProcessManager
            .identity(pid)
            .map_err(|error| error.to_string())?
        {
            identities.push(identity);
        }
    }

    match identities.as_slice() {
        [] => Err(format!(
            "no external process found listening on port {port}"
        )),
        [identity] => {
            adopt_identity_into_pid_file(paths, identity, port, host, None)?;
            Ok(identity.pid)
        }
        multiple => {
            let pids = multiple
                .iter()
                .map(|identity| identity.pid.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "multiple processes share the listening socket on port {port} \
                 (PIDs: {pids}); stop them manually"
            ))
        }
    }
}

/// PIDs holding a TCP LISTEN socket bound to `port`, discovered via Linux
/// `/proc`: parse socket inodes from `/proc/net/tcp{,6}` for the port in
/// LISTEN state (`0A`), then map inode → owner through `/proc/<pid>/fd`.
fn discover_port_listener_pids(port: u16) -> Result<Vec<u32>, String> {
    let mut inodes = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        match std::fs::read_to_string(table) {
            Ok(content) => inodes.extend(parse_listen_socket_inodes(&content, port)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot read {table}: {error}")),
        }
    }

    let mut owners = Vec::new();
    if !inodes.is_empty() {
        let entries =
            std::fs::read_dir("/proc").map_err(|error| format!("Cannot scan /proc: {error}"))?;
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                // Kernel threads and other users' processes hide their fds.
                continue;
            };
            'outer: for fd in fds.flatten() {
                let Ok(link) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                let text = link.to_string_lossy();
                if let Some(inode) = text
                    .strip_prefix("socket:[")
                    .and_then(|rest| rest.strip_suffix(']'))
                {
                    if inodes.iter().any(|known| known == inode) {
                        owners.push(pid);
                        break 'outer;
                    }
                }
            }
        }
    }

    owners.sort_unstable();
    owners.dedup();
    Ok(owners)
}

/// Extract the socket inodes of all LISTEN sockets whose local port is
/// `port` from one `/proc/net/tcp{,6}` table body.
fn parse_listen_socket_inodes(table: &str, port: u16) -> Vec<String> {
    let hex_port = format!("{port:04X}");
    table
        .lines()
        .skip(1) // header line
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 {
                return None;
            }
            let local_port = fields[1].rsplit_once(':')?.1;
            if !local_port.eq_ignore_ascii_case(&hex_port) {
                return None;
            }
            // st column: 0A == TCP_LISTEN.
            if fields[3] != "0A" {
                return None;
            }
            Some(fields[9].to_string())
        })
        .collect()
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

    #[test]
    fn discovers_own_process_holding_a_listen_socket() {
        // Hermetic: the socket belongs to this very test process, so /proc
        // scanning must report our own PID — no child process required.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        let pids = discover_port_listener_pids(port).expect("scan /proc for listeners");

        assert!(
            pids.contains(&std::process::id()),
            "expected own pid {} among {pids:?}",
            std::process::id()
        );
        drop(listener);
    }

    #[test]
    fn discovery_finds_nothing_for_a_closed_port() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let pids = discover_port_listener_pids(port).expect("scan /proc for listeners");
        assert!(
            pids.is_empty(),
            "unexpected listeners on closed port: {pids:?}"
        );
    }

    #[test]
    fn adopt_listening_process_errors_without_any_listener() {
        let root = temp_runtime_root();
        let mut config = BridgeConfig::default();
        config.runtime.runtime_dir = Some(root.clone());
        config.history.enabled = false;
        let state = AppState::new(config);
        let paths = RuntimePaths::from_config(&state.config);

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let error = adopt_listening_process(&paths, port, "127.0.0.1")
            .expect_err("nothing listens on the released port");
        assert!(error.contains(&port.to_string()), "message: {error}");
        assert!(!paths.pid_file().exists());
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }
}
