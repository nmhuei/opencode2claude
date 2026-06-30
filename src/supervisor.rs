//! Bridge supervisor — start, stop, and status commands.
//!
//! `start` spawns `serve` as a background child process, writes its PID,
//! and redirects stdout/stderr to `.runtime/opencode2claude.log`.
//! `stop` reads the PID, kills the process, cleans up the PID file.
//! `status` checks if the PID file exists and the process is alive.

use crate::pidfile::{PidFile, PidFileError};
use crate::runtime::RuntimePaths;
use std::fs::OpenOptions;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Possible states of the bridge supervisor.
pub enum SupervisorStatus {
    /// Bridge is running with the given PID and port.
    Running { pid: u32, port: u16 },
    /// Bridge is not running.
    Stopped,
}

impl SupervisorStatus {
    /// Returns true if the bridge is running.
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

impl std::fmt::Display for SupervisorStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running { pid, port } => {
                write!(f, "Running (PID: {}, port: {})", pid, port)
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

    /// Bridge is not running.
    #[allow(dead_code)]
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
}

impl Supervisor {
    /// Create a new supervisor with the given runtime paths and bind configuration.
    pub fn new(paths: RuntimePaths, port: u16, host: impl Into<String>) -> Self {
        Self {
            paths,
            port,
            host: host.into(),
        }
    }

    /// Start the bridge: create `.runtime/`, spawn `serve` as background child, write PID.
    pub fn start(&self) -> Result<(), SupervisorError> {
        // Check if already running
        let status = self.status()?;
        if let SupervisorStatus::Running { pid, .. } = status {
            return Err(SupervisorError::AlreadyRunning(pid));
        }

        // Ensure runtime directories exist
        self.paths.ensure_dirs()?;

        // Open log file for stdout/stderr (append mode)
        let log_path = self.paths.bridge_log();
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| SupervisorError::SpawnFailed(format!("Cannot open log file: {}", e)))?;

        // Spawn bridge serve as child process (detached)
        let exe = std::env::current_exe()
            .map_err(|e| SupervisorError::SpawnFailed(format!("Cannot get binary path: {}", e)))?;

        use std::os::unix::process::CommandExt;
        let child = unsafe {
            Command::new(&exe)
                .arg("serve")
                .arg("--port")
                .arg(self.port.to_string())
                .arg("--host")
                .arg(&self.host)
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

    /// Check bridge status from PID file + process existence.
    pub fn status(&self) -> Result<SupervisorStatus, SupervisorError> {
        let pidfile_path = self.paths.pid_file();
        if !pidfile_path.exists() {
            return Ok(SupervisorStatus::Stopped);
        }

        let pidfile = PidFile::read(&pidfile_path)?;
        let pid = pidfile.pid;

        if process_exists(pid) {
            Ok(SupervisorStatus::Running {
                pid,
                port: pidfile.port,
            })
        } else {
            // Stale PID file — clean up
            PidFile::remove(&pidfile_path)?;
            Ok(SupervisorStatus::Stopped)
        }
    }
}

/// Check if a process exists on Unix via `/proc/{pid}`.
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}
