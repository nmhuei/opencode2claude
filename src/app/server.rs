//! Server lifecycle commands and daemon orchestration.

use super::view::{
    cmd_print_config, cmd_print_status, maybe_print_proxy_table, show_logs, ServerStatusInfo,
};
use crate::cli::{self, ServerCommand};
use crate::config::{self, BridgeConfig};
use crate::docker;
use crate::output::OutputFormat;
use crate::runtime::RuntimePaths;
use crate::server::{run_server, ServeArgsBridge};
use crate::supervisor::{Supervisor, SupervisorError, SupervisorStatus};
use indicatif::{ProgressBar, ProgressStyle};
use yansi::Paint;

pub(super) async fn cmd_server(cmd: ServerCommand, fmt: OutputFormat) {
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
                    max_body_size: args.max_body_size,
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
                    Ok(status) => {
                        let resolved =
                            BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                        cmd_print_status(status, fmt, &resolved).await
                    }
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

pub(super) async fn start_daemon(mut args: cli::ServerStartArgs, fmt: OutputFormat) {
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
            Err(
                SupervisorError::AlreadyRunning(_) | SupervisorError::AlreadyRunningUnmanaged(_),
            ) => {
                let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
                let info = ServerStatusInfo::from(Ok(status));
                if let Ok(s) = serde_json::to_string_pretty(&info) {
                    println!("{s}");
                }
            }
            Err(e) => {
                eprintln!("{} start: {}", "✗".red().bold(), e);
                eprintln!(
                    "   Hint: Check if the bridge is already running. Try: `oc2api server status`"
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
        Err(SupervisorError::AlreadyRunning(_) | SupervisorError::AlreadyRunningUnmanaged(_)) => {
            let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
            spinner.finish_with_message(format!(
                "{} Bridge already running. {}",
                "✓".green().bold(),
                status
            ));
            maybe_print_proxy_table(fmt).await;
        }
        Err(e) => {
            spinner.finish_and_clear();
            eprintln!("{} start: {}", "✗".red().bold(), e);
            eprintln!("   Hint: Check status with `oc2api server status` or stop with `oc2api server stop`");
            std::process::exit(1);
        }
    }
}

pub(super) async fn cmd_serve_legacy(args: cli::ServeArgs) {
    let bridge_args = ServeArgsBridge {
        port: args.port,
        host: args.host,
        config: args.config,
        model: args.model,
        shell_policy: args.shell_policy.map(|p| p.to_string()),
        max_body_size: args.max_body_size,
        tavily_api_key: args.tavily_api_key,
        exa_api_key: args.exa_api_key,
        serper_api_key: args.serper_api_key,
        searxng_url: args.searxng_url,
        searxng_api_key: args.searxng_api_key,
    };
    cmd_run_server(bridge_args).await;
}

pub(super) async fn cmd_start_legacy(args: cli::StartArgs, fmt: OutputFormat) {
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

pub(super) async fn cmd_status_legacy(args: cli::StatusArgs, fmt: OutputFormat) {
    let sup = resolve_runtime(args.port, args.host);
    if fmt == OutputFormat::Json {
        let status_info = ServerStatusInfo::from(sup.status().map_err(|e| e.to_string()));
        if let Ok(s) = serde_json::to_string_pretty(&status_info) {
            println!("{s}");
        }
    } else {
        match sup.status() {
            Ok(status) => {
                let resolved = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                cmd_print_status(status, fmt, &resolved).await
            }
            Err(e) => eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e),
        }
    }
}

pub(super) fn cmd_stop_legacy(args: cli::StopArgs) {
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

pub(super) async fn cmd_restart_legacy(fmt: OutputFormat) {
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

pub(super) fn cmd_logs_legacy(fmt: OutputFormat) {
    show_logs(fmt);
}

pub(super) async fn cmd_run_server(args: ServeArgsBridge) {
    let sup = resolve_runtime(args.port, args.host.clone());
    if let Ok(status @ SupervisorStatus::Running { .. }) = sup.status() {
        eprintln!(
            "{} Bridge is already running. {}",
            "✓".green().bold(),
            status
        );
        eprintln!("   Use `o2a server status`, `o2a server stop`, or `o2a server start -f --port <port>`.");
        return;
    }
    run_server(args).await;
}

// ── Runtime helpers ──

pub(super) fn resolve_runtime(port: Option<u16>, host: Option<String>) -> Supervisor {
    let resolved = BridgeConfig::from_env_and_cli(config::CliOverrides {
        bridge_port: port,
        host,
        ..Default::default()
    });
    let paths = RuntimePaths::from_config(&resolved);
    Supervisor::new(paths, resolved.bridge_port, resolved.host.to_string())
}

pub(super) fn resolve_runtime_for_start(args: &cli::ServerStartArgs) -> Supervisor {
    let resolved = BridgeConfig::from_env_and_cli(config::CliOverrides {
        bridge_port: args.port,
        host: args.host.clone(),
        config_path: args.config.clone(),
        model: args.model.clone(),
        shell_policy: args.shell_policy.map(|value| value.to_string()),
        max_body_size: args.max_body_size,
        tavily_api_key: args.tavily_api_key.clone(),
        exa_api_key: args.exa_api_key.clone(),
        serper_api_key: args.serper_api_key.clone(),
        searxng_url: args.searxng_url.clone(),
        searxng_api_key: args.searxng_api_key.clone(),
    });
    let p = resolved.bridge_port;
    let h = resolved.host.to_string();

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
    push_opt_arg_usize(&mut child_args, "--max-body-size", args.max_body_size);

    let paths = RuntimePaths::from_config(&resolved);
    Supervisor::new(paths, p, h).with_child_args(child_args)
}

fn push_opt_arg(argv: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

fn push_opt_arg_usize(argv: &mut Vec<String>, flag: &str, value: Option<usize>) {
    if let Some(value) = value {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

pub(super) async fn maybe_bootstrap_proxies(no_proxy: bool, quiet: bool) {
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
