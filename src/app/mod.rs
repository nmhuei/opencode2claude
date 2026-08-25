//! CLI application orchestration.
//!
//! This module owns command dispatch only. Command implementations live in
//! focused submodules so the binary entry point stays trivial and testable.

mod dashboard;
mod proxy;
mod server;
mod utility;
mod view;

use crate::cli::{self, Command};
use crate::config::{BridgeConfig, CliOverrides};
use crate::output::{setup_color, OutputFormat};
use crate::supervisor::SupervisorStatus;
use clap::Parser;
use yansi::Paint;

pub async fn run_cli() {
    // Load environment variables from the working directory or from a `.env`
    // beside/above the executable. This keeps daemon launches from `$HOME`
    // consistent with direct launches from the repository.
    let _ = crate::config::load_dotenv();

    let cli = cli::Cli::parse();

    // Initialize color support BEFORE any output
    setup_color(&cli.color);

    // Determine output format from global flags
    let fmt = if cli.json {
        OutputFormat::Json
    } else if cli.quiet {
        OutputFormat::Quiet
    } else {
        OutputFormat::Human
    };

    match cli.command {
        // New server subcommand group
        Some(Command::Server(cmd)) => server::cmd_server(cmd, fmt).await,

        // New dashboard subcommand group
        Some(Command::Dashboard(cmd)) => dashboard::cmd_dashboard(cmd, fmt).await,

        // New commands
        Some(Command::Doctor) => utility::cmd_doctor(fmt).await,
        Some(Command::Completion(args)) => utility::cmd_completion(args, fmt),
        Some(Command::Update(args)) => utility::cmd_update(args, fmt).await,
        Some(Command::Init(args)) => utility::cmd_init(args, fmt).await,
        Some(Command::Env) => utility::cmd_env(fmt),
        Some(Command::Set(cmd)) => utility::cmd_set(cmd, fmt),
        Some(Command::Shell(cmd)) => utility::cmd_shell(cmd, fmt),
        Some(Command::ApiKey(cmd)) => utility::cmd_api_key(cmd, fmt),

        // Proxy group (unchanged, but uses fmt)
        Some(Command::Proxy(cmd)) => proxy::cmd_proxy(cmd, fmt).await,

        // Legacy aliases (backward compatible) — show deprecation hint once
        Some(Command::Serve(args)) => {
            eprintln!(
                "{} `serve` is deprecated, use `server start -f` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_serve_legacy(args).await
        }
        Some(Command::Start(args)) => {
            eprintln!(
                "{} `start` is deprecated, use `server start` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_start_legacy(args, fmt).await
        }
        Some(Command::Status(args)) => {
            eprintln!(
                "{} `status` is deprecated, use `server status` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_status_legacy(args, fmt).await
        }
        Some(Command::Stop(args)) => {
            eprintln!(
                "{} `stop` is deprecated, use `server stop` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_stop_legacy(args)
        }
        Some(Command::Restart) => {
            eprintln!(
                "{} `restart` is deprecated, use `server restart` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_restart_legacy(fmt).await
        }
        Some(Command::Logs) => {
            eprintln!(
                "{} `logs` is deprecated, use `server logs` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_logs_legacy(fmt)
        }

        // Default: the bridge lifecycle stays explicit. A bare invocation only
        // launches Claude Code after confirming the configured bridge is running.
        None => launch_claude_code(),
    }
}

fn launch_claude_code() {
    let resolved = BridgeConfig::from_env_and_cli(CliOverrides::default());
    let supervisor = server::resolve_runtime(None, None);

    match supervisor.status() {
        Ok(SupervisorStatus::Running { .. }) => {}
        Ok(SupervisorStatus::Stopped) => {
            eprintln!(
                "opencode2api: bridge is not running. Start it first with: opencode2api server start"
            );
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("opencode2api: could not determine bridge status: {error}");
            std::process::exit(1);
        }
    }

    let mut command = std::process::Command::new("claude");
    command.args(["--permission-mode", "bypassPermissions"]);
    for (key, value) in crate::application::integration::process_environment(&resolved) {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }

    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("opencode2api: Claude Code is not installed or not available in PATH.");
            std::process::exit(127);
        }
        Err(error) => {
            eprintln!("opencode2api: failed to launch Claude Code: {error}");
            std::process::exit(1);
        }
    }
}
