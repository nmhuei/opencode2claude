//! Output formatting utilities for the OpenCode2Claude CLI.
//!
//! Provides a unified output dispatch system (`OutputFormat`) that every
//! subcommand uses for consistent rendering across human-readable,
//! machine-readable JSON, and quiet modes.

use serde::Serialize;
use std::fmt::Display;
use std::io::IsTerminal;
use std::io::Write;
use yansi::Paint;

/// Error details inside a JSON CLI response.
#[derive(Debug, Serialize)]
pub struct CliErrorInfo {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Output format selection for CLI commands.
///
/// Controls how each subcommand renders its result data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    /// Default human-readable output with colors, tables, and spinners.
    Human,
    /// Machine-readable JSON output via `serde_json`.
    Json,
    /// Minimal output — only errors/success, no decorative formatting.
    Quiet,
}

/// Render `data` in the specified `format`.
///
/// `T` must implement both `Serialize` (for JSON) and `Display` (for Human/Quiet).
/// The `Display` impl should produce the human-readable format; `Quiet` mode
/// may use a compact summary format.
pub fn render<T: Serialize + Display>(data: T, format: OutputFormat) -> anyhow::Result<String> {
    match format {
        OutputFormat::Human => Ok(data.to_string()),
        OutputFormat::Json => serde_json::to_string_pretty(&data)
            .map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}")),
        OutputFormat::Quiet => Ok(data.to_string()),
    }
}

/// Control ANSI color output.
#[derive(Debug, Clone, Copy, PartialEq, Default, clap::ValueEnum)]
pub enum ColorChoice {
    /// Use colors if stdout is a terminal (default).
    #[default]
    Auto,
    /// Always use colors, even when piping.
    Always,
    /// Never use colors (disables ANSI escape codes).
    Never,
}

/// Initialize color support based on user preference and terminal capabilities.
///
/// Call this once at the start of `main()`, before any output is written.
pub fn setup_color(choice: &ColorChoice) {
    match choice {
        ColorChoice::Never => {
            yansi::disable();
        }
        ColorChoice::Always => {
            // yansi is enabled by default, but ensure it's on
            yansi::enable();
        }
        ColorChoice::Auto => {
            let no_color_set = std::env::var_os("NO_COLOR").is_some();
            let is_terminal = std::io::stdout().is_terminal();
            if no_color_set || !is_terminal {
                yansi::disable();
            }
        }
    }
}

/// A simple key-value pair for rendering config/status output.
#[derive(Debug, Serialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

impl Display for KeyValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.key, self.value)
    }
}

/// Prompt the user for yes/no confirmation via stdin.
///
/// Uses `tokio::task::spawn_blocking` to avoid stalling the async runtime.
/// Returns `true` if the user typed "y" or "yes".
pub async fn confirm_action(action: &str) -> bool {
    let action = action.to_string();
    tokio::task::spawn_blocking(move || {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let _ = write!(
            stdout,
            "{} Are you sure you want to {}? [y/N] ",
            "⚠".yellow().bold(),
            action
        );
        let _ = stdout.flush();
        let mut input = String::new();
        stdin.read_line(&mut input).ok();
        input.trim().to_lowercase() == "y" || input.trim().to_lowercase() == "yes"
    })
    .await
    .unwrap_or(false)
}

// ── Structured JSON output for CLI errors and responses ──

/// Wrapper for JSON CLI responses.
#[derive(Debug, Serialize)]
pub struct CliResponse<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CliErrorInfo>,
}

/// Print an error response as JSON and exit with status 1.
///
/// Emits a structured JSON object to stderr and calls `std::process::exit(1)`.
/// This is used by all CLI commands in JSON mode to guarantee machine-parseable errors.
pub fn print_json_error(code: &str, message: &str, hint: Option<&str>) -> ! {
    let resp = CliResponse::<()> {
        ok: false,
        data: None,
        error: Some(CliErrorInfo {
            code: code.to_string(),
            message: message.to_string(),
            hint: hint.map(|s| s.to_string()),
        }),
    };
    if let Ok(s) = serde_json::to_string_pretty(&resp) {
        eprintln!("{s}");
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct TestData {
        name: String,
        count: u32,
    }

    impl Display for TestData {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}: {}", self.name, self.count)
        }
    }

    #[test]
    fn test_render_human_calls_display() {
        let data = TestData {
            name: "test".into(),
            count: 42,
        };
        let result = render(&data, OutputFormat::Human).unwrap();
        assert_eq!(result, "test: 42");
    }

    #[test]
    fn test_render_json_valid() {
        let data = TestData {
            name: "json-test".into(),
            count: 7,
        };
        let result = render(&data, OutputFormat::Json).unwrap();
        assert!(result.contains("\"name\""));
        assert!(result.contains("\"count\""));
        assert!(result.contains("json-test"));
    }

    #[test]
    fn test_render_quiet_calls_display() {
        let data = TestData {
            name: "quiet".into(),
            count: 0,
        };
        let result = render(&data, OutputFormat::Quiet).unwrap();
        assert_eq!(result, "quiet: 0");
    }

    #[test]
    fn test_color_choice_default_is_auto() {
        assert_eq!(ColorChoice::default(), ColorChoice::Auto);
    }

    #[test]
    fn test_key_value_display() {
        let kv = KeyValue {
            key: "port".into(),
            value: "4000".into(),
        };
        assert_eq!(kv.to_string(), "port: 4000");
    }

    #[test]
    fn test_key_value_serialize() {
        let kv = KeyValue {
            key: "key".into(),
            value: "val".into(),
        };
        let json = serde_json::to_string(&kv).unwrap();
        assert!(json.contains("\"key\":\"key\""));
        assert!(json.contains("\"value\":\"val\""));
    }

    #[test]
    fn test_output_format_partial_eq() {
        assert_eq!(OutputFormat::Human, OutputFormat::Human);
        assert_ne!(OutputFormat::Human, OutputFormat::Json);
        assert_ne!(OutputFormat::Json, OutputFormat::Quiet);
    }
}
