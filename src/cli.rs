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
    about = "A local Anthropic and OpenAI-compatible model gateway",
    long_about = "OpenCode2API accepts Anthropic Messages and OpenAI Chat Completions requests, then routes them to the configured OpenCode-compatible model provider.",
    after_help = "Quick start:\n  opencode2api                 # load env and open Claude Code (bridge must already be running)\n  opencode2api server status\n  opencode2api set env         # env only, no Claude launch\n  opencode2api doctor\n\nTip: bare `opencode2api` launches Claude Code natively when the bridge is already running. The bash/zsh hook is only needed for `opencode2api set env` to modify the current shell. Use --json for automation, --quiet for shell-friendly output, and --color never when piping logs.",
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
        .header(AnsiColor::White.on_default().bold())
        .usage(AnsiColor::White.on_default().bold())
        .literal(AnsiColor::Cyan.on_default().bold())
        .placeholder(AnsiColor::BrightBlack.on_default())
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

    /// Apply session settings through the installed shell integration
    #[command(subcommand)]
    Set(SetCommand),

    /// Install or inspect the parent-shell integration hook
    #[command(subcommand)]
    Shell(ShellCommand),

    /// Generate and optionally persist bridge API keys
    #[command(name = "api-key", subcommand)]
    ApiKey(ApiKeyCommand),

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
    Restart {
        /// Print the planned operations without changing containers
        #[arg(long)]
        dry_run: bool,
    },

    /// Purge and recreate all primary proxy containers
    Purge {
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Print the planned operations without changing containers
        #[arg(long)]
        dry_run: bool,
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

/// Session-setting commands. `set env` is normally intercepted by the shell hook.
#[derive(Subcommand, Debug, Clone)]
pub enum SetCommand {
    /// Load the canonical OpenCode2API client environment into the current shell
    Env,
}

/// Shell-integration management commands.
#[derive(Subcommand, Debug, Clone)]
pub enum ShellCommand {
    /// Install or refresh the managed shell hook
    Install(ShellInstallArgs),
    /// Remove the managed shell hook
    Uninstall(ShellInstallArgs),
    /// Print the managed shell hook without modifying files
    Hook(ShellHookArgs),
}

/// Shells supported by the parent-shell integration.
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
pub enum CliIntegrationShell {
    Auto,
    Bash,
    Zsh,
}

impl std::fmt::Display for CliIntegrationShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Auto => "auto",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        };
        write!(f, "{value}")
    }
}

/// Arguments for installing or removing the shell hook.
#[derive(Args, Debug, Clone)]
pub struct ShellInstallArgs {
    /// Shell to configure; auto detects from $SHELL
    #[arg(long, value_enum, default_value_t = CliIntegrationShell::Auto)]
    pub shell: CliIntegrationShell,

    /// Override the rc file path (primarily for managed/custom setups)
    #[arg(long)]
    pub rc: Option<String>,
}

/// Arguments for printing the shell hook.
#[derive(Args, Debug, Clone)]
pub struct ShellHookArgs {
    /// Shell syntax to print; bash and zsh currently share the same hook
    #[arg(long, value_enum, default_value_t = CliIntegrationShell::Auto)]
    pub shell: CliIntegrationShell,
}

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

/// API-key management subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum ApiKeyCommand {
    /// Generate cryptographically secure bridge API keys
    Generate(ApiKeyGenerateArgs),
}

/// Arguments for `api-key generate`.
#[derive(Args, Debug, Clone)]
pub struct ApiKeyGenerateArgs {
    /// Number of keys to generate (1-20)
    #[arg(long, default_value_t = 1)]
    pub count: usize,

    /// Random bytes per key (16-64; 32 = 256-bit)
    #[arg(long, default_value_t = 32)]
    pub bytes: usize,

    /// Prefix placed before the random hexadecimal value
    #[arg(long, default_value = "sk-oc2-")]
    pub prefix: String,

    /// Save generated keys into auth_tokens in the active TOML config
    #[arg(long)]
    pub save: bool,

    /// Config file to update; defaults to the resolved active config
    #[arg(short, long)]
    pub config: Option<String>,

    /// Replace existing auth_tokens instead of appending
    #[arg(long, requires = "save")]
    pub replace: bool,
}

/// Dashboard management subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum DashboardCommand {
    /// Start the server if needed and print the dashboard URL
    Start,

    /// Check dashboard service status and active auth token details
    Status,
}
