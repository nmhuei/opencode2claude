//! Command-line interface for OpenCode2Claude.
//!
//! Defines the hierarchical command tree using Clap derive macros:
//! - `server` — manage the bridge daemon
//! - `proxy` — manage proxy pool
//! - `env` — display environment info
//! - `doctor` — diagnose common issues
//! - `update` — self-update binary
//! - `init` — generate default config
//! - `completion` — generate shell completions

use crate::output::ColorChoice;
use clap::{Args, Parser, Subcommand};

/// Command-line interface for the OpenCode2API bridge.
#[derive(Parser)]
#[command(
    name = "opencode2api",
    version,
    about = "A blazing-fast API bridge connecting Claude Code to any LLM",
    long_about = "OpenCode2API is a local HTTP proxy that translates Anthropic Messages API \n\
                  requests into OpenAI-compatible API calls. Use Claude Code with any LLM \n\
                  provider — DeepSeek, GPT-4o, Gemini, Llama, and more.",
    styles = clap_styles()
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Output in JSON format (machine-readable)
    #[arg(long, global = true, conflicts_with = "quiet")]
    pub json: bool,

    /// Minimal output (errors/success only)
    #[arg(long, global = true, conflicts_with = "json")]
    pub quiet: bool,

    /// Color output: auto (default), always, never
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::default())]
    pub color: ColorChoice,
}

/// Clap help text styling for a cyber/SOC aesthetic.
fn clap_styles() -> clap::builder::Styles {
    use clap::builder::styling::AnsiColor;
    clap::builder::Styles::styled()
        .header(AnsiColor::Green.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default().bold())
        .placeholder(AnsiColor::Blue.on_default().bold())
        .error(AnsiColor::Red.on_default().bold())
        .valid(AnsiColor::Cyan.on_default().bold())
}

/// Bridge subcommands.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Command {
    /// Manage the bridge server (start, stop, status, etc.)
    #[command(subcommand)]
    Server(ServerCommand),

    /// Manage WARP proxy pools
    #[command(subcommand)]
    Proxy(ProxyCommand),

    /// Manage the admin dashboard (start, status)
    #[command(subcommand)]
    Dashboard(DashboardCommand),

    /// Display environment information for Claude Code
    Env,

    /// Diagnose common issues with the bridge and its dependencies
    Doctor,

    /// Generate shell completion scripts
    Completion(CompletionArgs),

    /// Self-update to the latest release
    Update(UpdateArgs),

    /// Generate a default config file
    Init(InitArgs),

    // Legacy aliases (hidden, backward-compatible)
    /// Start the API bridge server (foreground)
    #[command(hide = true)]
    Serve(ServeArgs),
    /// Start the bridge as a background daemon
    #[command(hide = true)]
    Start(StartArgs),
    /// Show bridge status
    #[command(hide = true)]
    Status(StatusArgs),
    /// Stop the bridge
    #[command(hide = true)]
    Stop(StopArgs),
    /// Restart the bridge
    #[command(hide = true)]
    Restart,
    /// View bridge logs
    #[command(hide = true)]
    Logs,
}

/// Server management subcommands.
#[derive(Subcommand)]
pub enum ServerCommand {
    /// Start the bridge server
    ///
    /// By default starts as a background daemon.
    /// Use `-f` or `--foreground` to run in the current terminal.
    Start(ServerStartArgs),

    /// Stop the bridge daemon
    Stop(ServerStopArgs),

    /// Show bridge status
    Status(ServerStatusArgs),

    /// Restart the bridge daemon
    Restart,

    /// View bridge daemon logs
    Logs,

    /// Show current configuration
    Config,
}

/// Arguments for `server start`.
#[derive(Args, Debug, Default)]
pub struct ServerStartArgs {
    /// Run in foreground (don't daemonize)
    #[arg(short = 'f', long)]
    pub foreground: bool,

    /// Override bridge port
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address
    #[arg(long)]
    pub host: Option<String>,

    /// Path to custom TOML config file
    #[arg(short = 'c', long)]
    pub config: Option<String>,

    /// Override model
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Override shell policy (disabled, allowlist, unrestricted)
    #[arg(long = "shell-policy", value_enum)]
    pub shell_policy: Option<CliShellPolicy>,

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

    /// Override SearXNG API key override
    #[arg(long)]
    pub searxng_api_key: Option<String>,

    /// Skip Docker SOCKS5 proxy pool bootstrap
    #[arg(long = "no-proxy")]
    pub no_proxy: bool,

    /// Override max request body size in bytes (0 = unlimited)
    #[arg(long = "max-body-size")]
    pub max_body_size: Option<usize>,
}

/// Arguments for `server stop`.
#[derive(Args, Debug, Default)]
pub struct ServerStopArgs {
    /// Override bridge port
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address
    #[arg(long)]
    pub host: Option<String>,

    /// Remove proxy containers entirely instead of pausing them
    #[arg(long = "purge")]
    pub purge: bool,
}

/// Arguments for `server status`.
pub type ServerStatusArgs = ServerStopArgs;

/// Proxy pool management subcommands.
#[derive(Subcommand)]
pub enum ProxyCommand {
    /// List proxy pool status (table view)
    #[command(name = "ps")]
    Ps,

    /// Show proxy pool status (alias for ps)
    #[command(hide = true)]
    Status,

    /// Restart primary managed proxies
    Restart,

    /// Purge and recreate all primary proxy containers
    Purge {
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// View proxy container logs
    Logs,
}

// ── Legacy backward-compatible types ──

/// Arguments for `serve` (legacy, hidden).
#[derive(Args, Default)]
pub struct ServeArgs {
    /// Override bridge port
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address
    #[arg(long)]
    pub host: Option<String>,

    /// Path to custom TOML config file (default: opencode2api.toml)
    #[arg(short = 'c', long)]
    pub config: Option<String>,

    /// Override model
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Override shell policy (disabled, allowlist, unrestricted)
    #[arg(long = "shell-policy", value_enum)]
    pub shell_policy: Option<CliShellPolicy>,

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

    /// Override max request body size in bytes (0 = unlimited)
    #[arg(long = "max-body-size")]
    pub max_body_size: Option<usize>,
}

/// Base args shared by start/status/stop (legacy, hidden).
#[derive(Args, Default)]
pub struct StartArgs {
    /// Override bridge port for the daemon
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address for the daemon
    #[arg(long)]
    pub host: Option<String>,
}

/// Arguments for the `status` subcommand (legacy).
pub type StatusArgs = StartArgs;

/// Arguments for the `stop` subcommand (legacy).
pub type StopArgs = StartArgs;

/// Arguments for `completion <shell>`.
#[derive(Args, Debug)]
pub struct CompletionArgs {
    /// Shell to generate completions for (bash, zsh, fish, powershell, elvish)
    pub shell: clap_complete::Shell,
}

/// Arguments for `update`.
#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Check for updates without applying
    #[arg(long)]
    pub check: bool,

    /// Force reinstall even if up-to-date
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `init`.
#[derive(Args, Debug)]
pub struct InitArgs {
    /// Output path for the config file (default: ./opencode2api.toml)
    #[arg(short, long, default_value = "opencode2api.toml")]
    pub output: String,

    /// Overwrite existing file without prompting
    #[arg(short, long)]
    pub force: bool,
}

/// Shell policy override values accepted on command line.
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq)]
pub enum CliShellPolicy {
    Disabled,
    Allowlist,
    Unrestricted,
}

impl std::fmt::Display for CliShellPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            CliShellPolicy::Disabled => "disabled",
            CliShellPolicy::Allowlist => "allowlist",
            CliShellPolicy::Unrestricted => "unrestricted",
        };
        write!(f, "{value}")
    }
}

/// Dashboard management subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum DashboardCommand {
    /// Start the server if needed and print the dashboard URL
    Start,

    /// Check dashboard service status and active auth token details
    Status,
}
