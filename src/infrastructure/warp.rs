//! Host WARP controller boundary.

use crate::infrastructure::command::{CommandRequest, CommandRunner, SystemCommandRunner};
use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpStatus {
    Connected,
    Disconnected,
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum WarpError {
    #[error("WARP command failed: {0}")]
    Command(String),
    #[error("WARP command IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait WarpController: Send + Sync + fmt::Debug {
    async fn connect(&self) -> Result<(), WarpError>;
    async fn disconnect(&self) -> Result<(), WarpError>;
    async fn status(&self) -> Result<WarpStatus, WarpError>;

    async fn reconnect(&self) -> Result<(), WarpError> {
        self.disconnect().await?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        self.connect().await?;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        Ok(())
    }
}

#[derive(Debug)]
pub struct CliWarpController {
    binary: String,
    runner: Arc<dyn CommandRunner>,
}

impl CliWarpController {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            runner: Arc::new(SystemCommandRunner),
        }
    }

    pub fn with_runner(binary: impl Into<String>, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            binary: binary.into(),
            runner,
        }
    }

    async fn run(&self, argument: &str) -> Result<String, WarpError> {
        let output = self
            .runner
            .output(
                CommandRequest::new(&self.binary, [argument]).with_timeout(Duration::from_secs(15)),
            )
            .await?;
        if output.success {
            Ok(output.stdout_text())
        } else {
            Err(WarpError::Command(format!(
                "{} {}: {}",
                self.binary,
                argument,
                output.stderr_text()
            )))
        }
    }
}

#[async_trait]
impl WarpController for CliWarpController {
    async fn connect(&self) -> Result<(), WarpError> {
        self.run("connect").await.map(|_| ())
    }

    async fn disconnect(&self) -> Result<(), WarpError> {
        self.run("disconnect").await.map(|_| ())
    }

    async fn status(&self) -> Result<WarpStatus, WarpError> {
        let output = self.run("status").await?.to_ascii_lowercase();
        if output.contains("connected") && !output.contains("disconnected") {
            Ok(WarpStatus::Connected)
        } else if output.contains("disconnected") {
            Ok(WarpStatus::Disconnected)
        } else {
            Ok(WarpStatus::Unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::command::testing::RecordingCommandRunner;
    use crate::infrastructure::command::CommandOutput;

    #[tokio::test(start_paused = true)]
    async fn reconnect_uses_disconnect_then_connect() {
        let runner = RecordingCommandRunner::with_outputs(vec![
            CommandOutput::successful(Vec::new()),
            CommandOutput::successful(Vec::new()),
        ]);
        let controller = Arc::new(CliWarpController::with_runner(
            "warp-test",
            Arc::new(runner.clone()),
        ));
        let task = tokio::spawn({
            let controller = controller.clone();
            async move { controller.reconnect().await }
        });
        tokio::time::advance(Duration::from_secs(10)).await;
        task.await.expect("task").expect("reconnect");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, vec!["disconnect"]);
        assert_eq!(calls[1].args, vec!["connect"]);
    }

    #[tokio::test]
    async fn malformed_status_is_unknown() {
        let runner = RecordingCommandRunner::with_outputs(vec![CommandOutput::successful(
            b"unexpected output".to_vec(),
        )]);
        let controller = CliWarpController::with_runner("warp-test", Arc::new(runner));
        assert_eq!(controller.status().await.unwrap(), WarpStatus::Unknown);
    }
}
