//! Command services kept separate from Clap parsing and presentation.

pub mod config;
pub mod provider;

use crate::output::OutputFormat;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{0}")]
    Message(String),
}

impl From<String> for CommandError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<&str> for CommandError {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

pub fn emit(fmt: OutputFormat, title: &str, quiet: &str, value: Value) {
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"version": 1, "status": "ok", "data": value})
        ),
        OutputFormat::Quiet => println!("{quiet}"),
        OutputFormat::Human => {
            println!("\n◆ OpenCode2API");
            println!("  {title}\n");
            if let Some(object) = value.as_object() {
                for (key, value) in object {
                    println!("  {:<22} {}", key, display_value(value));
                }
            } else {
                println!("  {value}");
            }
            println!();
        }
    }
}

pub fn emit_error(fmt: OutputFormat, error: &CommandError) -> ! {
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"version": 1, "status": "error", "error": {"message": error.to_string()}})
        ),
        OutputFormat::Quiet | OutputFormat::Human => eprintln!("error: {error}"),
    }
    std::process::exit(1)
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        _ => value.to_string(),
    }
}
