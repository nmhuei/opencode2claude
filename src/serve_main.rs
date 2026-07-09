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

    /// Tavily search API key override
    #[arg(long = "tavily-api-key")]
    pub tavily_api_key: Option<String>,

    /// Exa search API key override
    #[arg(long = "exa-api-key")]
    pub exa_api_key: Option<String>,

    /// Serper.dev search API key override
    #[arg(long = "serper-api-key")]
    pub serper_api_key: Option<String>,

    /// SearXNG instance URL override
    #[arg(long = "searxng-url")]
    pub searxng_url: Option<String>,

    /// SearXNG API key override
    #[arg(long = "searxng-api-key")]
    pub searxng_api_key: Option<String>,

    /// Override max request body size in bytes (0 = unlimited)
    #[arg(long = "max-body-size")]
    pub max_body_size: Option<usize>,
}

#[tokio::main]
async fn main() {
    let args = ServeArgs::parse();

    let bridge_args = ServeArgsBridge {
        port: args.port,
        host: args.host,
        config: args.config,
        model: args.model,
        shell_policy: args.shell_policy,
        tavily_api_key: args.tavily_api_key,
        exa_api_key: args.exa_api_key,
        serper_api_key: args.serper_api_key,
        searxng_url: args.searxng_url,
        searxng_api_key: args.searxng_api_key,
        max_body_size: args.max_body_size,
    };

    run_server(bridge_args).await;
}
