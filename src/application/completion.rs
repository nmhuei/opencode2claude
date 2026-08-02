//! Shell completion generation as an in-memory application service.

use crate::cli;
use clap::CommandFactory;
use clap_complete::{generate, Shell};

pub fn generate_completion(shell: &str) -> Result<String, String> {
    let shell = parse_shell(shell)?;
    let mut command = cli::Cli::command();
    let name = command.get_name().to_string();
    let mut output = Vec::new();
    generate(shell, &mut command, name, &mut output);
    String::from_utf8(output).map_err(|error| error.to_string())
}

fn parse_shell(value: &str) -> Result<Shell, String> {
    match value.to_ascii_lowercase().as_str() {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        "powershell" | "pwsh" => Ok(Shell::PowerShell),
        "elvish" => Ok(Shell::Elvish),
        _ => Err("Supported shells: bash, zsh, fish, powershell, elvish".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_bash_completion() {
        let output = generate_completion("bash").unwrap();
        assert!(output.contains("opencode2api"));
        assert!(generate_completion("unknown").is_err());
    }
}
