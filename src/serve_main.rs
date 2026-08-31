//! The HTTP bridge server binary.
//!
//! Exclusively responsible for starting and running the foreground Axum HTTP server.

use clap::Parser;
use opencode2api::server::{run_server, ServeArgsBridge};

/// Argument parser for the foreground serve engine.
#[derive(Parser, Debug)]
#[command(
    name = "opencode2api-serve",
    version,
    about = "OpenCode2api foreground HTTP bridge server engine"
)]
pub struct ServeArgs {
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
    #[arg(long = "shell-policy")]
    pub shell_policy: Option<String>,

    /// SearXNG instance URL override
    #[arg(long = "searxng-url")]
    pub searxng_url: Option<String>,

    /// Override max request body size in bytes (0 = unlimited)
    #[arg(long = "max-body-size")]
    pub max_body_size: Option<usize>,

    /// Override egress mode (direct or proxy)
    #[arg(long = "egress-mode", hide = true)]
    pub egress_mode: Option<String>,

    /// Upstream base URL (e.g. https://api.b.ai/v1 or https://opencode.ai/zen/v1)
    #[arg(long = "upstream-base-url")]
    pub upstream_base_url: Option<String>,
}

#[tokio::main]
async fn main() {
    // The detached foreground engine can also be launched directly. Resolve
    // `.env` from the working directory or from the executable's ancestors so
    // daemon launches from `$HOME` keep the same configuration as the CLI.
    let _ = opencode2api::config::load_dotenv();

    let args = ServeArgs::parse();

    let bridge_args = ServeArgsBridge {
        port: args.port,
        host: args.host,
        config: args.config,
        model: args.model,
        shell_policy: args.shell_policy,
        tavily_api_key: None,
        exa_api_key: None,
        serper_api_key: None,
        searxng_url: args.searxng_url,
        searxng_api_key: None,
        max_body_size: args.max_body_size,
        egress_mode: args.egress_mode,
        upstream_base_url: args.upstream_base_url,
        upstream_api_key: None,
    };

    if let Err(error) = run_server(bridge_args).await {
        eprintln!("server failed: {error}");
        std::process::exit(1);
    }
}
