//! Linux process lifecycle boundary.
//!
//! Signal delivery is anchored to a pidfd where the kernel supports it
//! (`pidfd_open`/`pidfd_send_signal`, Linux ≥ 5.3): identity verification and
//! `kill(2)` then target the SAME pinned process handle, so a PID recycled
//! between verification and delivery can never receive the signal — the
//! kernel resolves the handle to the original process or reports `ESRCH`.
//! On older kernels (and non-Linux targets) delivery falls back to the
//! historical verify-then-signal-by-number path.

use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// Run an interactive foreground command behind the infrastructure boundary.
/// The child inherits stdin/stdout/stderr unless the caller changes them via
/// this adapter, preserving normal terminal behavior for Claude Code.
pub fn run_foreground<I, K, V>(
    executable: &str,
    args: I,
    environment: impl IntoIterator<Item = (K, Option<V>)>,
) -> io::Result<ExitStatus>
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new(executable);
    command.args(args);
    for (key, value) in environment {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    command.status()
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub executable: Option<PathBuf>,
    pub start_marker: Option<String>,
}

/// Result of an identity-verified signal delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalOutcome {
    /// The signal was delivered to the verified process.
    Delivered,
    /// The verified process exited before delivery and can no longer be
    /// signalled (`ESRCH` on the pinned handle, or a same-slot corpse).
    /// This is a completed stop, not a failure.
    AlreadyGone,
}

pub trait ProcessManager: Send + Sync + fmt::Debug {
    fn spawn_detached(&self, spec: &ProcessSpec) -> io::Result<ProcessIdentity>;
    fn identity(&self, pid: u32) -> io::Result<Option<ProcessIdentity>>;
    fn terminate(&self, pid: u32) -> io::Result<()>;
    fn force_kill(&self, pid: u32) -> io::Result<()>;

    /// Deliver TERM/KILL only after re-verifying that the PID slot still
    /// holds `expected`, against the SAME process handle used for delivery.
    ///
    /// The default implementation opens a pidfd for `expected.pid` (Linux),
    /// classifies the slot occupant, and signals through the pinned handle;
    /// on kernels without pidfd support it falls back to the historical
    /// read-then-signal sequence. A vanished target is reported as
    /// [`SignalOutcome::AlreadyGone`], never as an error.
    fn terminate_verified(
        &self,
        expected: &ProcessIdentity,
        force: bool,
    ) -> io::Result<SignalOutcome> {
        deliver_verified(expected, force)
    }

    fn exists(&self, pid: u32) -> bool {
        self.identity(pid).ok().flatten().is_some()
    }
}

#[derive(Debug, Default)]
pub struct SystemProcessManager;

impl ProcessManager for SystemProcessManager {
    fn spawn_detached(&self, spec: &ProcessSpec) -> io::Result<ProcessIdentity> {
        if let Some(parent) = spec.stdout_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.stdout_path)?;
        let stderr = if spec.stderr_path == spec.stdout_path {
            stdout.try_clone()?
        } else {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&spec.stderr_path)?
        };

        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        use std::os::unix::process::CommandExt;
        // A distinct process group is sufficient for supervisor ownership;
        // no unsafe pre_exec hook is required.
        command.process_group(0);

        let child = command.spawn()?;
        let pid = child.id();
        // Dropping `child` without wait() leaves an unreaped zombie until this
        // (short-lived controller) process exits and the child reparents to a
        // reaper. Callers rely on the corpse remaining visible in /proc so the
        // health wait can classify it as exited; see supervisor.rs.
        Ok(self.identity(pid)?.unwrap_or(ProcessIdentity {
            pid,
            executable: Some(spec.executable.clone()),
            start_marker: None,
        }))
    }

    fn identity(&self, pid: u32) -> io::Result<Option<ProcessIdentity>> {
        if !valid_pid(pid) {
            return Ok(None);
        }
        system_identity(pid)
    }

    fn terminate(&self, pid: u32) -> io::Result<()> {
        signal_process(pid, false)
    }

    fn force_kill(&self, pid: u32) -> io::Result<()> {
        signal_process(pid, true)
    }
}

fn valid_pid(pid: u32) -> bool {
    pid > 0 && pid <= i32::MAX as u32
}

fn system_identity(pid: u32) -> io::Result<Option<ProcessIdentity>> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    if !root.exists() {
        return Ok(None);
    }
    let executable = std::fs::read_link(root.join("exe")).ok();
    let start_marker = std::fs::read_to_string(root.join("stat"))
        .ok()
        .and_then(|stat| parse_linux_start_ticks(&stat));
    Ok(Some(ProcessIdentity {
        pid,
        executable,
        start_marker,
    }))
}

fn signal_process(pid: u32, force: bool) -> io::Result<()> {
    if !valid_pid(pid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal invalid pid {pid}"),
        ));
    }
    #[cfg(target_os = "linux")]
    {
        signal_process_linux(pid, force)
    }
    #[cfg(not(target_os = "linux"))]
    {
        signal_process_external(pid, force)
    }
}

/// Verified delivery: pin first, classify second, signal third.
///
/// Every step after the pin either touches the pinned handle or refuses to
/// act, so no interleaving of process exit + PID reuse can redirect the
/// signal to a new slot occupant.
fn deliver_verified(expected: &ProcessIdentity, force: bool) -> io::Result<SignalOutcome> {
    if !valid_pid(expected.pid) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid PID"));
    }
    if expected.executable.is_none() || expected.start_marker.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to terminate pid {}: expectation lacks verifiable identity evidence",
                expected.pid
            ),
        ));
    }

    #[cfg(target_os = "linux")]
    if let Some(outcome) = pidfd_verified(expected, force)? {
        return Ok(outcome);
    }

    // A kernel without pidfd support falls through to the legacy sequencing.
    legacy_verified(expected, force)
}

/// Historical sequencing used when no pidfd is available: read /proc, then
/// signal by number. The verification-to-delivery window is inherently racy
/// here; callers get best-effort protection only.
fn legacy_verified(expected: &ProcessIdentity, force: bool) -> io::Result<SignalOutcome> {
    match system_identity(expected.pid)? {
        Some(actual) => match classify_slot(expected, &actual) {
            SlotClassification::OccupantMatch => {}
            SlotClassification::SameSlotCorpse => return Ok(SignalOutcome::AlreadyGone),
            SlotClassification::Foreign => return Err(ownership_mismatch(expected.pid)),
        },
        None => return Ok(SignalOutcome::AlreadyGone),
    }
    // Platform dispatch: direct kill(2) on Linux (including pre-pidfd
    // kernels), external binary elsewhere.
    signal_process(expected.pid, force)?;
    Ok(SignalOutcome::Delivered)
}

/// pidfd-anchored delivery. Returns `Ok(None)` when the kernel does not
/// support pidfd (caller falls back), otherwise the delivery outcome.
#[cfg(target_os = "linux")]
fn pidfd_verified(expected: &ProcessIdentity, force: bool) -> io::Result<Option<SignalOutcome>> {
    let pinned = match PidFd::open(expected.pid) {
        Ok(pinned) => pinned,
        Err(error) if PidFd::is_unsupported(&error) => return Ok(None),
        Err(error) => return Err(error),
    };

    // Probe through the pinned handle first: if the original process already
    // exited AND was reaped, the answer is authoritative — no signal can ever
    // reach it again, regardless of who occupies the PID number now.
    match pinned.send_signal(0) {
        Err(ref error) if is_esrch(error) => return Ok(Some(SignalOutcome::AlreadyGone)),
        other => other?,
    }

    // The pinned process is alive at probe time, so the slot still belongs to
    // it. Re-read /proc to validate the caller's expectation; a mismatch means
    // the caller handed us stale evidence and we refuse rather than signal.
    if let Some(actual) = system_identity(expected.pid)? {
        match classify_slot(expected, &actual) {
            SlotClassification::OccupantMatch => {}
            SlotClassification::SameSlotCorpse => return Ok(Some(SignalOutcome::AlreadyGone)),
            SlotClassification::Foreign => return Err(ownership_mismatch(expected.pid)),
        }
    }
    // If /proc vanished microseconds after a successful live probe, the
    // process died and was reaped in between. Delivery below still targets
    // the pinned corpse and can only report ESRCH.

    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    match pinned.send_signal(signal) {
        Ok(()) => Ok(Some(SignalOutcome::Delivered)),
        // Died between probe and delivery: completed stop, not an error.
        Err(ref error) if is_esrch(error) => Ok(Some(SignalOutcome::AlreadyGone)),
        Err(error) => Err(error),
    }
}

enum SlotClassification {
    OccupantMatch,
    SameSlotCorpse,
    Foreign,
}

fn classify_slot(expected: &ProcessIdentity, actual: &ProcessIdentity) -> SlotClassification {
    if identity_matches(expected, actual) {
        return SlotClassification::OccupantMatch;
    }
    // Exited-but-unreaped zombies keep a readable /proc stat entry (so the
    // start marker survives) while their exe link disappears. That exact
    // signature is a completed stop, not an ownership violation.
    if actual.executable.is_none()
        && expected.executable.is_some()
        && actual.start_marker.is_some()
        && actual.start_marker == expected.start_marker
    {
        return SlotClassification::SameSlotCorpse;
    }
    SlotClassification::Foreign
}

fn identity_matches(expected: &ProcessIdentity, actual: &ProcessIdentity) -> bool {
    if expected.pid != actual.pid {
        return false;
    }
    let executable_matches = expected
        .executable
        .as_deref()
        .zip(actual.executable.as_deref())
        .is_some_and(|(expected_path, actual_path)| {
            same_executable(Some(actual_path), expected_path)
        });
    let marker_matches = expected
        .start_marker
        .as_deref()
        .zip(actual.start_marker.as_deref())
        .is_some_and(|(expected_marker, actual_marker)| expected_marker == actual_marker);
    executable_matches && marker_matches
}

fn ownership_mismatch(pid: u32) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "refusing to signal pid {pid}: PID slot now holds a different process than the verified identity"
        ),
    )
}

/// True when the error is `ESRCH` — the pinned/numeric target no longer
/// exists. Compared on the raw errno because std leaves ESRCH unmapped
/// (`ErrorKind::Uncategorized`), unlike ENOENT.
#[cfg(target_os = "linux")]
fn is_esrch(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
fn signal_process_linux(pid: u32, force: bool) -> io::Result<()> {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // Prefer the pinned-handle path even for unverified calls: opening the
    // fd and signalling through it removes both the PATH lookup and the
    // fork/exec delay of the historical external `kill` binary.
    match PidFd::open(pid) {
        Ok(pinned) => {
            return match pinned.send_signal(signal) {
                Ok(()) => Ok(()),
                // Target already gone: a completed stop, not a failure.
                Err(ref error) if is_esrch(error) => Ok(()),
                Err(error) => Err(error),
            };
        }
        Err(ref error) if PidFd::is_unsupported(error) => {} // fall back to kill(2)
        Err(error) => return Err(error),
    }
    // Pre-pidfd kernels: direct syscall, still no external binary.
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if is_esrch(&error) {
        return Ok(());
    }
    Err(error)
}

/// Non-Linux fallback: historical behaviour (external `kill` binary).
#[cfg(not(target_os = "linux"))]
fn signal_process_external(pid: u32, force: bool) -> io::Result<()> {
    let signal = if force { "-KILL" } else { "-TERM" };
    let status = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("kill {signal} {pid} failed")))
    }
}

/// A file descriptor pinning one exact process lifetime.
///
/// Once opened, the kernel guarantees the handle refers to the same process
/// until that process is fully terminated and reaped — PID reuse elsewhere
/// cannot retarget it. Signalling through the handle therefore closes the
/// verify-then-kill race that numeric PIDs cannot close.
#[cfg(target_os = "linux")]
struct PidFd {
    fd: std::os::unix::io::OwnedFd,
}

#[cfg(target_os = "linux")]
impl PidFd {
    /// Pin whatever currently occupies `pid`. Fails with `ENOSYS` on kernels
    /// older than 5.3 ([`Self::is_unsupported`]).
    fn open(pid: u32) -> io::Result<Self> {
        // SAFETY: raw syscall with two scalar arguments; on success the
        // kernel returns a fresh, exclusively-owned file descriptor which we
        // immediately wrap in OwnedFd for close-on-drop discipline.
        let rc = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        use std::os::unix::io::{FromRawFd, OwnedFd};
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(rc as std::os::unix::io::RawFd) },
        })
    }

    /// True when the error means pidfd is unavailable on this kernel
    /// (`ENOSYS`); callers should fall back to numeric-PID delivery.
    fn is_unsupported(error: &io::Error) -> bool {
        error.raw_os_error() == Some(libc::ENOSYS)
    }

    /// Send `sig` (0 performs a liveness/permission probe without signalling)
    /// to the pinned process. `ESRCH` surfaces as `ErrorKind::NotFound`.
    fn send_signal(&self, sig: i32) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;
        // SAFETY: fd is a valid open pidfd for the lifetime of self; the
        // siginfo pointer is NULL as required when flags == 0.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                sig,
                std::ptr::null::<core::ffi::c_void>(),
                0u32,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn parse_linux_start_ticks(stat: &str) -> Option<String> {
    // `/proc/<pid>/stat` field 2 can contain spaces inside parentheses. Split
    // only after the closing command-name parenthesis; starttime is field 22,
    // therefore index 19 in the remainder beginning with field 3.
    let remainder = stat.rsplit_once(") ")?.1;
    remainder.split_whitespace().nth(19).map(ToOwned::to_owned)
}

pub fn same_executable(actual: Option<&Path>, expected: &Path) -> bool {
    let Some(actual) = actual else {
        return false;
    };

    // Linux appends ` (deleted)` to /proc/<pid>/exe after an in-place rebuild
    // replaces the binary while the old process is still running. The process
    // start marker is checked separately, so stripping only this kernel suffix
    // preserves ownership without accepting a reused PID.
    let actual_text = actual.to_string_lossy();
    let normalized_actual = actual_text
        .strip_suffix(" (deleted)")
        .map(PathBuf::from)
        .unwrap_or_else(|| actual.to_path_buf());

    if normalized_actual == expected {
        return true;
    }

    normalized_actual
        .canonicalize()
        .ok()
        .zip(expected.canonicalize().ok())
        .is_some_and(|(left, right)| left == right)
}

#[cfg(all(test, target_os = "linux"))]
mod pidfd_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// RAII reap guard: every child spawned here is killed AND waited even
    /// when an assertion fails mid-test, so no zombie or stray sleeper leaks
    /// past the test binary.
    struct ChildGuard(std::process::Child);

    impl ChildGuard {
        fn spawn_sh(script: &str) -> Self {
            let child = std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sh test child");
            Self(child)
        }

        fn id(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn full_identity(pid: u32) -> ProcessIdentity {
        SystemProcessManager
            .identity(pid)
            .unwrap()
            .expect("live child identity with evidence")
    }

    /// Block until the freshly spawned child has finished exec'ing into the
    /// shell proper. Between fork() and exec(), /proc/<pid>/exe still names
    /// THIS test binary — an identity captured there records the wrong
    /// executable and verification would (correctly) refuse it moments
    /// later when exec swaps the image.
    fn wait_until_shell_execed(pid: u32) {
        let prefix = format!("{pid} (sh) ");
        wait_until(
            || {
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .map(|stat| stat.starts_with(&prefix))
                    .unwrap_or(false)
            },
            "child to finish exec'ing into sh",
        );
    }

    /// Block until the child has actually installed `trap "" TERM`, observed
    /// via the kernel's SigIgn bitmask in /proc/<pid>/status. Sending TERM
    /// before the trap exists kills the shell with its DEFAULT disposition,
    /// which would make the TERM-immunity scenario meaningless.
    fn wait_until_term_ignored(pid: u32) {
        fn sigign_mask(pid: u32) -> Option<u64> {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            let line = status.lines().find(|line| line.starts_with("SigIgn:"))?;
            let mask_field = line.split_whitespace().nth(1)?;
            u64::from_str_radix(mask_field, 16).ok()
        }
        let term_bit = 1u64 << (libc::SIGTERM - 1);
        wait_until(
            || sigign_mask(pid).is_some_and(|mask| mask & term_bit != 0),
            "trap to ignore SIGTERM",
        );
    }

    fn wait_until(predicate: impl Fn() -> bool, label: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !predicate() {
            assert!(
                std::time::Instant::now() < deadline,
                "condition never held within 5s: {label}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn pidfd_pins_current_process_for_probe() {
        let pinned = PidFd::open(std::process::id()).expect("pidfd_open on self");
        pinned
            .send_signal(0)
            .expect("signal-0 probe must succeed on own live process");
    }

    #[test]
    fn pidfd_send_after_reap_is_esrch_never_retarged() {
        let mut child = ChildGuard::spawn_sh("sleep 30");
        let pid = child.id();
        let pinned = PidFd::open(pid).expect("pin while child alive");

        // Exit AND reap the child while our handle stays open.
        child.0.kill().expect("kill sleeper");
        child.0.wait().expect("reap sleeper");

        let error = pinned
            .send_signal(libc::SIGKILL)
            .expect_err("signalling a fully-exited process must fail");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ESRCH),
            "reaped target must surface as ESRCH"
        );
    }

    #[test]
    fn terminate_verified_delivers_term_to_matching_child() {
        let mut child = ChildGuard::spawn_sh("sleep 30");
        wait_until_shell_execed(child.id());
        let expected = full_identity(child.id());

        let outcome = SystemProcessManager
            .terminate_verified(&expected, false)
            .expect("verified TERM");
        assert_eq!(outcome, SignalOutcome::Delivered);

        let status = child.0.wait().expect("reap after TERM");
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn terminate_verified_force_kills_term_immune_child() {
        let mut child = ChildGuard::spawn_sh("trap \"\" TERM; sleep 30");
        wait_until_shell_execed(child.id());
        let expected = full_identity(child.id());
        // The trap needs a moment to install; delivering earlier would kill
        // the shell by default disposition and void the scenario.
        wait_until_term_ignored(child.id());

        let outcome = SystemProcessManager
            .terminate_verified(&expected, false)
            .expect("verified TERM against immune child");
        assert_eq!(outcome, SignalOutcome::Delivered);

        // Give the ignored SIGTERM ample chance to (wrongly) take effect.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            SystemProcessManager.exists(child.id()),
            "TERM-immune child must survive the soft batch"
        );

        let outcome = SystemProcessManager
            .terminate_verified(&expected, true)
            .expect("verified KILL");
        assert_eq!(outcome, SignalOutcome::Delivered);

        let status = child.0.wait().expect("reap after KILL");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn terminate_verified_refuses_foreign_slot_evidence_without_signalling() {
        // Own live process, but fabricated start marker: the primitive must
        // refuse (and conspicuously NOT signal us) instead of delivering.
        let mut forged = full_identity(std::process::id());
        forged.start_marker = Some("0".repeat(forged.start_marker.as_ref().unwrap().len()));

        let error = SystemProcessManager
            .terminate_verified(&forged, true)
            .expect_err("mismatched slot evidence must be refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains(&forged.pid.to_string()));
    }

    #[test]
    fn terminate_verified_requires_identity_evidence() {
        let bare = ProcessIdentity {
            pid: std::process::id(),
            executable: None,
            start_marker: None,
        };
        let error = SystemProcessManager
            .terminate_verified(&bare, false)
            .expect_err("unverifiable expectation must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn terminate_verified_reports_same_slot_corpse_as_already_gone() {
        // Keep the shell alive briefly after exec so identity capture cannot
        // race its immediate exit and observe an already-zombified process.
        let mut child = ChildGuard::spawn_sh("sleep 0.2; exit 0");
        let pid = child.id();

        // Capture only after exec finished: during the fork window the exe
        // link still names this test binary, and an identity recorded there
        // would be refused once the shell image replaces it.
        wait_until_shell_execed(pid);
        let expected = full_identity(pid);

        // Deterministic setup: wait until the child exited and the unreaped
        // corpse shows the zombie signature (exe link gone, ticks unchanged).
        wait_until(
            || {
                matches!(
                    SystemProcessManager.identity(pid).unwrap(),
                    Some(ref actual) if actual.executable.is_none(),
                )
            },
            "child to become an unreaped zombie",
        );

        let outcome = SystemProcessManager
            .terminate_verified(&expected, false)
            .expect("corpse classification must not error");
        assert_eq!(
            outcome,
            SignalOutcome::AlreadyGone,
            "same-slot zombie is a completed stop, not a mismatch"
        );

        child.0.wait().expect("final reap");
    }

    #[test]
    fn invalid_pid_is_rejected_by_verified_delivery() {
        let bare = ProcessIdentity {
            pid: 0,
            executable: Some(PathBuf::from("/bin/sh")),
            start_marker: Some("1".to_string()),
        };
        let error = SystemProcessManager
            .terminate_verified(&bare, false)
            .expect_err("pid 0 must never be signalled");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_has_identity() {
        let identity = SystemProcessManager
            .identity(std::process::id())
            .unwrap()
            .expect("identity");
        assert_eq!(identity.pid, std::process::id());
    }

    #[test]
    fn impossible_pid_has_no_identity() {
        assert!(SystemProcessManager.identity(u32::MAX).unwrap().is_none());
    }

    #[test]
    fn parses_start_ticks_after_parenthesized_command_name() {
        let stat =
            "123 (name with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242 21";
        assert_eq!(parse_linux_start_ticks(stat).as_deref(), Some("424242"));
    }

    #[test]
    fn deleted_proc_executable_suffix_still_matches_original_path() {
        let expected = PathBuf::from("/tmp/opencode2api-serve");
        let actual = PathBuf::from("/tmp/opencode2api-serve (deleted)");
        assert!(same_executable(Some(&actual), &expected));
    }

    #[test]
    fn deleted_suffix_does_not_match_a_different_executable() {
        let expected = PathBuf::from("/tmp/opencode2api-serve");
        let actual = PathBuf::from("/tmp/other-service (deleted)");
        assert!(!same_executable(Some(&actual), &expected));
    }

    #[test]
    fn identity_matching_mirrors_pidfile_owns_semantics() {
        let expected = ProcessIdentity {
            pid: 4242,
            executable: Some(PathBuf::from("/usr/bin/serve")),
            start_marker: Some("777".to_string()),
        };
        let matching = ProcessIdentity {
            pid: 4242,
            executable: Some(PathBuf::from("/usr/bin/serve")),
            start_marker: Some("777".to_string()),
        };
        assert!(matches!(
            classify_slot(&expected, &matching),
            SlotClassification::OccupantMatch
        ));

        // Same slot, exe link gone, ticks unchanged: corpse, not foreigner.
        let corpse = ProcessIdentity {
            pid: 4242,
            executable: None,
            start_marker: Some("777".to_string()),
        };
        assert!(matches!(
            classify_slot(&expected, &corpse),
            SlotClassification::SameSlotCorpse
        ));

        // Same slot, different ticks: recycled — foreign.
        let recycled = ProcessIdentity {
            pid: 4242,
            executable: Some(PathBuf::from("/usr/bin/serve")),
            start_marker: Some("999".to_string()),
        };
        assert!(matches!(
            classify_slot(&expected, &recycled),
            SlotClassification::Foreign
        ));

        // Missing actual evidence is foreign (mirrors PidFile::owns zip logic).
        let blind = ProcessIdentity {
            pid: 4242,
            executable: None,
            start_marker: None,
        };
        assert!(matches!(
            classify_slot(&expected, &blind),
            SlotClassification::Foreign
        ));
    }
}
