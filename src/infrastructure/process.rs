//! Linux process lifecycle boundary.

use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

pub trait ProcessManager: Send + Sync + fmt::Debug {
    fn spawn_detached(&self, spec: &ProcessSpec) -> io::Result<ProcessIdentity>;
    fn identity(&self, pid: u32) -> io::Result<Option<ProcessIdentity>>;
    fn terminate(&self, pid: u32) -> io::Result<()>;
    fn force_kill(&self, pid: u32) -> io::Result<()>;

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
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid PID"));
    }
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
}
