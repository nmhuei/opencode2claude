//! Bridge supervisor — start, stop, and status commands.
//!
//! `start` spawns `serve` as a background child process, writes its PID,
//! and redirects stdout/stderr to `~/.opencode2api/opencode2api.log`.
//! `stop` reads the PID, kills the process, cleans up the PID file.
//! `status` checks if the PID file exists and the process is alive.

use crate::pidfile::{PidFile, PidFileError};
use crate::runtime::RuntimePaths;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
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
}

/// Supervisor orchestrates the bridge lifecycle.
pub struct Supervisor {
    paths: RuntimePaths,
    port: u16,
    host: String,
    child_args: Vec<String>,
}

impl Supervisor {
    /// Create a new supervisor with the given runtime paths and bind configuration.
    pub fn new(paths: RuntimePaths, port: u16, host: impl Into<String>) -> Self {
        Self {
            paths,
            port,
            host: host.into(),
            child_args: Vec::new(),
        }
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

        // Open log file for stdout/stderr (append mode)
        let log_path = self.paths.bridge_log();
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| SupervisorError::SpawnFailed(format!("Cannot open log file: {}", e)))?;

        // Spawn bridge server as child process (detached)
        let current_exe = std::env::current_exe()
            .map_err(|e| SupervisorError::SpawnFailed(format!("Cannot get binary path: {}", e)))?;
        let exe = current_exe
            .parent()
            .map(|p| p.join("opencode2api-serve"))
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

        use std::os::unix::process::CommandExt;
        let child = unsafe {
            Command::new(&exe)
                .args(&child_args)
                .pre_exec(|| {
                    extern "C" {
                        fn setsid() -> i32;
                    }
                    setsid();
                    Ok(())
                })
                .stdout(log_file.try_clone().map_err(|e| {
                    SupervisorError::SpawnFailed(format!("Cannot clone log fd: {}", e))
                })?)
                .stderr(log_file)
                .spawn()
        }
        .map_err(|e| SupervisorError::SpawnFailed(format!("Cannot spawn serve: {}", e)))?;

        let pid = child.id();

        if let Err(e) = wait_for_health(pid, &self.host, self.port, Duration::from_secs(20)) {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
            let _ = PidFile::remove(&self.paths.pid_file());
            return Err(SupervisorError::SpawnFailed(format!(
                "{}. See log: {}",
                e,
                log_path.display()
            )));
        }

        // Write PID file
        let pidfile = PidFile::new(pid, self.port, &self.host);
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

        // Send SIGTERM
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();

        // Wait briefly for graceful shutdown
        std::thread::sleep(Duration::from_millis(500));

        // Force kill if still alive (check /proc/{pid})
        if process_exists(pid) {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .status();
        }

        // Remove PID file
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

        if process_exists(pid) && healthy {
            Ok(SupervisorStatus::Running {
                pid: Some(pid),
                port: pidfile.port,
                started_at: pidfile.started_at,
                managed: true,
            })
        } else {
            // Stale PID file — clean up. If the service is still healthy on the
            // PID file port, report it as unmanaged rather than falsely stopped.
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

/// Check if a process exists on Unix via `/proc/{pid}`.
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn wait_for_health(pid: u32, host: &str, port: u16, timeout: Duration) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return Err("bridge process exited before becoming healthy");
        }

        if health_check(host, port) {
            std::thread::sleep(Duration::from_millis(100));
            if process_exists(pid) {
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
}
