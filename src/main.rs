//! OpenCode2Claude — A blazing-fast API bridge connecting Claude Code to any LLM.
//!
//! This binary provides a local HTTP server that translates Anthropic API requests
//! into OpenAI-compatible API calls forwarded to opencode.ai/zen/v1/chat/completions.

use opencode2api::cli::{
    self, Command, CompletionArgs, DashboardCommand, InitArgs, ProxyCommand, ServerCommand,
    ServerStartArgs, UpdateArgs,
};
use opencode2api::config::{self, BridgeConfig};
use opencode2api::docker;
use opencode2api::doctor;
use opencode2api::output::{setup_color, OutputFormat};
use opencode2api::proxy_pool;
use opencode2api::runtime::RuntimePaths;
use opencode2api::server::{run_server, ServeArgsBridge};
use opencode2api::supervisor::{Supervisor, SupervisorStatus};

use clap::CommandFactory;
use clap::Parser;
use clap_complete::generate;

use comfy_table::{presets, Cell as CtCell, Color as CtColor, ContentArrangement, Table};
use indicatif::{ProgressBar, ProgressStyle};
use yansi::Paint;

#[tokio::main]
async fn main() {
    // Load environment variables from .env file if present
    let _ = dotenvy::dotenv();

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

        // New dashboard subcommand group
        Some(Command::Dashboard(cmd)) => cmd_dashboard(cmd, fmt).await,

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
            cmd_stop_legacy(args)
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
                let quiet = fmt == OutputFormat::Quiet || fmt == OutputFormat::Json;
                maybe_bootstrap_proxies(args.no_proxy, quiet).await;
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
                };
                cmd_run_server(bridge_args).await;
            } else {
                start_daemon(args, fmt).await;
            }
        }
        ServerCommand::Stop(args) => {
            let sup = resolve_runtime(args.port, args.host);
            match sup.stop() {
                Ok(()) => {
                    println!("Bridge stopped.");
                    let quiet = fmt == OutputFormat::Quiet || fmt == OutputFormat::Json;
                    if let Err(e) = docker::stop_proxy_containers(args.purge).await {
                        if !quiet {
                            eprintln!(
                                "{} Failed to stop proxy containers: {}",
                                "✗".red().bold(),
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{} bridge: stop failed — {}", "✗".red().bold(), e);
                    eprintln!("   Hint: Is the bridge running? Try `oc2api server status`");
                    std::process::exit(1);
                }
            }
        }
        ServerCommand::Status(args) => {
            let sup = resolve_runtime(args.port, args.host);
            if fmt == OutputFormat::Json {
                let status_info = ServerStatusInfo::from(sup.status().map_err(|e| e.to_string()));
                if let Ok(s) = serde_json::to_string_pretty(&status_info) {
                    println!("{s}");
                }
            } else {
                match sup.status() {
                    Ok(status) => cmd_print_status(status, fmt).await,
                    Err(e) => {
                        if fmt == OutputFormat::Quiet {
                            println!("error");
                        } else {
                            eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e);
                        }
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
                    if fmt == OutputFormat::Json {
                        let info = ServerStatusInfo::from(Ok(status));
                        if let Ok(s) = serde_json::to_string_pretty(&info) {
                            println!("{s}");
                        }
                    } else {
                        println!("{} Bridge restarted. {}", "✓".green().bold(), status);
                        maybe_print_proxy_table(fmt).await;
                    }
                }
                Err(e) => {
                    eprintln!("{} restart: {}", "✗".red().bold(), e);
                    eprintln!("   Hint: Check the PID file or run `oc2api server start`");
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

async fn start_daemon(mut args: cli::ServerStartArgs, fmt: OutputFormat) {
    let quiet = fmt == OutputFormat::Quiet || fmt == OutputFormat::Json;
    if !args.no_proxy {
        maybe_bootstrap_proxies(false, quiet).await;
        args.no_proxy = true;
    }

    let sup = resolve_runtime_for_start(&args);

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
                eprintln!("{} start: {}", "✗".red().bold(), e);
                eprintln!(
                    "   Hint: Check if the bridge is already running. Try: `oc2api server stop`"
                );
                std::process::exit(1);
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
            spinner.finish_and_clear();
            eprintln!("{} start: {}", "✗".red().bold(), e);
            eprintln!("   Hint: Check if the bridge is already running. Try: `oc2api server stop`");
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
            let mut warnings = 0;
            let mut failures = 0;
            for c in &report.checks {
                match c.status {
                    doctor::CheckStatus::Warn => warnings += 1,
                    doctor::CheckStatus::Fail => failures += 1,
                    doctor::CheckStatus::Pass => {}
                }
            }
            println!("warnings={} failures={}", warnings, failures);
        }
        _ => {
            println!("{}", report);
        }
    }
    std::process::exit(report.summary.exit_code());
}

fn cmd_completion(args: CompletionArgs) {
    let mut cmd = cli::Cli::command();
    let name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, name, &mut std::io::stdout());
}

async fn cmd_update(args: UpdateArgs) {
    use opencode2api::update::{self, fetch_latest_release, find_matching_asset, has_update};

    let client = reqwest::Client::builder()
        .user_agent(concat!("opencode2api/", env!("CARGO_PKG_VERSION")))
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

            match update::apply_update(&client, asset).await {
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
    use opencode2api::init::generate_config;

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
                let mut primary_running = 0;
                let mut primary_total = 0;
                let mut standby_running = 0;
                let mut standby_total = 0;
                for (port, _, running) in &containers {
                    if primary_ports.contains(port) {
                        primary_total += 1;
                        if *running {
                            primary_running += 1;
                        }
                    } else if ws_ports.contains(port) {
                        standby_total += 1;
                        if *running {
                            standby_running += 1;
                        }
                    }
                }
                println!(
                    "primary={}/{} standby={}/{}",
                    primary_running, primary_total, standby_running, standby_total
                );
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
            if !yes {
                let ports = proxy_pool::get_primary_ports();
                eprintln!(
                    "{} About to purge and recreate {} primary proxies: {:?}",
                    "⚠".yellow().bold(),
                    ports.len(),
                    ports
                );
                eprintln!(
                    "{} This will reset all WARP connections.",
                    "⚠".yellow().bold()
                );
                eprint!("Continue? [y/N] ");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                let input = input.trim().to_lowercase();
                if input != "y" && input != "yes" {
                    println!("Aborted.");
                    return;
                }
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
    };
    cmd_run_server(bridge_args).await;
}

async fn cmd_start_legacy(args: cli::StartArgs, fmt: OutputFormat) {
    start_daemon(
        cli::ServerStartArgs {
            foreground: false,
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
    if fmt == OutputFormat::Json {
        let status_info = ServerStatusInfo::from(sup.status().map_err(|e| e.to_string()));
        if let Ok(s) = serde_json::to_string_pretty(&status_info) {
            println!("{s}");
        }
    } else {
        match sup.status() {
            Ok(status) => cmd_print_status(status, fmt).await,
            Err(e) => eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e),
        }
    }
}

fn cmd_stop_legacy(args: cli::StopArgs) {
    let sup = resolve_runtime(args.port, args.host);
    match sup.stop() {
        Ok(()) => println!("Bridge stopped."),
        Err(e) => {
            eprintln!("{} bridge: stop failed — {}", "✗".red().bold(), e);
            eprintln!("   Hint: Try `oc2api server status` to check if the bridge is running.");
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
            if fmt == OutputFormat::Json {
                let info = ServerStatusInfo::from(Ok(status));
                if let Ok(s) = serde_json::to_string_pretty(&info) {
                    println!("{s}");
                }
            } else {
                println!("{} Bridge restarted. {}", "✓".green().bold(), status);
                maybe_print_proxy_table(fmt).await;
            }
        }
        Err(e) => {
            eprintln!("{} restart: {}", "✗".red().bold(), e);
            eprintln!("   Hint: Check the PID file or run `oc2api server start`");
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
    if fmt == OutputFormat::Quiet {
        match status {
            SupervisorStatus::Running { .. } => println!("running"),
            SupervisorStatus::Stopped => println!("stopped"),
        }
        return;
    }
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

            // Bridge dashboard header
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
    println!(" {}", "eval \"$(oc2api env)\"".green().bold());
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
            "{} No log file found. Start the daemon first: `oc2api server start`",
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
            eprintln!("   Hint: Is the daemon running? Try `oc2api server start`");
            std::process::exit(1);
        }
    }
}

// ── Core server ──

async fn cmd_run_server(args: ServeArgsBridge) {
    run_server(args).await;
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

fn resolve_runtime_for_start(args: &cli::ServerStartArgs) -> Supervisor {
    let p = args
        .port
        .or_else(|| {
            std::env::var("BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(config::DEFAULT_BRIDGE_PORT);
    let h = args
        .host
        .clone()
        .or_else(|| std::env::var("BRIDGE_HOST").ok())
        .unwrap_or_else(|| config::DEFAULT_HOST.to_string());

    let mut child_args = vec![
        "--port".to_string(),
        p.to_string(),
        "--host".to_string(),
        h.clone(),
    ];
    push_opt_arg(&mut child_args, "--config", args.config.as_deref());
    push_opt_arg(&mut child_args, "--model", args.model.as_deref());
    let shell_policy_str = args.shell_policy.map(|p| p.to_string());
    push_opt_arg(
        &mut child_args,
        "--shell-policy",
        shell_policy_str.as_deref(),
    );
    push_opt_arg(
        &mut child_args,
        "--tavily-api-key",
        args.tavily_api_key.as_deref(),
    );
    push_opt_arg(
        &mut child_args,
        "--exa-api-key",
        args.exa_api_key.as_deref(),
    );
    push_opt_arg(
        &mut child_args,
        "--serper-api-key",
        args.serper_api_key.as_deref(),
    );
    push_opt_arg(
        &mut child_args,
        "--searxng-url",
        args.searxng_url.as_deref(),
    );
    push_opt_arg(
        &mut child_args,
        "--searxng-api-key",
        args.searxng_api_key.as_deref(),
    );

    let paths = RuntimePaths::new();
    Supervisor::new(paths, p, h).with_child_args(child_args)
}

fn push_opt_arg(argv: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

async fn maybe_bootstrap_proxies(no_proxy: bool, quiet: bool) {
    if no_proxy {
        return;
    }
    match docker::bootstrap_proxy_pool(quiet).await {
        Ok((primary, standby)) => {
            if !primary.is_empty() {
                std::env::set_var("BRIDGE_PRIMARY_PROXIES", primary);
            }
            if !standby.is_empty() {
                std::env::set_var("BRIDGE_WARM_STANDBY_PROXIES", standby);
            }
        }
        Err(e) => {
            if !quiet {
                eprintln!("{} Failed to bootstrap proxy pool: {}", "✗".red().bold(), e);
            }
        }
    }
}

async fn cmd_dashboard(cmd: DashboardCommand, fmt: OutputFormat) {
    // Load .env if present
    let _ = dotenvy::dotenv();

    let default_port = 4000;
    let default_host = std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));
    let supervisor = resolve_runtime(Some(default_port), Some(default_host.to_string()));

    let is_running = matches!(supervisor.status(), Ok(SupervisorStatus::Running { .. }));

    match cmd {
        DashboardCommand::Start => {
            if !is_running {
                if fmt == OutputFormat::Human {
                    println!(
                        "{} Bridge daemon is not running. Starting bridge daemon...",
                        "ℹ".blue()
                    );
                }
                start_daemon(ServerStartArgs::default(), fmt).await;
                // Wait briefly for startup bind
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            } else if fmt == OutputFormat::Human {
                println!("{} Bridge daemon is already running.", "✓".green());
            }

            let url = format!("http://127.0.0.1:{}/dashboard/", default_port);

            if fmt == OutputFormat::Human {
                println!("🚀 Opening dashboard in browser: {}", url.cyan().bold());

                let token = std::env::var("DASHBOARD_ADMIN_TOKEN").unwrap_or_default();
                if token.is_empty() {
                    println!(
                        "{} {} DASHBOARD_ADMIN_TOKEN is unset. The dashboard is in fail-closed mode and disabled!",
                        "⚠️".yellow().bold(),
                        "WARNING:".red().bold()
                    );
                    println!(
                        "   To fix: Set DASHBOARD_ADMIN_TOKEN in your environment or .env file."
                    );
                } else {
                    println!("🔑 Admin Token: {}", token.green().bold());
                }

                // Open browser
                let opened = if cfg!(target_os = "macos") {
                    std::process::Command::new("open")
                        .arg(&url)
                        .status()
                        .is_ok()
                } else if cfg!(target_os = "windows") {
                    std::process::Command::new("cmd")
                        .args(["/C", "start", &url])
                        .status()
                        .is_ok()
                } else {
                    std::process::Command::new("xdg-open")
                        .arg(&url)
                        .status()
                        .is_ok()
                };

                if !opened {
                    println!(
                        "   Failed to open browser automatically. Please open the URL manually."
                    );
                }
            } else if fmt == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "started",
                        "url": url,
                        "token": std::env::var("DASHBOARD_ADMIN_TOKEN").unwrap_or_default()
                    })
                );
            }
        }
        DashboardCommand::Status => {
            let url = format!("http://127.0.0.1:{}/dashboard/", default_port);
            let token = std::env::var("DASHBOARD_ADMIN_TOKEN").unwrap_or_default();

            if fmt == OutputFormat::Human {
                if is_running {
                    println!(
                        "{} Bridge Status: {}",
                        "✓".green(),
                        "RUNNING".green().bold()
                    );
                    println!("🔗 Dashboard URL: {}", url.cyan().bold());
                    if token.is_empty() {
                        println!(
                            "{} {} DASHBOARD_ADMIN_TOKEN is unset. The dashboard is in fail-closed mode and disabled!",
                            "⚠️".yellow().bold(),
                            "WARNING:".red().bold()
                        );
                    } else {
                        println!("🔑 Admin Token:  {}", token.green().bold());
                    }
                } else {
                    println!("{} Bridge Status: {}", "✗".red(), "STOPPED".red().bold());
                    println!("💡 Hint: Run `oc2api dashboard start` to launch the server and open the UI.");
                }
            } else if fmt == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({
                        "running": is_running,
                        "url": url,
                        "token": token
                    })
                );
            }
        }
    }
}
