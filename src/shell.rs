//! Shell command execution with configurable safety policy.
//!
//! The bridge can intercept prompts starting with `!` and execute them
//! directly on the local machine, bypassing the LLM entirely.
//! This module handles both the security policy and execution.

use crate::config::MSG_ID_SHELL;
use crate::sse::SseEventBuilder;
use futures_util::Stream;
use std::collections::HashSet;
use std::convert::Infallible;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::info;

use axum::response::sse::Event;

/// Defines how the bridge handles `!` shell commands.
#[derive(Debug, Clone)]
pub enum ShellPolicy {
    /// Shell commands are completely disabled (safest).
    Disabled,
    /// Only commands whose base name is in the set are allowed.
    AllowList(HashSet<String>),
    /// All shell commands are allowed (must opt-in explicitly).
    Unrestricted,
}

impl ShellPolicy {
    /// Check if a given shell command string is allowed under this policy.
    /// Returns Ok(()) if allowed, Err(reason) if blocked.
    pub fn check(&self, cmd_str: &str) -> Result<(), String> {
        match self {
            ShellPolicy::Disabled => Err("Shell commands are disabled by policy".to_string()),
            ShellPolicy::AllowList(allowed) => {
                let base_cmd = extract_base_command(cmd_str);
                if allowed.contains(&base_cmd) {
                    Ok(())
                } else {
                    Err(format!(
                        "Command '{}' is not in the allowlist. Allowed: {}",
                        base_cmd,
                        allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                    ))
                }
            }
            ShellPolicy::Unrestricted => Ok(()),
        }
    }

    /// Human-readable description of the current policy.
    pub fn description(&self) -> String {
        match self {
            ShellPolicy::Disabled => "disabled".to_string(),
            ShellPolicy::AllowList(set) => {
                format!(
                    "allowlist ({})",
                    set.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            ShellPolicy::Unrestricted => "unrestricted".to_string(),
        }
    }
}

/// Extract the base command name from a shell command string.
/// e.g., "git status" → "git", "ls -la" → "ls"
fn extract_base_command(cmd_str: &str) -> String {
    cmd_str.split_whitespace().next().unwrap_or("").to_string()
}

/// Execute a shell command synchronously and return the combined output.
pub async fn run_shell_sync(cmd_str: &str) -> String {
    info!("Executing shell command (sync): '{}'", cmd_str);
    match Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            format!("{}{}", stdout, stderr)
        }
        Err(e) => format!("Local Shell Error: {}", e),
    }
}

/// Execute a shell command and stream output as SSE events.
pub fn run_shell_stream(
    cmd_str: String,
    model: String,
    buffer_size: usize,
    channel_capacity: usize,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::channel(channel_capacity);
    let builder = SseEventBuilder::new(MSG_ID_SHELL.to_string(), model);

    tokio::spawn(async move {
        // Send opening SSE events
        let _ = tx.send(builder.message_start()).await;
        let _ = tx.send(builder.content_block_start()).await;

        match Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) {
                    let mut out_reader = tokio::io::BufReader::new(stdout);
                    let mut err_reader = tokio::io::BufReader::new(stderr);
                    let mut out_buffer = vec![0u8; buffer_size];
                    let mut err_buffer = vec![0u8; buffer_size];
                    let mut stdout_done = false;
                    let mut stderr_done = false;

                    loop {
                        if stdout_done && stderr_done {
                            break;
                        }
                        tokio::select! {
                            res = out_reader.read(&mut out_buffer), if !stdout_done => {
                                match res {
                                    Ok(0) => stdout_done = true,
                                    Ok(n) => {
                                        let text = String::from_utf8_lossy(&out_buffer[..n]).to_string();
                                        let _ = tx.send(builder.text_delta(&text)).await;
                                    }
                                    Err(_) => stdout_done = true,
                                }
                            }
                            res = err_reader.read(&mut err_buffer), if !stderr_done => {
                                match res {
                                    Ok(0) => stderr_done = true,
                                    Ok(n) => {
                                        let text = String::from_utf8_lossy(&err_buffer[..n]).to_string();
                                        let _ = tx.send(builder.text_delta(&text)).await;
                                    }
                                    Err(_) => stderr_done = true,
                                }
                            }
                        }
                    }
                    let _ = child.wait().await;
                }
            }
            Err(e) => {
                let _ = tx
                    .send(builder.text_delta(&format!("\n[Local Shell Error]: {}", e)))
                    .await;
            }
        }

        // Send closing SSE events
        let _ = tx.send(builder.content_block_stop()).await;
        let _ = tx.send(builder.message_delta()).await;
        let _ = tx.send(builder.message_stop()).await;
    });

    tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok)
}

use futures_util::StreamExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_base_command() {
        assert_eq!(extract_base_command("git status"), "git");
        assert_eq!(extract_base_command("ls -la /tmp"), "ls");
        assert_eq!(extract_base_command("  pwd  "), "pwd");
        assert_eq!(extract_base_command(""), "");
    }

    #[test]
    fn test_shell_policy_disabled() {
        let policy = ShellPolicy::Disabled;
        assert!(policy.check("ls").is_err());
        assert!(policy.check("git status").is_err());
    }

    #[test]
    fn test_shell_policy_allowlist() {
        let allowed: HashSet<String> = vec!["git", "ls", "pwd"]
            .into_iter()
            .map(String::from)
            .collect();
        let policy = ShellPolicy::AllowList(allowed);

        assert!(policy.check("git status").is_ok());
        assert!(policy.check("ls -la").is_ok());
        assert!(policy.check("pwd").is_ok());
        assert!(policy.check("rm -rf /").is_err());
        assert!(policy.check("curl evil.com").is_err());
    }

    #[test]
    fn test_shell_policy_unrestricted() {
        let policy = ShellPolicy::Unrestricted;
        assert!(policy.check("anything").is_ok());
        assert!(policy.check("rm -rf /").is_ok());
    }

    #[test]
    fn test_policy_description() {
        assert_eq!(ShellPolicy::Disabled.description(), "disabled");
        assert_eq!(ShellPolicy::Unrestricted.description(), "unrestricted");
    }

    #[tokio::test]
    async fn test_run_shell_sync_echo() {
        let output = run_shell_sync("echo hello_world").await;
        assert!(output.contains("hello_world"));
    }

    #[tokio::test]
    async fn test_run_shell_sync_invalid_command() {
        let output = run_shell_sync("nonexistent_command_12345").await;
        // Should contain error output (from stderr), not panic
        assert!(!output.is_empty());
    }
}
