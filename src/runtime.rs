//! Runtime directory management for supervisor artifacts.
//!
//! Stores PID files, logs, and other runtime state in
//! `~/.opencode2claude/` (XDG-style), making all supervisor
//! commands work regardless of the current working directory.

use std::path::PathBuf;

/// Default runtime directory name (under `$HOME`).
pub const RUNTIME_DIR_NAME: &str = ".opencode2claude";

/// PID file name.
pub const PID_FILE_NAME: &str = "opencode2claude.pid.json";

/// Log file name.
pub const LOG_FILE_NAME: &str = "opencode2claude.log";

/// Manages paths for runtime artifacts under `~/.opencode2claude/`.
pub struct RuntimePaths {
    root: PathBuf,
}

impl Default for RuntimePaths {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimePaths {
    /// Create a new RuntimePaths stored under `~/.opencode2claude/`.
    ///
    /// Falls back to the cwd if `HOME` is not set (rare).
    pub fn new() -> Self {
        let root = Self::default_root();
        Self { root }
    }

    /// Resolve the default runtime root: `$HOME/.opencode2claude`.
    ///
    /// Checks the `RUNTIME_DIR` environment variable first to support test suite isolation.
    pub fn default_root() -> PathBuf {
        if let Ok(dir) = std::env::var("RUNTIME_DIR") {
            PathBuf::from(dir)
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(RUNTIME_DIR_NAME)
        } else {
            PathBuf::from(".").join(RUNTIME_DIR_NAME)
        }
    }

    /// Path to the `~/.opencode2claude/` directory.
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.clone()
    }

    /// Path to the PID file: `~/.opencode2claude/opencode2claude.pid.json`.
    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join(PID_FILE_NAME)
    }

    /// Path to the bridge log file: `~/.opencode2claude/opencode2claude.log`.
    pub fn bridge_log(&self) -> PathBuf {
        self.runtime_dir().join(LOG_FILE_NAME)
    }

    /// Ensure `~/.opencode2claude/` directory and all subdirectories exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.runtime_dir())?;
        Ok(())
    }
}
