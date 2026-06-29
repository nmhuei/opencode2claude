//! PID file management for the bridge supervisor.
//!
//! Stores bridge process metadata in `.runtime/opencode2claude.pid.json`.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema of the JSON PID file.
#[derive(Debug, Serialize, Deserialize)]
pub struct PidFile {
    /// Process ID of the bridge server.
    pub pid: u32,
    /// Port the bridge is listening on.
    pub port: u16,
    /// Host address the bridge is bound to.
    pub host: String,
    /// Unix epoch milliseconds when the bridge started.
    pub started_at: u64,
}

impl PidFile {
    /// Create a new PidFile with current timestamp.
    pub fn new(pid: u32, port: u16, host: impl Into<String>) -> Self {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            pid,
            port,
            host: host.into(),
            started_at,
        }
    }

    /// Write PID file to the given path.
    pub fn write(&self, path: &Path) -> Result<(), PidFileError> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Read and parse a PID file from the given path.
    pub fn read(path: &Path) -> Result<Self, PidFileError> {
        let data = std::fs::read_to_string(path)?;
        let pidfile: Self = serde_json::from_str(&data)?;
        Ok(pidfile)
    }

    /// Remove the PID file if it exists.
    pub fn remove(path: &Path) -> Result<(), PidFileError> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

/// Errors from PID file operations.
#[derive(Debug, thiserror::Error)]
pub enum PidFileError {
    /// Wrapped IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Wrapped JSON error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
