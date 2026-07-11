//! Bridge supervisor — start, stop, and status commands.
//!
//! `start` spawns `serve` as a background child process, writes its PID,
//! and redirects stdout/stderr to `~/.opencode2api/opencode2api.log`.
//! `stop` reads the PID, kills the process, cleans up the PID file.
//! `status` checks if the PID file exists and the process is alive.

use crate::infrastructure::process::{ProcessManager, ProcessSpec, SystemProcessManager};
use crate::pidfile::{PidFile, PidFileError};
use crate::runtime::RuntimePaths;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Possible states of the bridge supervisor.
pub enum SupervisorStatus {
    /// Bridge is running with the given PID, port, and started-at timestamp.
    ///
    /// `pid == None` means the HTTP service is healthy on the expected port,
    /// but it was not started by this supervisor or the PID file was lost.
    Running {
        pid: Option<u32>,
        port: u16,
        /// Unix epoch millis when the bridge started.
        started_at: u64,
        /// Whether this process is tracked by the supervisor PID file.
        managed: bool,
    },
    /// Bridge is not running.
    Stopped,
}

impl SupervisorStatus {
    /// Returns true if the bridge is running.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

impl std::fmt::Display for SupervisorStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running {
                pid, port, managed, ..
            } => {
                if *managed {
                    write!(f, "Running (PID: {}, port: {})", pid.unwrap_or(0), port)
                } else {
                    write!(f, "Running (unmanaged, port: {})", port)
                }
            }
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

/// Errors from supervisor operations.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// Bridge is already running.
    #[error("Bridge is already running (PID: {0})")]
    AlreadyRunning(u32),

    /// Bridge appears to be running on the requested port but is not supervisor-managed.
    #[error("Bridge is already running on port {0}, but no supervisor PID file tracks it")]
    AlreadyRunningUnmanaged(u16),

    /// Bridge is not running.
    #[allow(dead_code)] // kept for supervisor response matching
    #[error("Bridge is not running")]
    NotRunning,

    /// PID file error.
    #[error("PID file error: {0}")]
    PidFile(#[from] PidFileError),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Spawning the bridge child process failed.
    #[error("Failed to start bridge: {0}")]
    SpawnFailed(String),

    /// PID exists but does not match the process identity captured at start.
    #[error("Refusing to terminate PID {0}: process identity does not match the managed bridge")]
    OwnershipMismatch(u32),

    /// Legacy/corrupt PID metadata cannot prove ownership safely.
    #[error("Refusing to terminate PID {0}: PID file has no verifiable process identity")]
    OwnershipUnverified(u32),
}

/// Supervisor orchestrates the bridge lifecycle.
pub struct Supervisor {
    paths: RuntimePaths,
    port: u16,
    host: String,
    child_args: Vec<String>,
    process_manager: Arc<dyn ProcessManager>,
}

impl Supervisor {
    /// Create a new supervisor with the given runtime paths and bind configuration.
    pub fn new(paths: RuntimePaths, port: u16, host: impl Into<String>) -> Self {
        Self {
            paths,
            port,
            host: host.into(),
            child_args: Vec::new(),
            process_manager: Arc::new(SystemProcessManager),
        }
    }

    pub fn with_process_manager(mut self, process_manager: Arc<dyn ProcessManager>) -> Self {
        self.process_manager = process_manager;
        self
    }

    /// Override the argv used for the foreground child process.
    pub fn with_child_args(mut self, child_args: Vec<String>) -> Self {
        self.child_args = child_args;
        self
    }

    /// Start the bridge: create `~/.opencode2api/`, spawn a foreground server
    /// as background child, wait until it is healthy, then write PID.
    pub fn start(&self) -> Result<(), SupervisorError> {
        // Check if already running
        let status = self.status()?;
        if let SupervisorStatus::Running {
            pid, port, managed, ..
        } = status
        {
            if managed {
                return Err(SupervisorError::AlreadyRunning(pid.unwrap_or(0)));
            }
            return Err(SupervisorError::AlreadyRunningUnmanaged(port));
        }

        // Ensure runtime directories exist
        self.paths.ensure_dirs()?;

        // Fail fast before spawning if the requested bind address is unavailable.
        match TcpListener::bind((self.host.as_str(), self.port)) {
            Ok(listener) => drop(listener),
            Err(e) => {
                return Err(SupervisorError::SpawnFailed(format!(
                    "Cannot bind to {}:{}: {}",
                    self.host, self.port, e
                )));
            }
        }

        let log_path = self.paths.bridge_log();
        let current_exe = std::env::current_exe()
            .map_err(|e| SupervisorError::SpawnFailed(format!("Cannot get binary path: {e}")))?;
        let executable = current_exe
            .parent()
            .map(|parent| parent.join("opencode2api-serve"))
            .ok_or_else(|| {
                SupervisorError::SpawnFailed(
                    "Cannot determine parent directory of current exe".to_string(),
                )
            })?;
        let child_args = if self.child_args.is_empty() {
            vec![
                "--port".to_string(),
                self.port.to_string(),
                "--host".to_string(),
                self.host.clone(),
            ]
        } else {
            self.child_args.clone()
        };
        let identity = self
            .process_manager
            .spawn_detached(&ProcessSpec {
                executable,
                args: child_args,
                stdout_path: log_path.clone(),
                stderr_path: log_path.clone(),
            })
            .map_err(|error| {
                SupervisorError::SpawnFailed(format!("Cannot spawn serve: {error}"))
            })?;
        let pid = identity.pid;

        if let Err(e) = wait_for_health(
            self.process_manager.as_ref(),
            pid,
            &self.host,
            self.port,
            Duration::from_secs(20),
        ) {
            let _ = self.process_manager.terminate(pid);
            let _ = PidFile::remove(&self.paths.pid_file());
            return Err(SupervisorError::SpawnFailed(format!(
                "{}. See log: {}",
                e,
                log_path.display()
            )));
        }

        // Persist the exact process identity captured after spawn so a reused
        // PID can never be mistaken for the managed bridge.
        let pidfile = PidFile::with_identity(identity, self.port, &self.host);
        pidfile.write(&self.paths.pid_file())?;

        Ok(())
    }

    /// Stop the bridge: send SIGTERM, wait briefly, SIGKILL if needed, clean up PID file.
    pub fn stop(&self) -> Result<(), SupervisorError> {
        let pidfile_path = self.paths.pid_file();
        if !pidfile_path.exists() {
            return Ok(());
        }

        let pidfile = PidFile::read(&pidfile_path)?;
        let pid = pidfile.pid;
        if !pidfile.has_identity_evidence() {
            return Err(SupervisorError::OwnershipUnverified(pid));
        }
        let Some(actual) = self.process_manager.identity(pid)? else {
            PidFile::remove(&pidfile_path)?;
            return Ok(());
        };
        if !pidfile.owns(&actual) {
            return Err(SupervisorError::OwnershipMismatch(pid));
        }

        self.process_manager.terminate(pid)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && self.process_manager.exists(pid) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.process_manager.exists(pid) {
            // Re-check ownership immediately before the destructive fallback.
            let Some(actual) = self.process_manager.identity(pid)? else {
                PidFile::remove(&pidfile_path)?;
                return Ok(());
            };
            if !pidfile.owns(&actual) {
                return Err(SupervisorError::OwnershipMismatch(pid));
            }
            self.process_manager.force_kill(pid)?;
        }

        PidFile::remove(&pidfile_path)?;
        Ok(())
    }

    /// Check bridge status from PID file + process existence + `/health`.
    pub fn status(&self) -> Result<SupervisorStatus, SupervisorError> {
        let pidfile_path = self.paths.pid_file();

        if !pidfile_path.exists() {
            return Ok(self.status_from_health_probe());
        }

        let pidfile = PidFile::read(&pidfile_path)?;
        let pid = pidfile.pid;
        let healthy = health_check(&pidfile.host, pidfile.port);
        let identity = self.process_manager.identity(pid)?;
        let owned = identity.as_ref().is_some_and(|actual| pidfile.owns(actual));

        if owned && healthy {
            Ok(SupervisorStatus::Running {
                pid: Some(pid),
                port: pidfile.port,
                started_at: pidfile.started_at,
                managed: true,
            })
        } else {
            // Never treat a merely-existing PID as managed. Remove stale or
            // unverifiable metadata and report a healthy socket as unmanaged.
            PidFile::remove(&pidfile_path)?;
            if healthy {
                Ok(SupervisorStatus::Running {
                    pid: None,
                    port: pidfile.port,
                    started_at: now_millis(),
                    managed: false,
                })
            } else {
                Ok(self.status_from_health_probe())
            }
        }
    }

    fn status_from_health_probe(&self) -> SupervisorStatus {
        if health_check(&self.host, self.port) {
            SupervisorStatus::Running {
                pid: None,
                port: self.port,
                started_at: now_millis(),
                managed: false,
            }
        } else {
            SupervisorStatus::Stopped
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn wait_for_health(
    process_manager: &dyn ProcessManager,
    pid: u32,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_manager.exists(pid) {
            return Err("bridge process exited before becoming healthy");
        }

        if health_check(host, port) {
            std::thread::sleep(Duration::from_millis(100));
            if process_manager.exists(pid) {
                return Ok(());
            }
            return Err("bridge process exited after health check");
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    Err("bridge did not become healthy before timeout")
}

fn health_check(host: &str, port: u16) -> bool {
    let connect_host = match host {
        "0.0.0.0" | "::" => "127.0.0.1",
        h => h,
    };
    let Ok(mut stream) = TcpStream::connect((connect_host, port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));

    let request = format!(
        "GET /health HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        connect_host, port
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = [0_u8; 128];
    match stream.read(&mut response) {
        Ok(n) => {
            response[..n].starts_with(b"HTTP/1.1 200") || response[..n].starts_with(b"HTTP/1.0 200")
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimePaths;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn temp_runtime_root(name: &str) -> std::path::PathBuf {
        let mut root = std::env::temp_dir();
        root.push(format!(
            "opencode2api-supervisor-test-{}-{}",
            name,
            now_millis()
        ));
        let _ = std::fs::create_dir_all(&root);
        root
    }

    fn spawn_one_health_server() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 256];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}",
                );
            }
        });
        port
    }

    #[test]
    fn process_probe_detects_current_process() {
        assert!(SystemProcessManager.exists(std::process::id()));
    }

    #[test]
    fn process_probe_rejects_impossible_pid() {
        assert!(!SystemProcessManager.exists(u32::MAX));
    }

    #[test]
    fn status_reports_unmanaged_running_when_health_ok_without_pidfile() {
        let root = temp_runtime_root("unmanaged");
        let port = spawn_one_health_server();
        let sup = Supervisor::new(RuntimePaths::from_root(&root), port, "127.0.0.1");

        match sup.status().unwrap() {
            SupervisorStatus::Running {
                pid,
                port: reported_port,
                managed,
                ..
            } => {
                assert_eq!(pid, None);
                assert_eq!(reported_port, port);
                assert!(!managed);
            }
            SupervisorStatus::Stopped => panic!("healthy unmanaged server was reported stopped"),
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn status_cleans_stale_pidfile_and_falls_back_to_health() {
        let root = temp_runtime_root("stale");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        let port = spawn_one_health_server();
        PidFile::new(u32::MAX, port, "127.0.0.1")
            .write(&paths.pid_file())
            .unwrap();
        let sup = Supervisor::new(paths, port, "127.0.0.1");

        match sup.status().unwrap() {
            SupervisorStatus::Running { pid, managed, .. } => {
                assert_eq!(pid, None);
                assert!(!managed);
            }
            SupervisorStatus::Stopped => {
                panic!("healthy service behind stale pidfile was reported stopped")
            }
        }

        assert!(!root.join(crate::runtime::PID_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Debug)]
    struct FakeProcessManager {
        identity: std::sync::Mutex<Option<crate::infrastructure::process::ProcessIdentity>>,
        terminate_calls: std::sync::Mutex<Vec<u32>>,
        force_calls: std::sync::Mutex<Vec<u32>>,
    }

    impl FakeProcessManager {
        fn new(identity: crate::infrastructure::process::ProcessIdentity) -> Self {
            Self {
                identity: std::sync::Mutex::new(Some(identity)),
                terminate_calls: std::sync::Mutex::new(Vec::new()),
                force_calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ProcessManager for FakeProcessManager {
        fn spawn_detached(
            &self,
            _spec: &ProcessSpec,
        ) -> std::io::Result<crate::infrastructure::process::ProcessIdentity> {
            self.identity
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| std::io::Error::other("no identity"))
        }

        fn identity(
            &self,
            _pid: u32,
        ) -> std::io::Result<Option<crate::infrastructure::process::ProcessIdentity>> {
            Ok(self.identity.lock().unwrap().clone())
        }

        fn terminate(&self, pid: u32) -> std::io::Result<()> {
            self.terminate_calls.lock().unwrap().push(pid);
            *self.identity.lock().unwrap() = None;
            Ok(())
        }

        fn force_kill(&self, pid: u32) -> std::io::Result<()> {
            self.force_calls.lock().unwrap().push(pid);
            *self.identity.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn stop_refuses_reused_pid_without_sending_signal() {
        let root = temp_runtime_root("ownership-mismatch");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        let executable = std::env::current_exe().unwrap();
        let expected = crate::infrastructure::process::ProcessIdentity {
            pid: 4242,
            executable: Some(executable.clone()),
            start_marker: Some("old-start".to_string()),
        };
        PidFile::with_identity(expected, 4000, "127.0.0.1")
            .write(&paths.pid_file())
            .unwrap();
        let actual = crate::infrastructure::process::ProcessIdentity {
            pid: 4242,
            executable: Some(executable),
            start_marker: Some("new-start".to_string()),
        };
        let manager = Arc::new(FakeProcessManager::new(actual));
        let supervisor =
            Supervisor::new(paths.clone(), 4000, "127.0.0.1").with_process_manager(manager.clone());

        assert!(matches!(
            supervisor.stop(),
            Err(SupervisorError::OwnershipMismatch(4242))
        ));
        assert!(manager.terminate_calls.lock().unwrap().is_empty());
        assert!(manager.force_calls.lock().unwrap().is_empty());
        assert!(paths.pid_file().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stop_terminates_only_matching_identity_and_removes_pidfile() {
        let root = temp_runtime_root("ownership-match");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        let identity = crate::infrastructure::process::ProcessIdentity {
            pid: 4343,
            executable: Some(std::env::current_exe().unwrap()),
            start_marker: Some("same-start".to_string()),
        };
        PidFile::with_identity(identity.clone(), 4000, "127.0.0.1")
            .write(&paths.pid_file())
            .unwrap();
        let manager = Arc::new(FakeProcessManager::new(identity));
        let supervisor =
            Supervisor::new(paths.clone(), 4000, "127.0.0.1").with_process_manager(manager.clone());

        supervisor.stop().unwrap();
        assert_eq!(*manager.terminate_calls.lock().unwrap(), vec![4343]);
        assert!(manager.force_calls.lock().unwrap().is_empty());
        assert!(!paths.pid_file().exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
