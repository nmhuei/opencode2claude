//! Bridge supervisor — start, stop, and status commands.
//!
//! `start` spawns `serve` as a background child process, writes its PID,
//! and redirects stdout/stderr to `~/.opencode2claude/opencode2claude.log`.
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
    /// Bridge is running with the given PID, port, and started-at timestamp.
    Running {
        pid: u32,
        port: u16,
        /// Unix epoch millis when the bridge started.
        started_at: u64,
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
            Self::Running { pid, port, .. } => {
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

/// Optional spawn arguments for the embedded `serve` subcommand.
///
/// Only `Some` values are forwarded as CLI flags; `None` values are omitted
/// so the child falls through to its own config chain (env → toml → default).
#[derive(Debug, Clone, Default)]
pub struct DaemonSpawnOptions {
    pub config: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

/// Supervisor orchestrates the bridge lifecycle.
pub struct Supervisor {
    paths: RuntimePaths,
    port: u16,
    host: String,
    spawn_opts: DaemonSpawnOptions,
}

impl Supervisor {
    /// Create a new supervisor with the given runtime paths and bind configuration.
    pub fn new(paths: RuntimePaths, port: u16, host: impl Into<String>) -> Self {
        Self {
            paths,
            port,
            host: host.into(),
            spawn_opts: DaemonSpawnOptions::default(),
        }
    }

    /// Set optional spawn arguments forwarded to the `serve` child subcommand.
    pub fn with_spawn_options(mut self, opts: DaemonSpawnOptions) -> Self {
        self.spawn_opts = opts;
        self
    }

    /// Start the bridge: create `~/.opencode2claude/`, spawn `serve` as background child, write PID.
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
        let mut cmd = Command::new(&exe);
        cmd.arg("serve")
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--host")
            .arg(&self.host);

        // Forward all optional spawn options to the child
        push_optional_arg(&mut cmd, "--config", &self.spawn_opts.config);
        push_optional_arg(&mut cmd, "--model", &self.spawn_opts.model);
        push_optional_arg(&mut cmd, "--shell-policy", &self.spawn_opts.shell_policy);
        push_optional_arg(
            &mut cmd,
            "--tavily-api-key",
            &self.spawn_opts.tavily_api_key,
        );
        push_optional_arg(&mut cmd, "--exa-api-key", &self.spawn_opts.exa_api_key);
        push_optional_arg(
            &mut cmd,
            "--serper-api-key",
            &self.spawn_opts.serper_api_key,
        );
        push_optional_arg(&mut cmd, "--searxng-url", &self.spawn_opts.searxng_url);
        push_optional_arg(
            &mut cmd,
            "--searxng-api-key",
            &self.spawn_opts.searxng_api_key,
        );
        push_optional_arg(&mut cmd, "--tls-cert", &self.spawn_opts.tls_cert);
        push_optional_arg(&mut cmd, "--tls-key", &self.spawn_opts.tls_key);

        let child =
            unsafe {
                cmd.pre_exec(|| {
                    // SAFETY: setsid() in pre_exec:
                    // 1. Fork has happened — we are in the child.
                    // 2. No threads yet in child.
                    // 3. child is not a process group leader, so setsid() succeeds.
                    // 4. Both are async-signal-safe per POSIX.
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

        // Send SIGTERM via direct syscall to avoid TOCTOU race with PID reuse
        let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };

        // Wait briefly for graceful shutdown
        std::thread::sleep(Duration::from_millis(500));

        // Force kill if still alive (check /proc/{pid})
        if process_exists(pid) {
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
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
                started_at: pidfile.started_at,
            })
        } else {
            // Stale PID file — clean up
            PidFile::remove(&pidfile_path)?;
            Ok(SupervisorStatus::Stopped)
        }
    }
}

/// Append an optional flag+value pair to `cmd` when the value is `Some`.
pub(crate) fn push_optional_arg(cmd: &mut Command, flag: &str, value: &Option<String>) {
    if let Some(v) = value {
        cmd.arg(flag).arg(v);
    }
}

/// Check if a process exists on Unix via `/proc/{pid}`.
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}
