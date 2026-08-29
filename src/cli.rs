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
    after_help = "Quick start:\n  opencode2api                 # open a new Claude Code conversation\n  opencode2api -c              # continue the latest conversation in this directory\n  opencode2api -r              # open Claude Code's resume picker\n  opencode2api -r <session-id> # resume a specific conversation\n  opencode2api server status\n  opencode2api set env         # env only, no Claude launch\n\nTip: the bridge must already be running. Bare `opencode2api` launches Claude Code natively with bypassPermissions. The bash/zsh hook is only needed for `opencode2api set env` to modify the current shell.",
    styles = clap_styles()
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Continue the most recent Claude Code conversation in the current directory
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    pub continue_session: bool,

    /// Resume a Claude Code conversation by session ID, or open the resume picker when omitted
    #[arg(short = 'r', long = "resume", num_args = 0..=1, default_missing_value = "", conflicts_with = "continue_session")]
    pub resume: Option<String>,

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

    /// List available free models from OpenCode Zen
    #[command(alias = "models")]
    List(ListArgs),

    /// Inspect or set the active model
    Model(ModelArgs),

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
    ///
    /// Refuses to act when the configured port answers but no supervisor PID
    /// file tracks the listener (exit code 4); pass --unmanaged to verify that
    /// listener's process identity, adopt it, and stop it.
    Stop(ServerStopArgs),

    /// Show bridge status
    Status(ServerStatusArgs),

    /// Restart the bridge daemon
    ///
    /// Refuses to act when the configured port answers but no supervisor PID
    /// file tracks the listener (exit code 4); pass --unmanaged to verify that
    /// listener's process identity, adopt it, and restart over it.
    Restart(ServerRestartArgs),

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

    /// Adopt an untracked listener on the configured port before stopping it:
    /// verifies the listener's executable and start time via /proc, records
    /// the supervisor PID file, then terminates through the normal verified
    /// flow. Without this flag an untracked listener is refused (exit code 4).
    #[arg(long = "unmanaged")]
    pub unmanaged: bool,
}

/// Arguments for `server restart`.
#[derive(Args, Debug, Default)]
pub struct ServerRestartArgs {
    /// Adopt an untracked listener on the configured port before restarting
    /// over it: verifies the listener's executable and start time via /proc,
    /// records the supervisor PID file, stops it through the normal verified
    /// flow, then starts a fresh supervised daemon. Without this flag an
    /// untracked listener aborts the restart before anything is started
    /// (exit code 4).
    #[arg(long = "unmanaged")]
    pub unmanaged: bool,
}

/// Arguments for `server status`.
///
/// Deliberately distinct from [`ServerStopArgs`]: status only reads runtime
/// state, so stop-only flags (`--purge`, `--unmanaged`) must not appear in its
/// help or be silently accepted as no-ops.
#[derive(Args, Debug, Default)]
pub struct ServerStatusArgs {
    /// Override bridge port
    #[arg(short = 'p', long)]
    pub port: Option<u16>,

    /// Override bind address
    #[arg(long)]
    pub host: Option<String>,
}

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

    /// Resolve and report the candidate without changing the installed binary
    #[arg(long, conflicts_with = "force")]
    pub dry_run: bool,

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

/// List available models.
#[derive(Args, Debug, Clone, Default)]
pub struct ListArgs {
    /// Probe live availability and latency from upstream API
    #[arg(long, short = 'p')]
    pub probe: bool,
}

/// Manage configured and available models.
#[derive(Args, Debug, Clone, Default)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: Option<ModelSubcommand>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ModelSubcommand {
    /// List available free models
    List(ListArgs),

    /// Set the active model in configuration
    Set(ModelSetArgs),

    /// Show current active model profile and tuning parameters
    Status,
}

#[derive(Args, Debug, Clone)]
pub struct ModelSetArgs {
    /// The model identifier to use (e.g. mimo-v2.5-free, nemotron-3-ultra-free)
    pub model: String,

    /// Config file to update; defaults to the resolved active config
    #[arg(short, long)]
    pub config: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("opencode2api").chain(args.iter().copied()))
    }

    #[test]
    fn update_dry_run_is_non_destructive_alias_and_conflicts_with_force() {
        let parsed = parse(&["update", "--dry-run"]).expect("update dry-run");
        let Command::Update(args) = parsed.command.unwrap() else {
            panic!("expected update command");
        };
        assert!(args.dry_run);
        assert!(!args.check);
        assert!(!args.force);
        assert!(parse(&["update", "--dry-run", "--force"]).is_err());
    }

    #[test]
    fn restart_accepts_unmanaged_override_and_defaults_to_refusing() {
        let plain = Cli::try_parse_from(["opencode2api", "server", "restart"]).unwrap();
        let Command::Server(ServerCommand::Restart(args)) = plain.command.unwrap() else {
            panic!("expected server restart");
        };
        assert!(!args.unmanaged, "plain restart must refuse by default");

        let override_parse =
            Cli::try_parse_from(["opencode2api", "server", "restart", "--unmanaged"]).unwrap();
        let Command::Server(ServerCommand::Restart(args)) = override_parse.command.unwrap() else {
            panic!("expected server restart");
        };
        assert!(args.unmanaged);
    }

    #[test]
    fn stop_keeps_unmanaged_override_and_status_rejects_stop_only_flags() {
        let stop = Cli::try_parse_from(["opencode2api", "server", "stop", "--unmanaged"]).unwrap();
        let Command::Server(ServerCommand::Stop(args)) = stop.command.unwrap() else {
            panic!("expected server stop");
        };
        assert!(args.unmanaged);

        // `status` shares no args struct with `stop`, so its dead flags are
        // gone from both help and the parser.
        let err = Cli::try_parse_from(["opencode2api", "server", "status", "--purge"])
            .err()
            .expect("status must reject the stop-only --purge flag");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        let err = Cli::try_parse_from(["opencode2api", "server", "status", "--unmanaged"])
            .err()
            .expect("status must reject the stop-only --unmanaged flag");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);

        // Plain status still parses and keeps its port/host overrides.
        let status =
            Cli::try_parse_from(["opencode2api", "server", "status", "-p", "4321"]).unwrap();
        let Command::Server(ServerCommand::Status(args)) = status.command.unwrap() else {
            panic!("expected server status");
        };
        assert_eq!(args.port, Some(4321));
    }

    #[test]
    fn legacy_hidden_aliases_still_parse() {
        // The deprecated top-level aliases stay unit-style: their handlers
        // resolve runtime from config alone and point refusals at the modern
        // commands.
        for argv in [vec!["restart"], vec!["stop"], vec!["status"], vec!["start"]] {
            assert!(
                parse(&argv).is_ok(),
                "legacy alias `{}` must keep parsing",
                argv.join(" ")
            );
        }
        // No subcommand is valid (bare invocation launches Claude Code); an
        // empty parse must not error at the CLI layer.
        assert!(parse(&[]).is_ok());
    }

    #[test]
    fn list_and_model_subcommands_parse() {
        let list = parse(&["list"]).unwrap();
        let Command::List(args) = list.command.unwrap() else {
            panic!("expected list command");
        };
        assert!(!args.probe);

        let list_probed = parse(&["list", "--probe"]).unwrap();
        let Command::List(args) = list_probed.command.unwrap() else {
            panic!("expected list --probe command");
        };
        assert!(args.probe);

        let model_set = parse(&["model", "set", "mimo-v2.5-free"]).unwrap();
        let Command::Model(model_args) = model_set.command.unwrap() else {
            panic!("expected model command");
        };
        let Some(ModelSubcommand::Set(set_args)) = model_args.command else {
            panic!("expected model set subcommand");
        };
        assert_eq!(set_args.model, "mimo-v2.5-free");

        let model_status = parse(&["model", "status"]).unwrap();
        let Command::Model(model_args) = model_status.command.unwrap() else {
            panic!("expected model command");
        };
        assert!(matches!(model_args.command, Some(ModelSubcommand::Status)));

        let model_bare = parse(&["model"]).unwrap();
        let Command::Model(model_args) = model_bare.command.unwrap() else {
            panic!("expected model command");
        };
        assert!(model_args.command.is_none());
    }
}
