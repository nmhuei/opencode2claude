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

    /// Exit code contract for `server status`:
    /// - `0`: running and tracked by this supervisor's PID file;
    /// - `3`: answering on the probed configured port but untracked by any
    ///   PID file (unmanaged fallback — possibly a different instance);
    /// - `1`: stopped.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Running { managed: true, .. } => 0,
            Self::Running { managed: false, .. } => 3,
            Self::Stopped => 1,
        }
    }

    /// Single-line machine-readable label used by `--quiet` output. The
    /// unmanaged fallback must name itself and show the probed port so a
    /// stopped instance never masquerades as a plain managed "running".
    pub fn quiet_label(&self) -> String {
        match self {
            Self::Running { managed: true, .. } => "running".to_string(),
            Self::Running {
                managed: false,
                port,
                ..
            } => format!("running (unmanaged, port {port})"),
            Self::Stopped => "stopped".to_string(),
        }
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

/// Exit code used by `server stop` when it refuses to act on an untracked
/// listener. Deliberately distinct from generic failure (1) so shell callers
/// can separate "refused for safety" from "tried and failed".
pub const STOP_REFUSED_UNMANAGED_EXIT_CODE: i32 = 4;

/// Single source of truth for the untracked-listener refusal guidance.
///
/// [`SupervisorError::UnmanagedListener`] is raised from the supervisor's stop
/// phase, so its Display renders the *stop* override command; flows that reach
/// the same refusal through a restart must render the template with
/// `operation = "restart"` so users are pointed at the command they actually
/// ran (see [`SupervisorError::refusal_message`]).
fn unmanaged_listener_refusal(operation: &str, detail: &str) -> String {
    format!(
        "refusing to {operation}: no supervisor PID file tracks this gateway, but {detail}. \
         It may be a process started outside the supervisor. Run \
         `opencode2api server {operation} --unmanaged` to verify that listener's \
         identity and adopt it into supervisor state."
    )
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

    /// The configured port is answering, but no PID file tracks the listener.
    /// Stopping would act on an unverified process, so it is refused unless
    /// the caller explicitly opts in via `stop_adopting_unmanaged`.
    ///
    /// Display renders the shared refusal template for the *stop* operation;
    /// restart flows re-render it for their own verb via
    /// [`SupervisorError::refusal_message`].
    #[error("{}", unmanaged_listener_refusal("stop", .detail))]
    UnmanagedListener { port: u16, detail: String },

    /// Adopting an unmanaged listener into supervisor state failed before any
    /// signal was sent; the listener was left untouched.
    #[error("Cannot adopt the unmanaged listener on port {port}: {detail}")]
    AdoptionFailed { port: u16, detail: String },
}

impl SupervisorError {
    /// Refusal guidance for the untracked-listener case, naming `operation`'s
    /// own `--unmanaged` override command.
    ///
    /// The variant's Display always says `stop` because the supervisor only
    /// raises it from its stop phase; restart flows call this with `"restart"`
    /// so the guidance advertises the command the user actually ran. Returns
    /// `None` for every other error, whose Display is already accurate.
    pub fn refusal_message(&self, operation: &str) -> Option<String> {
        match self {
            Self::UnmanagedListener { detail, .. } => {
                Some(unmanaged_listener_refusal(operation, detail))
            }
            _ => None,
        }
    }
}

/// Supervisor orchestrates the bridge lifecycle.
pub struct Supervisor {
    paths: RuntimePaths,
    port: u16,
    host: String,
    child_args: Vec<String>,
    child_environment: Vec<(String, String)>,
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
            child_environment: Vec::new(),
            process_manager: Arc::new(SystemProcessManager),
        }
    }

    pub fn with_process_manager(mut self, process_manager: Arc<dyn ProcessManager>) -> Self {
        self.process_manager = process_manager;
        self
    }

    /// Clean up after a failed start by removing the PID file only when it
    /// still describes OUR failed child. When two `server start` invocations
    /// race, the loser's health probe can be answered by the winner's child
    /// while its own child dies on the bind race; an unconditional removal
    /// here would delete the winner's freshly written PID file and leave a
    /// live bridge untracked.
    fn discard_failed_start(&self, pid: u32) {
        let path = self.paths.pid_file();
        match PidFile::read(&path) {
            Ok(existing) if existing.pid == pid => {
                let _ = PidFile::remove(&path);
            }
            // A foreign or unreadable entry belongs to someone else's
            // lifecycle — never touch it from a failed start.
            _ => {}
        }
    }

    /// Override the argv used for the foreground child process.
    pub fn with_child_args(mut self, child_args: Vec<String>) -> Self {
        self.child_args = child_args;
        self
    }

    /// Override environment variables injected into the detached server child.
    /// Secrets belong here instead of argv so they are not exposed via ps or
    /// /proc/<pid>/cmdline.
    pub fn with_child_environment(mut self, child_environment: Vec<(String, String)>) -> Self {
        self.child_environment = child_environment;
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
                environment: self.child_environment.clone(),
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
            self.discard_failed_start(pid);
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
    ///
    /// Honest-stop contract: with no PID file, a live listener on the
    /// configured port is refused ([`SupervisorError::UnmanagedListener`])
    /// instead of being silently ignored; only a silent port reports success.
    pub fn stop(&self) -> Result<(), SupervisorError> {
        self.stop_inner(false)
    }

    /// Same as [`Supervisor::stop`], but when no PID file exists yet something
    /// listens on the configured port, first adopt that listener into the
    /// supervisor PID file — verifying its executable and start marker via
    /// `/proc` through [`crate::application::lifecycle::adopt_listening_process`]
    /// — and then run the normal verified TERM/wait/KILL flow.
    pub fn stop_adopting_unmanaged(&self) -> Result<(), SupervisorError> {
        self.stop_inner(true)
    }

    fn stop_inner(&self, adopt_unmanaged: bool) -> Result<(), SupervisorError> {
        let pidfile_path = self.paths.pid_file();
        if !pidfile_path.exists() {
            return self.stop_without_pid_file(adopt_unmanaged);
        }
        self.stop_managed(&pidfile_path)
    }

    /// No-PID-file stop: probe the configured port before claiming anything.
    /// Nothing answering → truthfully stopped. A live listener is either
    /// refused (default) or, on explicit opt-in, adopted first.
    fn stop_without_pid_file(&self, adopt_unmanaged: bool) -> Result<(), SupervisorError> {
        let healthy = health_check(&self.host, self.port);
        if !healthy && !self.port_accepts_connections() {
            // Nothing accepts connections on the configured port.
            return Ok(());
        }

        let detail = if healthy {
            format!("its /health endpoint answered on probed port {}", self.port)
        } else {
            format!(
                "a TCP listener accepted a connection on probed port {}",
                self.port
            )
        };
        if !adopt_unmanaged {
            return Err(SupervisorError::UnmanagedListener {
                port: self.port,
                detail,
            });
        }

        // Explicit opt-in: record verifiable identity evidence for the actual
        // listening process, then fall through to the managed stop flow so the
        // same TERM/wait/KILL + ownership verification applies to it.
        crate::application::lifecycle::adopt_listening_process(&self.paths, self.port, &self.host)
            .map_err(|detail| SupervisorError::AdoptionFailed {
                port: self.port,
                detail,
            })?;
        let pidfile_path = self.paths.pid_file();
        if !pidfile_path.exists() {
            return Err(SupervisorError::AdoptionFailed {
                port: self.port,
                detail: "adoption did not produce a supervisor PID file".to_string(),
            });
        }
        self.stop_managed(&pidfile_path)
    }

    /// Managed stop for an existing PID file: every signal batch re-verifies
    /// PID ownership immediately before signaling, closing the window in which
    /// a reused or exited PID could receive a signal meant for the bridge.
    fn stop_managed(&self, pidfile_path: &std::path::Path) -> Result<(), SupervisorError> {
        let pidfile = PidFile::read(pidfile_path)?;
        let pid = pidfile.pid;
        if !pidfile.has_identity_evidence() {
            return Err(SupervisorError::OwnershipUnverified(pid));
        }

        // Signal batch 1: SIGTERM.
        if !self.verified_signal(&pidfile, pidfile_path, false)? {
            // Process already exited on its own; PID file was removed.
            return Ok(());
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && self.process_manager.exists(pid)
            && !Self::process_is_zombie(pid)
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        // An unreaped zombie means the process accepted its signal and is
        // gone; skip the destructive fallback rather than re-reading a corpse.
        if self.process_manager.exists(pid) && !Self::process_is_zombie(pid) {
            // Signal batch 2: destructive fallback. Re-verify again so a
            // process that died during the wait can never be KILLed through a
            // reused PID.
            self.verified_signal(&pidfile, pidfile_path, true)?;
        }

        PidFile::remove(pidfile_path)?;
        Ok(())
    }

    /// Re-verify PID ownership immediately before sending one signal batch.
    /// Returns `Ok(true)` when the signal was delivered and `Ok(false)` when
    /// the process vanished beforehand (PID file removed; treat as stopped).
    ///
    /// A same-slot corpse is not an ownership violation: when `/proc/<pid>/exe`
    /// has become unreadable while the `/proc` start marker still matches, the
    /// process image is gone and only an unreaped zombie remains — the stop is
    /// already complete, so the PID file is removed and no further signal is
    /// sent. Only a genuinely different occupant of the PID slot (different
    /// start marker — i.e. reuse) raises [`SupervisorError::OwnershipMismatch`].
    fn verified_signal(
        &self,
        pidfile: &PidFile,
        pidfile_path: &std::path::Path,
        force: bool,
    ) -> Result<bool, SupervisorError> {
        let Some(actual) = self.process_manager.identity(pidfile.pid)? else {
            PidFile::remove(pidfile_path)?;
            return Ok(false);
        };
        if !pidfile.owns(&actual) {
            if Self::is_same_slot_corpse(pidfile, &actual) {
                PidFile::remove(pidfile_path)?;
                return Ok(false);
            }
            return Err(SupervisorError::OwnershipMismatch(pidfile.pid));
        }
        if force {
            self.process_manager.force_kill(pidfile.pid)?;
        } else {
            self.process_manager.terminate(pidfile.pid)?;
        }
        Ok(true)
    }

    /// True when `actual` still occupies the exact same process lifetime as
    /// the recorded identity (`/proc` start ticks unchanged) but its
    /// executable link is gone — the signature of an exited-but-unreaped
    /// zombie. Such a process cannot be signalled meaningfully and MUST NOT
    /// be reported as an ownership mismatch.
    fn is_same_slot_corpse(
        expected: &PidFile,
        actual: &crate::infrastructure::process::ProcessIdentity,
    ) -> bool {
        if actual.executable.is_some() {
            return false;
        }
        match (&actual.start_marker, &expected.start_marker) {
            (Some(actual_marker), Some(expected_marker)) => actual_marker == expected_marker,
            _ => false,
        }
    }

    /// True when `/proc` marks the process as a zombie (`state == Z`) — it has
    /// exited but its parent has not reaped it yet. Zombies keep a readable
    /// `/proc/<pid>/stat` (so identity-based existence probes stay `true`)
    /// while their executable link disappears; the wait loop uses this to
    /// recognise completion instead of stalling and then misreading the corpse
    /// as a foreign occupant of the PID slot.
    fn process_is_zombie(pid: u32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // Field 3 (process state) is the first character after the
        // parenthesised command name, which may itself contain spaces and
        // parentheses.
        match stat.rsplit_once(") ") {
            Some((_, remainder)) => remainder.starts_with('Z'),
            None => false,
        }
    }

    /// True when any TCP listener accepts a connection on the configured port,
    /// regardless of whether it answers HTTP. Used as fallback evidence so a
    /// non-HTTP socket is never misreported as "stopped".
    fn port_accepts_connections(&self) -> bool {
        TcpStream::connect((probe_host(&self.host), self.port)).is_ok()
    }

    /// Check bridge status from PID file + process existence + `/health`.
    pub fn status(&self) -> Result<SupervisorStatus, SupervisorError> {
        let pidfile_path = self.paths.pid_file();

        if !pidfile_path.exists() {
            return Ok(self.status_from_health_probe());
        }

        let pidfile = PidFile::read(&pidfile_path)?;
        let pid = pidfile.pid;
        let identity = self.process_manager.identity(pid)?;
        let owned = identity.as_ref().is_some_and(|actual| pidfile.owns(actual));

        if owned {
            // Tracked and alive: report managed regardless of the /health
            // probe. The probe uses a hard 250 ms timeout, so a transient
            // miss under load must never destroy supervisor tracking for a
            // live, ownership-verified process — deleting the PID file here
            // would turn one slow /health response into permanent state loss
            // (later stops refuse with exit 4, restarts abort).
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
            if health_check(&pidfile.host, pidfile.port) {
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
        // The spawned child's handle is dropped without reaping, so a child
        // that dies instantly lingers as a zombie whose /proc entry keeps
        // identity-based existence probes true. Treat the corpse as exited so
        // an exec-time crash fails fast instead of burning the whole timeout.
        if !process_manager.exists(pid) || Supervisor::process_is_zombie(pid) {
            return Err("bridge process exited before becoming healthy");
        }

        if health_check(host, port) {
            std::thread::sleep(Duration::from_millis(100));
            if process_manager.exists(pid) && !Supervisor::process_is_zombie(pid) {
                return Ok(());
            }
            return Err("bridge process exited after health check");
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    Err("bridge did not become healthy before timeout")
}

fn probe_host(host: &str) -> &str {
    match host {
        "0.0.0.0" | "::" => "127.0.0.1",
        h => h,
    }
}

fn health_check(host: &str, port: u16) -> bool {
    let connect_host = probe_host(host);
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
    use std::io::{BufRead, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
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

        let status = sup.status().unwrap();
        let (pid, reported_port, managed) = match &status {
            SupervisorStatus::Running {
                pid, port, managed, ..
            } => (*pid, *port, *managed),
            SupervisorStatus::Stopped => panic!("healthy unmanaged server was reported stopped"),
        };
        assert_eq!(pid, None);
        assert_eq!(reported_port, port);
        assert!(!managed);
        // No-PID-file + probe-success ⇒ the wording must say unmanaged and
        // show which port was probed.
        assert_eq!(
            status.quiet_label(),
            format!("running (unmanaged, port {port})")
        );
        assert_eq!(
            status.to_string(),
            format!("Running (unmanaged, port: {port})")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn quiet_label_keeps_plain_running_only_for_managed() {
        let managed = SupervisorStatus::Running {
            pid: Some(7),
            port: 4000,
            started_at: 0,
            managed: true,
        };
        assert_eq!(managed.quiet_label(), "running");
        assert_eq!(managed.to_string(), "Running (PID: 7, port: 4000)");
        assert_eq!(SupervisorStatus::Stopped.quiet_label(), "stopped");
        assert_eq!(SupervisorStatus::Stopped.to_string(), "Stopped");
    }

    #[test]
    fn exit_code_contract_distinguishes_managed_unmanaged_stopped() {
        let managed = SupervisorStatus::Running {
            pid: Some(1),
            port: 4000,
            started_at: 0,
            managed: true,
        };
        let unmanaged = SupervisorStatus::Running {
            pid: None,
            port: 4000,
            started_at: 0,
            managed: false,
        };
        assert_eq!(managed.exit_code(), 0);
        assert_eq!(unmanaged.exit_code(), 3);
        assert_eq!(SupervisorStatus::Stopped.exit_code(), 1);
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

    /// Reserve an ephemeral port and release it again so every probe against
    /// it is refused immediately (silent port).
    fn grab_silent_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// An ownership-verified process that merely misses the 250 ms /health
    /// probe is still tracked and alive: status must keep reporting it as
    /// managed and MUST NOT delete its PID file. Destroying tracking here
    /// turns a transient health flap into permanent supervisor-state loss
    /// (subsequent stops refuse with exit 4, restarts abort).
    #[test]
    fn status_keeps_pidfile_when_owned_process_misses_health_probe() {
        let root = temp_runtime_root("owned-unhealthy");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        let port = grab_silent_port();
        let identity = crate::infrastructure::process::SystemProcessManager
            .identity(std::process::id())
            .unwrap()
            .expect("current test process identity");
        PidFile::with_identity(identity, port, "127.0.0.1")
            .write(&paths.pid_file())
            .unwrap();
        let sup = Supervisor::new(paths.clone(), port, "127.0.0.1");

        match sup.status().unwrap() {
            SupervisorStatus::Running {
                pid,
                port: reported,
                managed,
                ..
            } => {
                assert_eq!(pid, Some(std::process::id()));
                assert_eq!(reported, port);
                assert!(managed, "owned-but-slow process must stay managed");
            }
            other => panic!("owned process reported as {other}, expected managed running"),
        }

        assert!(
            paths.pid_file().exists(),
            "status must not destroy tracking for a live owned process"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The supervisor drops the spawned child's handle without reaping, so a
    /// child that dies instantly (bad config, bind race, early panic) lingers
    /// as a zombie whose /proc entry keeps `exists()` true. The health wait
    /// must classify that corpse as "exited" and fail fast instead of burning
    /// the whole timeout with a misleading message.
    #[test]
    fn start_health_wait_classifies_zombified_child_as_exited_not_timeout() {
        let mut child = std::process::Command::new("python3")
            .arg("-c")
            .arg("pass")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn throwaway instant-exit child");
        let pid = child.id();

        // Deterministic setup: wait until the kernel actually marks the child
        // Z (exited, unreaped) before exercising the wait loop.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !Supervisor::process_is_zombie(pid) {
            assert!(
                Instant::now() < deadline,
                "child never became a zombie within 5s"
            );
            thread::sleep(Duration::from_millis(20));
        }

        let port = grab_silent_port();
        let error = wait_for_health(
            &crate::infrastructure::process::SystemProcessManager,
            pid,
            "127.0.0.1",
            port,
            Duration::from_secs(2),
        )
        .expect_err("zombified child must end the health wait");

        assert_eq!(
            error, "bridge process exited before becoming healthy",
            "zombie must be classified as exited, not as a health timeout"
        );

        // Reap the throwaway child so no zombie leaks past this test.
        let _ = child.kill();
        let _ = child.wait();
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

        fn without_identity() -> Self {
            let manager = Self::new(crate::infrastructure::process::ProcessIdentity {
                pid: 0,
                executable: None,
                start_marker: None,
            });
            *manager.identity.lock().unwrap() = None;
            manager
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

    #[test]
    fn stop_reports_truthfully_stopped_without_pidfile_or_listener() {
        let root = temp_runtime_root("no-pid-closed-port");
        // Reserve a port, then release it so nothing is listening on it.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let sup = Supervisor::new(RuntimePaths::from_root(&root), port, "127.0.0.1");

        sup.stop()
            .expect("nothing listening: stop must succeed as stopped");
        assert!(!root.join(crate::runtime::PID_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stop_refuses_unmanaged_listener_without_pidfile() {
        let root = temp_runtime_root("unmanaged-stop-refusal");
        let port = spawn_one_health_server();
        let sup = Supervisor::new(RuntimePaths::from_root(&root), port, "127.0.0.1");

        let error = sup.stop().expect_err("untracked listener must be refused");
        match &error {
            SupervisorError::UnmanagedListener {
                port: reported,
                detail,
            } => {
                assert_eq!(*reported, port);
                assert!(detail.contains(&port.to_string()), "evidence: {detail}");
            }
            other => panic!("expected UnmanagedListener, got: {other:?}"),
        }
        assert_eq!(
            crate::supervisor::STOP_REFUSED_UNMANAGED_EXIT_CODE,
            4,
            "refusal exit code contract"
        );
        let message = error.to_string();
        assert!(
            message.contains("--unmanaged"),
            "guidance missing: {message}"
        );
        assert!(
            message.contains(&port.to_string()),
            "port missing: {message}"
        );
        // Refusal must not fabricate supervisor state.
        assert!(!root.join(crate::runtime::PID_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// RAII wrapper so the dummy listener is always signalled AND reaped,
    /// even when an assertion fails mid-test: no stray listeners may leak
    /// into sibling tests, and no zombie may outlive the test binary.
    struct ChildGuard(std::process::Child);

    impl ChildGuard {
        fn id(&self) -> u32 {
            self.0.id()
        }
    }

    impl std::ops::Deref for ChildGuard {
        type Target = std::process::Child;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl std::ops::DerefMut for ChildGuard {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Spawns a real separate process (python3) holding a listening socket and
    /// answering /health with 200, so adoption can be exercised end to end
    /// without ever pointing the supervisor at the test process itself.
    fn spawn_dummy_listener_process() -> (ChildGuard, u16) {
        const SCRIPT: &str = concat!(
            "import socket\n",
            "s = socket.socket()\n",
            "s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n",
            "s.bind(('127.0.0.1', 0))\n",
            "s.listen(8)\n",
            "print(s.getsockname()[1], flush=True)\n",
            "while True:\n",
            "    c, _ = s.accept()\n",
            "    try:\n",
            "        c.recv(1024)\n",
            "        c.sendall(b'HTTP/1.1 200 OK\\r\\nContent-Length: 2\\r\\n",
            "Connection: close\\r\\n\\r\\n{}')\n",
            "    finally:\n",
            "        c.close()\n",
        );
        let mut child = std::process::Command::new("python3")
            .arg("-c")
            .arg(SCRIPT)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn python3 dummy listener (python3 is required for this test)");
        // Read exactly one line: the child keeps stdout open for its accept
        // loop, so reading to EOF here would block forever.
        let mut stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut port_line = String::new();
        stdout
            .read_line(&mut port_line)
            .expect("read dummy listener port line");
        let port = port_line.trim().parse().expect("dummy listener port");
        (ChildGuard(child), port)
    }

    #[test]
    fn stop_adopting_unmanaged_verifies_adopts_and_stops_dummy_listener() {
        let (mut child, port) = spawn_dummy_listener_process();
        let root = temp_runtime_root("adopt-stop");
        let paths = RuntimePaths::from_root(&root);
        let sup = Supervisor::new(paths.clone(), port, "127.0.0.1");

        // The listener must be alive and fully identifiable before hand-off;
        // otherwise a premature exit could masquerade as supervisor success.
        assert!(child.try_wait().expect("poll dummy listener").is_none());
        assert!(
            crate::infrastructure::process::SystemProcessManager
                .identity(child.id())
                .unwrap()
                .is_some(),
            "dummy listener identity must be readable before adoption"
        );

        sup.stop_adopting_unmanaged()
            .expect("adoption + verified stop must succeed against owned dummy listener");

        // The adopted process must actually be gone shortly after SIGTERM.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("poll dummy listener").is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "adopted dummy listener survived supervised stop"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!paths.pid_file().exists());
        drop(child);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Simulates a process that dies from SIGTERM but stays visible to
    /// `exists()` (lingering). The destructive KILL batch must re-verify
    /// ownership, see no identity anymore, and never fire force_kill.
    #[derive(Debug)]
    struct DiesDuringWaitManager {
        identity: std::sync::Mutex<Option<crate::infrastructure::process::ProcessIdentity>>,
        terminate_calls: std::sync::Mutex<Vec<u32>>,
        force_calls: std::sync::Mutex<Vec<u32>>,
    }

    impl DiesDuringWaitManager {
        fn new(identity: crate::infrastructure::process::ProcessIdentity) -> Self {
            Self {
                identity: std::sync::Mutex::new(Some(identity)),
                terminate_calls: std::sync::Mutex::new(Vec::new()),
                force_calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ProcessManager for DiesDuringWaitManager {
        fn spawn_detached(
            &self,
            _spec: &ProcessSpec,
        ) -> std::io::Result<crate::infrastructure::process::ProcessIdentity> {
            Err(std::io::Error::other("not used"))
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
            Ok(())
        }

        fn exists(&self, _pid: u32) -> bool {
            // Stays observable even though the identity is gone: exactly the
            // stale-visibility case the pre-KILL re-verification must catch.
            true
        }
    }

    #[test]
    fn stop_reverifies_before_kill_when_process_dies_during_wait() {
        let root = temp_runtime_root("dies-during-wait");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        let identity = crate::infrastructure::process::ProcessIdentity {
            pid: 4545,
            executable: Some(std::env::current_exe().unwrap()),
            start_marker: Some("same-start".to_string()),
        };
        PidFile::with_identity(identity.clone(), 4000, "127.0.0.1")
            .write(&paths.pid_file())
            .unwrap();
        let manager = Arc::new(DiesDuringWaitManager::new(identity));
        let supervisor =
            Supervisor::new(paths.clone(), 4000, "127.0.0.1").with_process_manager(manager.clone());

        supervisor.stop().unwrap();

        assert_eq!(*manager.terminate_calls.lock().unwrap(), vec![4545]);
        assert!(
            manager.force_calls.lock().unwrap().is_empty(),
            "KILL must not fire after the process died during the wait"
        );
        assert!(!paths.pid_file().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// An exited-but-unreaped process (zombie) keeps a readable `/proc` stat
    /// entry — so identity probes stay non-None and the default `exists()`
    /// stays true — while its `/proc/<pid>/exe` link disappears. Verification
    /// must classify that combination as "already dead", never as an
    /// ownership mismatch, and must not waste a destructive signal on it.
    #[derive(Debug)]
    struct ZombieManager {
        terminate_calls: std::sync::Mutex<Vec<u32>>,
        force_calls: std::sync::Mutex<Vec<u32>>,
    }

    impl ZombieManager {
        fn new() -> Self {
            Self {
                terminate_calls: std::sync::Mutex::new(Vec::new()),
                force_calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ProcessManager for ZombieManager {
        fn spawn_detached(
            &self,
            _spec: &ProcessSpec,
        ) -> std::io::Result<crate::infrastructure::process::ProcessIdentity> {
            Err(std::io::Error::other("not used"))
        }

        fn identity(
            &self,
            pid: u32,
        ) -> std::io::Result<Option<crate::infrastructure::process::ProcessIdentity>> {
            // Same PID slot forever (start marker unchanged), but the
            // executable link is unreadable: the zombie signature.
            Ok(Some(crate::infrastructure::process::ProcessIdentity {
                pid,
                executable: None,
                start_marker: Some("same-start".to_string()),
            }))
        }

        fn terminate(&self, pid: u32) -> std::io::Result<()> {
            self.terminate_calls.lock().unwrap().push(pid);
            Ok(())
        }

        fn force_kill(&self, pid: u32) -> std::io::Result<()> {
            self.force_calls.lock().unwrap().push(pid);
            Ok(())
        }
    }

    #[test]
    fn stop_treats_same_slot_zombie_as_already_dead_not_mismatch() {
        let root = temp_runtime_root("zombie-at-verify");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        PidFile::with_identity(
            crate::infrastructure::process::ProcessIdentity {
                pid: 4646,
                executable: Some(std::env::current_exe().unwrap()),
                start_marker: Some("same-start".to_string()),
            },
            4000,
            "127.0.0.1",
        )
        .write(&paths.pid_file())
        .unwrap();
        let manager = Arc::new(ZombieManager::new());
        let supervisor =
            Supervisor::new(paths.clone(), 4000, "127.0.0.1").with_process_manager(manager.clone());

        supervisor
            .stop()
            .expect("a zombie in the same PID slot is a completed stop, not a mismatch");

        assert!(manager.terminate_calls.lock().unwrap().is_empty());
        assert!(manager.force_calls.lock().unwrap().is_empty());
        assert!(!paths.pid_file().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stop_removes_stale_pidfile_when_process_already_exited() {
        let root = temp_runtime_root("stale-pid-gone");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        PidFile::with_identity(
            crate::infrastructure::process::ProcessIdentity {
                pid: u32::MAX,
                executable: Some(PathBuf::from("/nonexistent/opencode2api-serve")),
                start_marker: Some("gone".to_string()),
            },
            4000,
            "127.0.0.1",
        )
        .write(&paths.pid_file())
        .unwrap();
        let manager = Arc::new(FakeProcessManager::without_identity());
        let supervisor =
            Supervisor::new(paths.clone(), 4000, "127.0.0.1").with_process_manager(manager.clone());

        supervisor.stop().unwrap();
        assert!(manager.terminate_calls.lock().unwrap().is_empty());
        assert!(manager.force_calls.lock().unwrap().is_empty());
        assert!(!paths.pid_file().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A failed start may only clean up ITS OWN PID file entry. When two
    /// `server start` invocations race, the loser's health probe can be
    /// answered by the winner's child while its own child dies on the bind
    /// race; unconditional cleanup in the failure path would then delete the
    /// winner's freshly written PID file and leave a live bridge untracked.
    #[test]
    fn failed_start_cleanup_leaves_foreign_pidfile_alone() {
        let root = temp_runtime_root("failed-start-foreign");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        PidFile::with_identity(
            crate::infrastructure::process::ProcessIdentity {
                pid: 4242,
                executable: Some(std::env::current_exe().unwrap()),
                start_marker: Some("winner".to_string()),
            },
            4000,
            "127.0.0.1",
        )
        .write(&paths.pid_file())
        .unwrap();
        let sup = Supervisor::new(paths.clone(), 4000, "127.0.0.1");

        sup.discard_failed_start(9999);

        assert!(
            paths.pid_file().exists(),
            "cleanup of PID 9999 must never delete another instance's PID file"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_start_cleanup_removes_own_pidfile() {
        let root = temp_runtime_root("failed-start-own");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        PidFile::with_identity(
            crate::infrastructure::process::ProcessIdentity {
                pid: 9999,
                executable: Some(PathBuf::from("/nonexistent/opencode2api-serve")),
                start_marker: Some("loser".to_string()),
            },
            4000,
            "127.0.0.1",
        )
        .write(&paths.pid_file())
        .unwrap();
        let sup = Supervisor::new(paths.clone(), 4000, "127.0.0.1");

        sup.discard_failed_start(9999);

        assert!(!paths.pid_file().exists());
        // Absent or unreadable PID files are equally fine to skip.
        sup.discard_failed_start(9999);
        let _ = std::fs::remove_dir_all(root);
    }
}
