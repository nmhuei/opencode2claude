//! Command-line interface for OpenCode2Claude.
//!
//! Uses Clap derive macros to define a subcommand-based CLI:
//! - `serve` (default): Start the API bridge server
//! - `start`: Start the supervisor daemon
//! - `status`: Show bridge status
//! - `stop`: Stop the bridge
//! - `restart`: Restart the bridge (not yet implemented)
//! - `env`: Display environment information (not yet implemented)
//! - `logs`: View bridge logs (not yet implemented)

use clap::{Args, Parser, Subcommand};

/// Command-line interface for the OpenCode2Claude bridge.
#[derive(Parser)]
#[command(
    name = "opencode2claude",
    version,
    about = "A blazing-fast API bridge connecting Claude Code to OpenCode CLI and any LLM"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Bridge subcommands.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Command {
    /// Start the API bridge server
    Serve(ServeArgs),
    /// Start the bridge as a background daemon
    Start(StartArgs),
    /// Show bridge status
    Status(StatusArgs),
    /// Stop the bridge
    Stop(StopArgs),
    /// Restart the bridge
    Restart,
    /// Display environment information
    Env,
    /// View bridge logs
    Logs,
}

/// Arguments for the `serve` subcommand.
#[derive(Args, Default)]
pub struct ServeArgs {
    /// Override bridge port
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address
    #[arg(long)]
    pub host: Option<String>,

    /// Path to custom TOML config file (default: opencode2claude.toml)
    #[arg(short = 'c', long)]
    pub config: Option<String>,

    /// Override model
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Override shell policy (disabled, allowlist, unrestricted)
    #[arg(long = "shell-policy")]
    pub shell_policy: Option<String>,

    /// Tavily search API key override
    #[arg(long)]
    pub tavily_api_key: Option<String>,

    /// Exa search API key override
    #[arg(long)]
    pub exa_api_key: Option<String>,

    /// Serper.dev search API key override
    #[arg(long)]
    pub serper_api_key: Option<String>,

    /// SearXNG instance URL override
    #[arg(long)]
    pub searxng_url: Option<String>,

    /// SearXNG API key override
    #[arg(long)]
    pub searxng_api_key: Option<String>,
}

/// Base args shared by start/status/stop (port and host).
#[derive(Args, Default)]
pub struct StartArgs {
    /// Override bridge port for the daemon
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address for the daemon
    #[arg(long)]
    pub host: Option<String>,
}

/// Arguments for the `status` subcommand.
pub type StatusArgs = StartArgs;

/// Arguments for the `stop` subcommand.
pub type StopArgs = StartArgs;
