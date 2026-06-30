//! Runtime directory management for supervisor artifacts.
//!
//! Manages the `.runtime/` directory that stores PID files, logs, and
//! other runtime state for the bridge supervisor.

use std::path::PathBuf;

/// Default runtime directory name.
pub const RUNTIME_DIR_NAME: &str = ".runtime";

/// PID file name.
pub const PID_FILE_NAME: &str = "opencode2claude.pid.json";

/// Log file name.
pub const LOG_FILE_NAME: &str = "opencode2claude.log";

/// Manages paths for runtime artifacts.
pub struct RuntimePaths {
    root: PathBuf,
}

impl RuntimePaths {
    /// Create a new RuntimePaths rooted at the project/repo root.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Path to the `.runtime/` directory.
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join(RUNTIME_DIR_NAME)
    }

    /// Path to the PID file: `.runtime/opencode2claude.pid.json`.
    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join(PID_FILE_NAME)
    }

    /// Path to the bridge log file: `.runtime/opencode2claude.log`.
    pub fn bridge_log(&self) -> PathBuf {
        self.runtime_dir().join(LOG_FILE_NAME)
    }

    /// Ensure `.runtime/` directory and all subdirectories exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.runtime_dir())?;
        Ok(())
    }
}
