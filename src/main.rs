//! OpenCode2Claude — A blazing-fast API bridge connecting Claude Code to any LLM.
//!
//! This binary provides a local HTTP server that translates Anthropic API requests
//! into OpenAI-compatible API calls forwarded to opencode.ai/zen/v1/chat/completions.

use opencode2claude::cli::{
    self, Command, CompletionArgs, InitArgs, ProxyCommand, ServerCommand, UpdateArgs,
};
use opencode2claude::config::{self, BridgeConfig};
use opencode2claude::docker;
use opencode2claude::doctor;
use opencode2claude::handlers;
use opencode2claude::middleware;
use opencode2claude::output::{self, print_json_error, setup_color, OutputFormat};
use opencode2claude::proxy_pool;
use opencode2claude::runtime::RuntimePaths;
use opencode2claude::state::AppState;
use opencode2claude::supervisor::{DaemonSpawnOptions, Supervisor, SupervisorStatus};

use clap::Command as ClapCommand;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::generate;

use axum::routing::{get, post};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use comfy_table::{presets, Cell as CtCell, Color as CtColor, ContentArrangement, Table};
use indicatif::{ProgressBar, ProgressStyle};
use std::net::SocketAddr;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use yansi::Paint;

#[tokio::main]
async fn main() {
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
        Some(Command::Server(cmd)) => cmd_server(cmd, fmt).await,

        // New commands
        Some(Command::Doctor) => cmd_doctor(fmt).await,
        Some(Command::Completion(args)) => cmd_completion(args),
        Some(Command::Update(args)) => cmd_update(args).await,
        Some(Command::Init(args)) => cmd_init(args).await,
        Some(Command::Env) => cmd_env(fmt),

        // Proxy group (unchanged, but uses fmt)
        Some(Command::Proxy(cmd)) => cmd_proxy(cmd, fmt).await,

        // Legacy aliases (backward compatible) — show deprecation hint once
        Some(Command::Serve(args)) => {
            eprintln!(
                "{} `serve` is deprecated, use `server start -f` instead",
                "ℹ".cyan().dim()
            );
            cmd_serve_legacy(args).await
        }
        Some(Command::Start(args)) => {
            eprintln!(
                "{} `start` is deprecated, use `server start` instead",
                "ℹ".cyan().dim()
            );
            cmd_start_legacy(args, fmt).await
        }
        Some(Command::Status(args)) => {
            eprintln!(
                "{} `status` is deprecated, use `server status` instead",
                "ℹ".cyan().dim()
            );
            cmd_status_legacy(args, fmt).await
        }
        Some(Command::Stop(args)) => {
            eprintln!(
                "{} `stop` is deprecated, use `server stop` instead",
                "ℹ".cyan().dim()
            );
            cmd_stop_legacy(args, fmt)
        }
        Some(Command::Restart) => {
            eprintln!(
                "{} `restart` is deprecated, use `server restart` instead",
                "ℹ".cyan().dim()
            );
            cmd_restart_legacy(fmt).await
        }
        Some(Command::Logs) => {
            eprintln!(
                "{} `logs` is deprecated, use `server logs` instead",
                "ℹ".cyan().dim()
            );
            cmd_logs_legacy(fmt)
        }

        // Default: run server in foreground
        None => cmd_run_server(ServeArgsBridge::default()).await,
    }
}

// ── New Server command group ──

async fn cmd_server(cmd: ServerCommand, fmt: OutputFormat) {
    match cmd {
        ServerCommand::Start(args) => {
            if args.foreground {
                // Run in foreground using bridge args
                let bridge_args = ServeArgsBridge {
                    port: args.port,
                    host: args.host,
                    config: args.config,
                    model: args.model,
                    shell_policy: args.shell_policy.map(|p| p.to_string()),
                    tavily_api_key: args.tavily_api_key,
                    exa_api_key: args.exa_api_key,
                    serper_api_key: args.serper_api_key,
                    searxng_url: args.searxng_url,
                    searxng_api_key: args.searxng_api_key,
                    tls_cert: args.tls_cert,
                    tls_key: args.tls_key,
                };
                cmd_run_server(bridge_args).await;
            } else {
                let bridge_args = ServeArgsBridge {
                    port: args.port,
                    host: args.host,
                    config: args.config.clone(),
                    model: args.model.clone(),
                    shell_policy: args.shell_policy.map(|p| p.to_string()),
                    tavily_api_key: args.tavily_api_key.clone(),
                    exa_api_key: args.exa_api_key.clone(),
                    serper_api_key: args.serper_api_key.clone(),
                    searxng_url: args.searxng_url.clone(),
                    searxng_api_key: args.searxng_api_key.clone(),
                    tls_cert: args.tls_cert.clone(),
                    tls_key: args.tls_key.clone(),
                };
                start_daemon(bridge_args, fmt).await;
            }
        }
        ServerCommand::Stop(args) => {
            let sup = resolve_runtime(args.port, args.host);
            match sup.stop() {
                Ok(()) => {
                    if fmt == OutputFormat::Quiet {
                        println!("stopped");
                    } else {
                        println!("Bridge stopped.");
                    }
                }
                Err(e) => {
                    if fmt == OutputFormat::Json {
                        print_json_error("stop_failed", &e.to_string(), None);
                    } else {
                        eprintln!("{} bridge: stop failed — {}", "✗".red().bold(), e);
                        eprintln!(
                            "   Hint: Is the bridge running? Try `opencode2claude server status`"
                        );
                    }
                    std::process::exit(1);
                }
            }
        }
        ServerCommand::Status(args) => {
            let sup = resolve_runtime(args.port, args.host);
            match sup.status() {
                Ok(status) => {
                    if fmt == OutputFormat::Json {
                        let status_info = ServerStatusInfo::from(Ok(status));
                        if let Ok(s) = serde_json::to_string_pretty(&status_info) {
                            println!("{s}");
                        }
                    } else {
                        cmd_print_status(status, fmt).await;
                    }
                }
                Err(e) => {
                    if fmt == OutputFormat::Json {
                        print_json_error("status_failed", &e.to_string(), None);
                    } else {
                        eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e);
                    }
                }
            }
        }
        ServerCommand::Restart => {
            let sup = resolve_runtime(None, None);
            let _ = sup.stop();
            match sup.start() {
                Ok(()) => {
                    let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
                    match fmt {
                        OutputFormat::Json => {
                            let info = ServerStatusInfo::from(Ok(status));
                            if let Ok(s) = serde_json::to_string_pretty(&info) {
                                println!("{s}");
                            }
                        }
                        OutputFormat::Quiet => match status {
                            SupervisorStatus::Running { pid, port, .. } => {
                                println!("running pid={} port={}", pid, port);
                            }
                            _ => println!("stopped"),
                        },
                        OutputFormat::Human => {
                            println!("{} Bridge restarted. {}", "✓".green().bold(), status);
                            maybe_print_proxy_table(fmt).await;
                        }
                    }
                }
                Err(e) => {
                    if fmt == OutputFormat::Json {
                        print_json_error("restart_failed", &e.to_string(), None);
                    } else {
                        eprintln!("{} restart: {}", "✗".red().bold(), e);
                        eprintln!(
                            "   Hint: Check the PID file or run `opencode2claude server start`"
                        );
                    }
                    std::process::exit(1);
                }
            }
        }
        ServerCommand::Logs => {
            show_logs(fmt);
        }
        ServerCommand::Config => {
            if fmt == OutputFormat::Json {
                #[derive(serde::Serialize)]
                struct ConfigInfo {
                    bridge_port: u16,
                    bridge_host: String,
                    auth_enabled: bool,
                    shell_policy: String,
                    model: String,
                    max_body_size: usize,
                }
                let cfg = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                let info = ConfigInfo {
                    bridge_port: cfg.bridge_port,
                    bridge_host: cfg.host.to_string(),
                    auth_enabled: cfg.auth_enabled(),
                    shell_policy: cfg.shell_policy.description().to_string(),
                    model: cfg.model.unwrap_or_else(|| "auto".to_string()),
                    max_body_size: cfg.max_body_size,
                };
                if let Ok(s) = serde_json::to_string_pretty(&info) {
                    println!("{s}");
                }
            } else {
                cmd_print_config();
            }
        }
    }
}

async fn start_daemon(args: ServeArgsBridge, fmt: OutputFormat) {
    let sup = resolve_runtime(args.port, args.host).with_spawn_options(DaemonSpawnOptions {
        config: args.config.clone(),
        model: args.model.clone(),
        shell_policy: args.shell_policy.clone(),
        tavily_api_key: args.tavily_api_key.clone(),
        exa_api_key: args.exa_api_key.clone(),
        serper_api_key: args.serper_api_key.clone(),
        searxng_url: args.searxng_url.clone(),
        searxng_api_key: args.searxng_api_key.clone(),
        tls_cert: args.tls_cert.clone(),
        tls_key: args.tls_key.clone(),
    });

    if fmt == OutputFormat::Json {
        // No spinner in JSON mode — output structured data only
        match sup.start() {
            Ok(()) => {
                let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
                let info = ServerStatusInfo::from(Ok(status));
                if let Ok(s) = serde_json::to_string_pretty(&info) {
                    println!("{s}");
                }
            }
            Err(e) => {
                let msg = format!("{}", e);
                let hint = if msg.contains("already running") {
                    Some("Run `opencode2claude server stop` first.")
                } else {
                    None
                };
                if msg.contains("already running") {
                    print_json_error("already_running", &msg, hint);
                } else {
                    print_json_error("start_failed", &msg, hint);
                }
            }
        }
        return;
    }

    // Human / Quiet: spinner while starting
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    spinner.set_message("Starting bridge daemon...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    match sup.start() {
        Ok(()) => {
            let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
            spinner.finish_with_message(format!(
                "{} Bridge started. {}",
                "✓".green().bold(),
                status
            ));
            maybe_print_proxy_table(fmt).await;
        }
        Err(e) => {
            spinner.finish_with_message(format!("{} Error: {}", "✗".red().bold(), e));
            eprintln!("   Hint: Check if the bridge is already running. Try: `opencode2claude server stop`");
            std::process::exit(1);
        }
    }
}

// ── New commands ──

async fn cmd_doctor(fmt: OutputFormat) {
    let report = doctor::run_diagnostics().await;
    match fmt {
        OutputFormat::Json => {
            if let Ok(s) = serde_json::to_string_pretty(&report) {
                println!("{s}");
            }
        }
        OutputFormat::Quiet => {
            // Quiet: only exit code, no output on success
        }
        OutputFormat::Human => {
            println!("{}", report);
        }
    }
    std::process::exit(report.summary.exit_code());
}

fn cmd_completion(args: CompletionArgs) {
    let original = cli::Cli::command();
    let name = original.get_name().to_string();

    // Strip hidden (legacy alias) subcommands from completions so users
    // discover the v2 subcommand tree instead of hidden backward-compat aliases.
    let version = original.get_version().unwrap_or_default().to_owned();
    let about = original.get_about().unwrap_or_default().to_owned();
    drop(original);

    let mut filtered = ClapCommand::new(name.clone()).version(version).about(about);

    // Only add non-hidden subcommands
    let original = cli::Cli::command();
    for sub in original.get_subcommands() {
        if !sub.is_hide_set() {
            filtered = filtered.subcommand(sub.clone());
        }
    }

    // Copy all global args
    for arg in original.get_arguments() {
        filtered = filtered.arg(arg.clone());
    }

    generate(args.shell, &mut filtered, name, &mut std::io::stdout());
}

async fn cmd_update(args: UpdateArgs) {
    use opencode2claude::update::{self, fetch_latest_release, find_matching_asset, has_update};

    let client = reqwest::Client::builder()
        .user_agent(concat!("opencode2claude/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();

    match fetch_latest_release(&client).await {
        Ok(release) => {
            let current = update::current_version();
            let available = has_update(current, &release);

            if args.check {
                // Just check mode — don't download
                if available {
                    eprintln!(
                        "{} Update available: {} → {}",
                        "↑".green().bold(),
                        current,
                        release.version
                    );
                    if !release.body.is_empty() {
                        eprintln!("\nRelease notes:\n{}", release.body);
                    }
                } else {
                    eprintln!("{} You are up-to-date ({})", "✓".green().bold(), current);
                }
                return;
            }

            if !available && !args.force {
                eprintln!(
                    "{} You are up-to-date ({}) — use --force to reinstall",
                    "✓".green().bold(),
                    current
                );
                return;
            }

            // Find matching asset for this platform
            let asset = match find_matching_asset(&release) {
                Some(a) => a,
                None => {
                    eprintln!(
                        "{} No binary available for {}/{}",
                        "✗".red().bold(),
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    );
                    eprintln!("   Supported platforms: linux (amd64, arm64), macOS (amd64, arm64)");
                    std::process::exit(1);
                }
            };

            eprintln!(
                "{} Updating {} → {} (downloading {})...",
                "↓".cyan().bold(),
                current,
                release.version,
                asset.name
            );

            match update::apply_update(&client, asset, &release).await {
                Ok(path) => {
                    eprintln!(
                        "{} Updated to {} — binary replaced at {}",
                        "✓".green().bold(),
                        release.version,
                        path.display()
                    );
                    eprintln!("   Restart the bridge if it was running.");
                }
                Err(e) => {
                    eprintln!("{} Update failed: {}", "✗".red().bold(), e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{} Failed to check for updates: {}", "✗".red().bold(), e);
            std::process::exit(1);
        }
    }
}

async fn cmd_init(args: InitArgs) {
    use opencode2claude::init::generate_config;

    let path = std::path::Path::new(&args.output);
    match generate_config(path, args.force).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{} Init failed: {}", "✗".red().bold(), e);
            std::process::exit(1);
        }
    }
}

fn cmd_env(fmt: OutputFormat) {
    if fmt == OutputFormat::Json {
        #[derive(serde::Serialize)]
        struct EnvInfo {
            anthropic_api_key: String,
            anthropic_base_url: String,
            opencode_model: Option<String>,
        }
        let port = std::env::var("BRIDGE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(config::DEFAULT_BRIDGE_PORT);
        let model = std::env::var("OPENCODE_MODEL").ok();
        let info = EnvInfo {
            anthropic_api_key: "opencode-bridge".to_string(),
            anthropic_base_url: format!("http://127.0.0.1:{}/v1", port),
            opencode_model: model,
        };
        if let Ok(s) = serde_json::to_string_pretty(&info) {
            println!("{s}");
        }
        return;
    }

    cmd_print_env();
}

// ── Proxy commands ──

async fn cmd_proxy(cmd: ProxyCommand, fmt: OutputFormat) {
    use docker::DockerError;

    let primary_ports = proxy_pool::get_primary_ports();
    let ws_ports = proxy_pool::get_warm_standby_ports();

    match cmd {
        ProxyCommand::Ps | ProxyCommand::Status => {
            let containers = docker::list_containers(
                &primary_ports
                    .iter()
                    .chain(ws_ports.iter())
                    .copied()
                    .collect::<Vec<_>>(),
            )
            .await;

            if fmt == OutputFormat::Json {
                #[derive(serde::Serialize)]
                struct ProxyNode {
                    port: u16,
                    name: String,
                    role: String,
                    running: bool,
                }
                let nodes: Vec<ProxyNode> = containers
                    .into_iter()
                    .map(|(port, name, running)| {
                        let role = if primary_ports.contains(&port) {
                            "primary"
                        } else {
                            "standby"
                        };
                        ProxyNode {
                            port,
                            name,
                            role: role.to_string(),
                            running,
                        }
                    })
                    .collect();
                if let Ok(s) = serde_json::to_string_pretty(&nodes) {
                    println!("{s}");
                }
                return;
            }

            if fmt == OutputFormat::Quiet {
                // Quiet: no output for proxy ps
                return;
            }

            println!();
            println!(" {}", " Proxy Pool Status".cyan().bold());
            let proxy_table = print_proxy_table().await;
            println!("{}", proxy_table);
            println!();

            println!(
                " {} Primary: {:?}   Standby: {:?}",
                "ℹ".cyan().dim(),
                primary_ports,
                ws_ports
            );
            println!(
                " {} Warm-standby proxies are never modified by CLI.",
                "ℹ".cyan().dim()
            );
        }
        ProxyCommand::Restart => {
            if fmt == OutputFormat::Json {
                #[derive(serde::Serialize)]
                struct RestartResult {
                    port: u16,
                    status: String,
                }
                let mut results = Vec::new();
                for port in proxy_pool::get_primary_ports() {
                    let status = match docker::create_container(port).await {
                        Ok(()) => "ok".to_string(),
                        Err(DockerError::Protected(msg)) => format!("skipped: {}", msg),
                        Err(e) => format!("error: {}", e),
                    };
                    results.push(RestartResult { port, status });
                }
                if let Ok(s) = serde_json::to_string_pretty(&results) {
                    println!("{s}");
                }
                return;
            }

            let ports = proxy_pool::get_primary_ports();
            let mp = indicatif::MultiProgress::new();
            let sty =
                ProgressStyle::with_template("{prefix} [{bar:20.cyan/blue}] {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("=> ");

            // Create progress bars
            let mut bars: Vec<(u16, ProgressBar)> = Vec::new();
            for port in &ports {
                let pb = mp.add(ProgressBar::new(1));
                pb.set_style(sty.clone());
                pb.set_prefix(format!("  {}", port));
                pb.set_message("restarting...");
                bars.push((*port, pb));
            }

            // Run all restarts sequentially, updating each bar
            for (port, pb) in &bars {
                match docker::create_container(*port).await {
                    Ok(()) => {
                        pb.set_message("OK".to_string());
                        pb.inc(1);
                    }
                    Err(DockerError::Protected(msg)) => {
                        pb.set_message(format!("SKIPPED ({})", msg));
                        pb.finish();
                    }
                    Err(e) => {
                        pb.set_message(format!("ERROR: {}", e));
                        pb.finish();
                    }
                }
            }

            for (_, pb) in bars {
                pb.finish_and_clear();
            }
            mp.clear().unwrap();

            println!();
            println!(
                " {}",
                "Warm-standby proxies (40004, 40005) are always protected."
                    .cyan()
                    .dim()
            );
        }
        ProxyCommand::Logs => {
            if fmt == OutputFormat::Json {
                #[derive(serde::Serialize)]
                struct ProxyLog {
                    port: u16,
                    name: String,
                    logs: String,
                }
                let mut results = Vec::new();
                for port in proxy_pool::get_primary_ports() {
                    if let Ok(logs) = docker::container_logs(port, 50).await {
                        results.push(ProxyLog {
                            port,
                            name: docker::container_name(port),
                            logs,
                        });
                    }
                }
                if let Ok(s) = serde_json::to_string_pretty(&results) {
                    println!("{s}");
                }
                return;
            }

            for port in proxy_pool::get_primary_ports() {
                match docker::container_logs(port, 50).await {
                    Ok(logs) => {
                        println!(
                            "{} {} ({})",
                            "▶".cyan().bold(),
                            docker::container_name(port),
                            port
                        );
                        println!("{}", logs);
                    }
                    Err(e) => eprintln!(
                        "{} Error getting logs for port {}: {}",
                        "✗".red().bold(),
                        port,
                        e
                    ),
                }
            }
        }
        ProxyCommand::Purge { yes } => {
            if fmt == OutputFormat::Json {
                // JSON path: no spinner, no confirm
                #[derive(serde::Serialize)]
                struct PurgeResult {
                    port: u16,
                    action: String,
                    status: String,
                }
                let mut results = Vec::new();
                for port in proxy_pool::get_primary_ports() {
                    let rs = match docker::remove_container(port).await {
                        Ok(()) => "removed".to_string(),
                        Err(DockerError::Protected(msg)) => format!("skipped: {}", msg),
                        Err(e) => format!("error: {}", e),
                    };
                    results.push(PurgeResult {
                        port,
                        action: "remove".into(),
                        status: rs,
                    });
                    let cs = match docker::create_container(port).await {
                        Ok(()) => "ok".to_string(),
                        Err(e) => format!("error: {}", e),
                    };
                    results.push(PurgeResult {
                        port,
                        action: "create".into(),
                        status: cs,
                    });
                }
                if let Ok(s) = serde_json::to_string_pretty(&results) {
                    println!("{s}");
                }
                return;
            }

            // Quiet mode: skip confirm, no decoration
            if fmt == OutputFormat::Quiet {
                for port in proxy_pool::get_primary_ports() {
                    let _ = docker::remove_container(port).await;
                    let _ = docker::create_container(port).await;
                }
                return;
            }

            // Human mode: confirm + MultiProgress
            if !yes && !output::confirm_action("purge all primary proxies").await {
                eprintln!("Aborted.");
                return;
            }

            let ports = proxy_pool::get_primary_ports();
            let mp = indicatif::MultiProgress::new();
            let sty =
                ProgressStyle::with_template("{prefix} [{bar:20.cyan/blue}] {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("=> ");

            // Phase 1: remove
            let mut bars: Vec<(u16, ProgressBar)> = Vec::new();
            for port in &ports {
                let pb = mp.add(ProgressBar::new(1));
                pb.set_style(sty.clone());
                pb.set_prefix(format!("  {} remove", port));
                pb.set_message("removing...");
                bars.push((*port, pb));
            }

            for (port, pb) in &bars {
                match docker::remove_container(*port).await {
                    Ok(()) => {
                        pb.set_message("removed".to_string());
                        pb.inc(1);
                    }
                    Err(DockerError::Protected(msg)) => {
                        pb.set_message(format!("SKIPPED ({})", msg));
                        pb.finish();
                    }
                    Err(e) => {
                        pb.set_message(format!("ERROR: {}", e));
                        pb.finish();
                    }
                }
            }
            for (_, pb) in bars {
                pb.finish_and_clear();
            }

            // Phase 2: recreate
            let mut bars: Vec<(u16, ProgressBar)> = Vec::new();
            for port in &ports {
                let pb = mp.add(ProgressBar::new(1));
                pb.set_style(sty.clone());
                pb.set_prefix(format!("  {} recreate", port));
                pb.set_message("creating...");
                bars.push((*port, pb));
            }

            for (port, pb) in &bars {
                match docker::create_container(*port).await {
                    Ok(()) => {
                        pb.set_message("OK".to_string());
                        pb.inc(1);
                    }
                    Err(e) => {
                        pb.set_message(format!("ERROR: {}", e));
                        pb.finish();
                    }
                }
            }
            for (_, pb) in bars {
                pb.finish_and_clear();
            }

            mp.clear().unwrap();
            println!();
            println!(
                " {}",
                "Warm-standby proxies (40004, 40005) are always protected."
                    .cyan()
                    .dim()
            );
        }
    }
}

// ── Legacy backward-compat commands ──

#[derive(Clone, Default)]
struct ServeArgsBridge {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub config: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

async fn cmd_serve_legacy(args: cli::ServeArgs) {
    let bridge_args = ServeArgsBridge {
        port: args.port,
        host: args.host,
        config: args.config,
        model: args.model,
        shell_policy: args.shell_policy.map(|p| p.to_string()),
        tavily_api_key: args.tavily_api_key,
        exa_api_key: args.exa_api_key,
        serper_api_key: args.serper_api_key,
        searxng_url: args.searxng_url,
        searxng_api_key: args.searxng_api_key,
        tls_cert: args.tls_cert,
        tls_key: args.tls_key,
    };
    cmd_run_server(bridge_args).await;
}

async fn cmd_start_legacy(args: cli::StartArgs, fmt: OutputFormat) {
    start_daemon(
        ServeArgsBridge {
            port: args.port,
            host: args.host,
            ..Default::default()
        },
        fmt,
    )
    .await;
}

async fn cmd_status_legacy(args: cli::StatusArgs, fmt: OutputFormat) {
    let sup = resolve_runtime(args.port, args.host);
    match sup.status() {
        Ok(status) => {
            if fmt == OutputFormat::Json {
                let status_info = ServerStatusInfo::from(Ok(status));
                if let Ok(s) = serde_json::to_string_pretty(&status_info) {
                    println!("{s}");
                }
            } else {
                cmd_print_status(status, fmt).await;
            }
        }
        Err(e) => {
            if fmt == OutputFormat::Json {
                print_json_error("status_failed", &e.to_string(), None);
            } else {
                eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e);
            }
        }
    }
}

fn cmd_stop_legacy(args: cli::StopArgs, fmt: OutputFormat) {
    let sup = resolve_runtime(args.port, args.host);
    match sup.stop() {
        Ok(()) => {
            if fmt == OutputFormat::Quiet {
                println!("stopped");
            } else {
                println!("Bridge stopped.");
            }
        }
        Err(e) => {
            if fmt == OutputFormat::Json {
                print_json_error("stop_failed", &e.to_string(), None);
            } else {
                eprintln!("{} bridge: stop failed — {}", "✗".red().bold(), e);
                eprintln!("   Hint: Is the bridge running? Try `opencode2claude server status`");
            }
            std::process::exit(1);
        }
    }
}

async fn cmd_restart_legacy(fmt: OutputFormat) {
    let sup = resolve_runtime(None, None);
    let _ = sup.stop();
    match sup.start() {
        Ok(()) => {
            let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
            match fmt {
                OutputFormat::Json => {
                    let info = ServerStatusInfo::from(Ok(status));
                    if let Ok(s) = serde_json::to_string_pretty(&info) {
                        println!("{s}");
                    }
                }
                OutputFormat::Quiet => match status {
                    SupervisorStatus::Running { pid, port, .. } => {
                        println!("running pid={} port={}", pid, port);
                    }
                    _ => println!("stopped"),
                },
                OutputFormat::Human => {
                    println!("{} Bridge restarted. {}", "✓".green().bold(), status);
                    maybe_print_proxy_table(fmt).await;
                }
            }
        }
        Err(e) => {
            eprintln!("{} restart: {}", "✗".red().bold(), e);
            eprintln!("   Hint: Check the PID file or run `opencode2claude server start`");
            std::process::exit(1);
        }
    }
}

fn cmd_logs_legacy(fmt: OutputFormat) {
    show_logs(fmt);
}

// ── Print utilities ──

fn uptime_str(started_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let elapsed_secs = (now.saturating_sub(started_at)) / 1000;
    let hours = elapsed_secs / 3600;
    let mins = (elapsed_secs % 3600) / 60;
    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

/// Print proxy pool status table (used by `server status` and `proxy ps`).
async fn print_proxy_table() -> Table {
    let primary_ports = proxy_pool::get_primary_ports();
    let ws_ports = proxy_pool::get_warm_standby_ports();
    let all_ports: Vec<u16> = primary_ports
        .iter()
        .chain(ws_ports.iter())
        .copied()
        .collect();
    let containers = docker::list_containers(&all_ports).await;

    let mut table = Table::new();
    table
        .load_preset(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Node", "Role", "Status", "Port"]);

    for (port, name, running) in &containers {
        let short_name = name.strip_prefix("opencode-warp-").unwrap_or(name);
        let role = if primary_ports.contains(port) {
            "Primary"
        } else {
            "Standby"
        };
        let (status_str, status_color) = if *running {
            ("● Alive", CtColor::Green)
        } else {
            ("● Dead", CtColor::Red)
        };

        table.add_row(vec![
            CtCell::new(short_name),
            CtCell::new(role),
            CtCell::new(status_str).fg(status_color),
            CtCell::new(port.to_string()),
        ]);
    }

    table
}

/// Print proxy pool table in Human mode; no-op in Json/Quiet.
async fn maybe_print_proxy_table(fmt: OutputFormat) {
    if fmt == OutputFormat::Human {
        println!();
        println!(" {}", " Proxy Pool".cyan().bold());
        let proxy_table = print_proxy_table().await;
        println!("{}", proxy_table);
    }
}

/// Bridge status dashboard with uptime and proxy pool table.
async fn cmd_print_status(status: SupervisorStatus, fmt: OutputFormat) {
    match fmt {
        OutputFormat::Quiet => match status {
            SupervisorStatus::Running { pid, port, .. } => {
                println!("running pid={} port={}", pid, port);
            }
            SupervisorStatus::Stopped => {
                println!("stopped");
            }
        },
        OutputFormat::Json => {
            let info = ServerStatusInfo::from(Ok(status));
            if let Ok(s) = serde_json::to_string_pretty(&info) {
                println!("{s}");
            }
        }
        OutputFormat::Human => {
            println!();
            match status {
                SupervisorStatus::Running {
                    pid,
                    port,
                    started_at,
                } => {
                    let uptime = uptime_str(started_at);
                    let model = std::env::var("OPENCODE_MODEL").unwrap_or_else(|_| "auto".into());
                    let auth = if std::env::var("BRIDGE_AUTH_TOKEN")
                        .ok()
                        .filter(|t| !t.is_empty())
                        .is_some()
                    {
                        "enabled".green().bold().to_string()
                    } else {
                        "disabled".yellow().bold().to_string()
                    };

                    println!(
                        " {}            PID: {:<10} Uptime: {}",
                        "● Online".green().bold(),
                        pid.to_string().yellow().bold(),
                        uptime.cyan().bold()
                    );
                    println!(
                        "  Port: {:<14} Model: {}",
                        port.to_string().cyan().bold(),
                        model.blue().bold()
                    );
                    println!("  Auth: {}", auth);
                    maybe_print_proxy_table(fmt).await;
                }
                SupervisorStatus::Stopped => {
                    println!(" {}  Bridge is not running", "● Stopped".red().bold());
                }
            }
            println!();
        }
    }
}

#[derive(serde::Serialize)]
struct ServerStatusInfo {
    status: String,
    pid: Option<u32>,
    uptime: Option<String>,
    message: Option<String>,
}

impl From<Result<SupervisorStatus, String>> for ServerStatusInfo {
    fn from(result: Result<SupervisorStatus, String>) -> Self {
        match result {
            Ok(SupervisorStatus::Running {
                pid, started_at, ..
            }) => Self {
                status: "running".to_string(),
                pid: Some(pid),
                uptime: Some(uptime_str(started_at)),
                message: None,
            },
            Ok(SupervisorStatus::Stopped) => Self {
                status: "stopped".to_string(),
                pid: None,
                uptime: None,
                message: None,
            },
            Err(e) => Self {
                status: "error".to_string(),
                pid: None,
                uptime: None,
                message: Some(e),
            },
        }
    }
}

fn cmd_print_env() {
    let port = std::env::var("BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(config::DEFAULT_BRIDGE_PORT);
    let model = std::env::var("OPENCODE_MODEL").ok();

    println!();
    println!(" {}", "Environment Configuration".cyan().bold());
    println!("{}", "─".repeat(40).cyan().dim());
    println!(
        " {} = {}",
        "ANTHROPIC_API_KEY".bold(),
        "opencode-bridge".green().dim()
    );
    println!(
        " {} = {}",
        "ANTHROPIC_BASE_URL".bold(),
        format!("http://127.0.0.1:{}/v1", port).cyan().bold()
    );
    if let Some(m) = model {
        println!(" {} = {}", "OPENCODE_MODEL".bold(), m.yellow().bold());
    }
    println!();
    println!(" {}", "eval \"$(opencode2claude env)\"".green().bold());
    println!();
}

fn cmd_print_config() {
    let config = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
    println!();
    println!(" {}", "Server Configuration".cyan().bold());
    println!("{}", "─".repeat(40).cyan().dim());
    println!(
        " {} = {}",
        "BRIDGE_PORT".bold(),
        config.bridge_port.to_string().cyan().bold()
    );
    println!(
        " {} = {}",
        "BRIDGE_HOST".bold(),
        config.host.to_string().cyan().bold()
    );
    println!(
        " {} = {}",
        "BRIDGE_AUTH_TOKEN".bold(),
        if config.auth_enabled() {
            "enabled".green().bold()
        } else {
            "disabled".yellow().bold()
        }
    );
    println!(
        " {} = {}",
        "BRIDGE_SHELL_POLICY".bold(),
        config.shell_policy.description().cyan().bold()
    );
    println!(
        " {} = {}",
        "OPENCODE_MODEL".bold(),
        config
            .model
            .unwrap_or_else(|| "auto (claude-3-5-sonnet)".to_string())
            .yellow()
            .bold()
    );
    println!(
        " {} = {}",
        "MAX_BODY_SIZE".bold(),
        format!("{} bytes", config.max_body_size).cyan().bold()
    );
    println!();
}

fn show_logs(fmt: OutputFormat) {
    let paths = RuntimePaths::new();
    let log_path = paths.bridge_log();

    if !log_path.exists() {
        eprintln!(
            "{} No log file found. Start the daemon first: `opencode2claude server start`",
            "✗".red().bold()
        );
        std::process::exit(1);
    }

    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let tail = if lines.len() > 100 {
                &lines[lines.len() - 100..]
            } else {
                &lines
            };

            if fmt == OutputFormat::Json {
                #[derive(serde::Serialize)]
                struct LogEntry {
                    line: String,
                    line_number: usize,
                }
                let entries: Vec<LogEntry> = tail
                    .iter()
                    .enumerate()
                    .map(|(i, l)| LogEntry {
                        line: l.to_string(),
                        line_number: i + 1,
                    })
                    .collect();
                if let Ok(s) = serde_json::to_string_pretty(&entries) {
                    println!("{s}");
                }
                return;
            }

            for line in tail {
                let colored = if line.contains("ERROR") {
                    line.replace("ERROR", &"ERROR".red().bold().to_string())
                } else if line.contains("WARN") {
                    line.replace("WARN", &"WARN".yellow().bold().to_string())
                } else if line.contains("INFO") {
                    line.replace("INFO", &"INFO".cyan().bold().to_string())
                } else {
                    line.to_string()
                };
                println!("{}", colored);
            }
        }
        Err(e) => {
            eprintln!("{} log: {}", "✗".red().bold(), e);
            eprintln!("   Hint: Is the daemon running? Try `opencode2claude server start`");
            std::process::exit(1);
        }
    }
}

// ── Core server ──

async fn cmd_run_server(args: ServeArgsBridge) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let overrides = config::CliOverrides {
        bridge_port: args.port,
        host: args.host,
        model: args.model,
        shell_policy: args.shell_policy,
        config_path: args.config,
        tavily_api_key: args.tavily_api_key,
        exa_api_key: args.exa_api_key,
        serper_api_key: args.serper_api_key,
        searxng_url: args.searxng_url,
        searxng_api_key: args.searxng_api_key,
        tls_cert_path: args.tls_cert,
        tls_key_path: args.tls_key,
    };
    let config = BridgeConfig::from_env_and_cli(overrides);
    let addr = SocketAddr::from((config.host, config.bridge_port));

    if let Err(err) = config.validate_security() {
        eprintln!("{}", err);
        std::process::exit(1);
    }

    let max_body = config.max_body_size;

    info!("╔══════════════════════════════════════════════╗");
    info!(
        "║     OpenCode2Claude Bridge v{}          ║",
        env!("CARGO_PKG_VERSION")
    );
    info!("╠══════════════════════════════════════════════╣");
    info!(
        "║  Bridge:  http{}://{}{}║",
        if config.tls_enabled { "s" } else { "" },
        addr,
        " ".repeat(
            25usize.saturating_sub(addr.to_string().len() + if config.tls_enabled { 1 } else { 0 })
        )
    );
    info!(
        "║  Daemon:  port {}                          ║",
        config.opencode_port
    );
    info!(
        "║  Model:   {}{}║",
        config.model.as_deref().unwrap_or("(auto)"),
        " ".repeat(33usize.saturating_sub(config.model.as_deref().unwrap_or("(auto)").len()))
    );
    info!(
        "║  Shell:   {}{}║",
        config.shell_policy.description(),
        " ".repeat(33usize.saturating_sub(config.shell_policy.description().len()))
    );
    info!(
        "║  Auth:    {}{}║",
        if config.auth_enabled() {
            "enabled"
        } else {
            "disabled"
        },
        " ".repeat(
            33usize.saturating_sub(
                if config.auth_enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
                .len()
            )
        )
    );
    info!("╚══════════════════════════════════════════════╝");
    let scheme = if config.tls_enabled { "https" } else { "http" };
    info!(
        "To use: export ANTHROPIC_BASE_URL=\"{scheme}://{}/v1\"",
        addr
    );

    let state = AppState::new(config.clone());

    let app = Router::new()
        .route("/v1/messages", post(handlers::handle_messages))
        .route(
            "/v1/messages/count_tokens",
            post(handlers::handle_count_tokens),
        )
        .route("/v1/models", get(handlers::handle_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        .route("/health", get(handlers::handle_health))
        .layer(RequestBodyLimitLayer::new(max_body))
        .with_state(state);

    if config.tls_enabled {
        let tls_config = match RustlsConfig::from_pem_file(
            config.tls_cert_path.as_ref().unwrap(),
            config.tls_key_path.as_ref().unwrap(),
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{} Failed to load TLS config: {}", "✗".red().bold(), e);
                std::process::exit(1);
            }
        };

        info!("Server started successfully. Waiting for requests...");

        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await
            .unwrap_or_else(|e| {
                eprintln!("{} Server error: {}", "✗".red().bold(), e);
                std::process::exit(1);
            });
    } else {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("{} Failed to bind to {}: {}", "✗".red().bold(), addr, e);
                eprintln!(
                    "   Hint: Is another process using port {}? Try: lsof -i :{}",
                    addr.port(),
                    addr.port()
                );
                std::process::exit(1);
            }
        };

        info!("Server started successfully. Waiting for requests...");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .unwrap_or_else(|e| {
                eprintln!("{} Server error: {}", "✗".red().bold(), e);
                std::process::exit(1);
            });
    }

    info!("Server shut down gracefully.");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("Received SIGINT, shutting down..."); },
        _ = terminate => { info!("Received SIGTERM, shutting down..."); },
    }
}

// ── Runtime helpers ──

fn resolve_runtime(port: Option<u16>, host: Option<String>) -> Supervisor {
    let p = port
        .or_else(|| {
            std::env::var("BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(config::DEFAULT_BRIDGE_PORT);
    let h = host
        .or_else(|| std::env::var("BRIDGE_HOST").ok())
        .unwrap_or_else(|| config::DEFAULT_HOST.to_string());
    let paths = RuntimePaths::new();
    Supervisor::new(paths, p, h)
}
