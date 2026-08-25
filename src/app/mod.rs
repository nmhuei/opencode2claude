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
use crate::output::{setup_color, OutputFormat};
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

        // Default: keep legacy foreground behavior, but do not try to bind over
        // an already-running bridge. This avoids confusing "address in use"
        // errors when users type `o2a` after `o2a server start`.
        None => server::cmd_run_server(crate::server::ServeArgsBridge::default()).await,
    }
}
