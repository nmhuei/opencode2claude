//! Runtime directory management for supervisor artifacts.
//!
//! Stores PID files, logs, and other runtime state in
//! `~/.opencode2api/` (XDG-style), making all supervisor
//! commands work regardless of the current working directory.

use std::path::PathBuf;

/// Default runtime directory name (under `$HOME`).
pub const RUNTIME_DIR_NAME: &str = ".opencode2api";

/// PID file name.
pub const PID_FILE_NAME: &str = "opencode2api.pid.json";

/// Log file name.
pub const LOG_FILE_NAME: &str = "opencode2api.log";
/// Request-history directory name.
pub const HISTORY_DIR_NAME: &str = "history";
/// Request-history SQLite database name.
pub const HISTORY_DATABASE_NAME: &str = "request-history.sqlite3";

/// Manages paths for runtime artifacts under `~/.opencode2api/`.
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    root: PathBuf,
}

impl Default for RuntimePaths {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimePaths {
    /// Build runtime paths from the resolved application configuration.
    pub fn from_config(config: &crate::config::BridgeConfig) -> Self {
        config
            .runtime
            .runtime_dir
            .clone()
            .map(Self::from_root)
            .unwrap_or_default()
    }

    /// Create a new RuntimePaths stored under `~/.opencode2api/`.
    ///
    /// Falls back to the cwd if `HOME` is not set (rare).
    pub fn new() -> Self {
        let root = Self::default_root();
        Self { root }
    }

    /// Create paths from an explicit runtime root.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve the default runtime root: `$HOME/.opencode2api`.
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

    /// Path to the `~/.opencode2api/` directory.
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.clone()
    }

    /// Path to the PID file: `~/.opencode2api/opencode2api.pid.json`.
    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join(PID_FILE_NAME)
    }

    /// Path to the bridge log file: `~/.opencode2api/opencode2api.log`.
    pub fn bridge_log(&self) -> PathBuf {
        self.runtime_dir().join(LOG_FILE_NAME)
    }

    /// Path to the request-history directory.
    pub fn history_dir(&self) -> PathBuf {
        self.runtime_dir().join(HISTORY_DIR_NAME)
    }

    /// Path to the request-history SQLite database.
    pub fn history_database(&self) -> PathBuf {
        self.history_dir().join(HISTORY_DATABASE_NAME)
    }

    /// Ensure `~/.opencode2api/` directory and all subdirectories exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.runtime_dir())?;
        Ok(())
    }
}
