//! Async command execution boundary with bounded runtime.

use async_trait::async_trait;
use std::fmt;
use std::io;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl CommandRequest {
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    pub fn successful(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            success: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    pub fn failed(code: i32, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            success: false,
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

#[async_trait]
pub trait CommandRunner: Send + Sync + fmt::Debug {
    async fn output(&self, request: CommandRequest) -> io::Result<CommandOutput>;
}

#[derive(Debug, Default)]
pub struct SystemCommandRunner;

#[async_trait]
impl CommandRunner for SystemCommandRunner {
    async fn output(&self, request: CommandRequest) -> io::Result<CommandOutput> {
        let mut command = tokio::process::Command::new(&request.program);
        command.args(&request.args).kill_on_drop(true);
        let output = tokio::time::timeout(request.timeout, command.output())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "command '{}' exceeded {:?}",
                        request.program, request.timeout
                    ),
                )
            })??;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Default)]
    pub struct RecordingCommandRunner {
        calls: Arc<Mutex<Vec<CommandRequest>>>,
        outputs: Arc<Mutex<VecDeque<io::Result<CommandOutput>>>>,
    }

    impl RecordingCommandRunner {
        pub fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
            Self {
                calls: Arc::default(),
                outputs: Arc::new(Mutex::new(outputs.into_iter().map(Ok).collect())),
            }
        }

        pub fn with_results(outputs: Vec<io::Result<CommandOutput>>) -> Self {
            Self {
                calls: Arc::default(),
                outputs: Arc::new(Mutex::new(outputs.into())),
            }
        }

        pub fn calls(&self) -> Vec<CommandRequest> {
            self.calls.lock().expect("calls lock").clone()
        }

        pub fn push(&self, output: CommandOutput) {
            self.outputs
                .lock()
                .expect("outputs lock")
                .push_back(Ok(output));
        }
    }

    #[async_trait]
    impl CommandRunner for RecordingCommandRunner {
        async fn output(&self, request: CommandRequest) -> io::Result<CommandOutput> {
            self.calls.lock().expect("calls lock").push(request);
            self.outputs
                .lock()
                .expect("outputs lock")
                .pop_front()
                .unwrap_or_else(|| Ok(CommandOutput::successful(Vec::new())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_runner_reports_non_zero_without_conflating_io_failure() {
        #[cfg(unix)]
        let request = CommandRequest::new("sh", ["-c", "printf err >&2; exit 7"]);
        #[cfg(windows)]
        let request = CommandRequest::new("cmd", ["/C", "echo err 1>&2 & exit /B 7"]);
        let output = SystemCommandRunner.output(request).await.expect("output");
        assert!(!output.success);
        assert_eq!(output.code, Some(7));
        assert!(output.stderr_text().contains("err"));
    }

    #[tokio::test]
    async fn system_runner_enforces_timeout() {
        #[cfg(unix)]
        let request =
            CommandRequest::new("sh", ["-c", "sleep 5"]).with_timeout(Duration::from_millis(10));
        #[cfg(windows)]
        let request = CommandRequest::new("cmd", ["/C", "ping 127.0.0.1 -n 6 >NUL"])
            .with_timeout(Duration::from_millis(10));
        let error = SystemCommandRunner
            .output(request)
            .await
            .expect_err("timeout expected");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
