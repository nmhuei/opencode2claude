//! PID file management with process-identity metadata.

use crate::infrastructure::process::{same_executable, ProcessIdentity};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidFile {
    pub pid: u32,
    pub port: u16,
    pub host: String,
    pub started_at: u64,
    /// Canonical executable path captured after spawn. Optional only for
    /// backward-compatible reads of legacy PID files.
    #[serde(default)]
    pub executable: Option<PathBuf>,
    /// Platform process-start marker (`/proc` start ticks on Linux, `ps` start
    /// text on macOS). Prevents terminating a reused PID.
    #[serde(default)]
    pub start_marker: Option<String>,
    /// Supervisor-generated instance identifier for diagnostics and migration.
    #[serde(default)]
    pub instance_id: Option<String>,
}

impl PidFile {
    pub fn new(pid: u32, port: u16, host: impl Into<String>) -> Self {
        Self::with_identity(
            ProcessIdentity {
                pid,
                executable: None,
                start_marker: None,
            },
            port,
            host,
        )
    }

    pub fn with_identity(identity: ProcessIdentity, port: u16, host: impl Into<String>) -> Self {
        Self {
            pid: identity.pid,
            port,
            host: host.into(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            executable: identity.executable,
            start_marker: identity.start_marker,
            instance_id: Some(uuid::Uuid::new_v4().simple().to_string()),
        }
    }

    pub fn has_identity_evidence(&self) -> bool {
        self.executable.is_some() && self.start_marker.is_some()
    }

    pub fn owns(&self, actual: &ProcessIdentity) -> bool {
        if self.pid != actual.pid || !self.has_identity_evidence() {
            return false;
        }
        let executable_matches = self
            .executable
            .as_deref()
            .zip(actual.executable.as_deref())
            .is_some_and(|(expected, actual)| same_executable(Some(actual), expected));
        let marker_matches = self
            .start_marker
            .as_deref()
            .zip(actual.start_marker.as_deref())
            .is_some_and(|(expected, actual)| expected == actual);
        executable_matches && marker_matches
    }

    pub fn write(&self, path: &Path) -> Result<(), PidFileError> {
        let json = serde_json::to_vec_pretty(self)?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        use std::io::Write;
        let result = (|| {
            file.write_all(&json)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)?;
            Ok::<(), std::io::Error>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self, PidFileError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn remove(path: &Path) -> Result<(), PidFileError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PidFileError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_match_requires_executable_and_start_marker() {
        let executable = std::env::current_exe().unwrap();
        let identity = ProcessIdentity {
            pid: 42,
            executable: Some(executable.clone()),
            start_marker: Some("start-1".to_string()),
        };
        let pidfile = PidFile::with_identity(identity.clone(), 4000, "127.0.0.1");
        assert!(pidfile.owns(&identity));
        let reused = ProcessIdentity {
            start_marker: Some("start-2".to_string()),
            ..identity
        };
        assert!(!pidfile.owns(&reused));
        assert!(!PidFile::new(42, 4000, "127.0.0.1").owns(&reused));
    }

    #[test]
    fn legacy_pid_file_deserializes_without_claiming_ownership() {
        let legacy = r#"{"pid":1,"port":4000,"host":"127.0.0.1","started_at":1}"#;
        let parsed: PidFile = serde_json::from_str(legacy).unwrap();
        assert!(!parsed.has_identity_evidence());
    }
}
