//! Server lifecycle commands and daemon orchestration.

use super::view::{
    cmd_print_config, cmd_print_status, maybe_print_proxy_table, print_brand_header, print_error,
    print_start_summary, print_success, print_tip, show_logs, ServerStatusInfo,
};
use crate::cli::{self, ServerCommand};
use crate::config::{self, BridgeConfig};
use crate::docker;
use crate::output::{animations_enabled, OutputFormat};
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
                let resolved = resolve_config_for_start(&args);
                if startup_proxy_policy(&resolved, args.no_proxy)
                    == StartupProxyPolicy::BlockingBootstrap
                {
                    maybe_bootstrap_proxies(false, quiet).await;
                }
                // Run in foreground using bridge args
                let bridge_args = ServeArgsBridge {
                    port: args.port,
                    host: args.host,
                    config: args.config,
                    model: args.model,
                    shell_policy: args.shell_policy.map(|p| p.to_string()),
                    max_body_size: args.max_body_size,
                    tavily_api_key: None,
                    exa_api_key: None,
                    serper_api_key: None,
                    searxng_url: args.searxng_url,
                    searxng_api_key: None,
                    egress_mode: args.no_proxy.then(|| "direct".to_string()),
                    upstream_base_url: args.upstream_base_url,
                    upstream_api_key: None,
                };
                cmd_run_server(bridge_args).await;
            } else {
                start_daemon(args, fmt).await;
            }
        }
        ServerCommand::Stop(args) => {
            let resolved = resolve_config(args.port, args.host);
            let sup = supervisor_from_config(&resolved);
            let result = if args.unmanaged {
                sup.stop_adopting_unmanaged()
            } else {
                sup.stop()
            };
            let exit_code = stop_exit_code(&result);
            match &result {
                Ok(()) => {
                    let mut proxy_error = None;
                    if should_manage_proxy_containers(&resolved) {
                        if let Err(error) = docker::stop_proxy_containers(args.purge).await {
                            proxy_error = Some(error.to_string());
                        }
                    }

                    match fmt {
                        OutputFormat::Json => println!(
                            "{}",
                            serde_json::json!({
                                "status": "stopped",
                                "proxy_action": if args.purge { "purged" } else { "paused" },
                                "proxy_error": proxy_error,
                            })
                        ),
                        OutputFormat::Quiet => println!("stopped"),
                        OutputFormat::Human => {
                            print_brand_header("Gateway stop", "Service lifecycle");
                            print_success("Gateway stopped");
                            if let Some(error) = proxy_error {
                                println!();
                                super::view::print_warning(&format!(
                                    "Gateway stopped, but proxy cleanup failed: {error}"
                                ));
                            }
                            println!();
                        }
                    }
                }
                Err(error @ SupervisorError::UnmanagedListener { port, .. }) => {
                    let message = error.to_string();
                    match fmt {
                        OutputFormat::Json => println!(
                            "{}",
                            serde_json::json!({
                                "status": "refused",
                                "operation": "stop",
                                "port": port,
                                "message": message,
                            })
                        ),
                        OutputFormat::Quiet => eprintln!("refused: {message}"),
                        OutputFormat::Human => {
                            print_error(
                                "Refused to stop an untracked gateway",
                                &message,
                                &[
                                    "opencode2api server status",
                                    "opencode2api server stop --unmanaged",
                                ],
                            );
                        }
                    }
                }
                Err(error) => {
                    if fmt == OutputFormat::Json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "status": "error",
                                "operation": "stop",
                                "message": error.to_string(),
                            })
                        );
                    } else if fmt == OutputFormat::Quiet {
                        eprintln!("error");
                    } else {
                        print_error(
                            "Could not stop the gateway",
                            &error.to_string(),
                            &["opencode2api server status"],
                        );
                    }
                }
            }
            exit_with_status(exit_code);
        }
        ServerCommand::Status(args) => {
            let sup = resolve_runtime(args.port, args.host);
            if fmt == OutputFormat::Json {
                let status_result = sup.status().map_err(|e| e.to_string());
                let exit_code = status_result.as_ref().map_or(1, |s| s.exit_code());
                let status_info = status_info(status_result).await;
                if let Ok(s) = serde_json::to_string_pretty(&status_info) {
                    println!("{s}");
                }
                exit_with_status(exit_code);
            } else {
                match sup.status() {
                    Ok(status) => {
                        let exit_code = status.exit_code();
                        let resolved =
                            BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                        cmd_print_status(status, fmt, &resolved).await;
                        exit_with_status(exit_code);
                    }
                    Err(e) => {
                        if fmt == OutputFormat::Quiet {
                            println!("error");
                        } else {
                            eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e);
                        }
                        exit_with_status(1);
                    }
                }
            }
        }
        ServerCommand::Restart(args) => {
            let sup = resolve_runtime(None, None);
            // Honest-lifecycle contract: a stop-phase failure aborts the
            // restart BEFORE anything is started — never the old
            // swallow-then-confusing-start-failure behavior.
            if let Err(error) = restart_stop_phase(&sup, args.unmanaged) {
                let exit_code = match &error {
                    SupervisorError::UnmanagedListener { port, .. } => {
                        let message = error
                            .refusal_message("restart")
                            .unwrap_or_else(|| error.to_string());
                        match fmt {
                            OutputFormat::Json => println!(
                                "{}",
                                serde_json::json!({
                                    "status": "refused",
                                    "operation": "restart",
                                    "port": port,
                                    "message": message,
                                })
                            ),
                            OutputFormat::Quiet => eprintln!("refused: {message}"),
                            OutputFormat::Human => {
                                print_error(
                                    "Refused to restart over an untracked gateway",
                                    &message,
                                    &[
                                        "opencode2api server status",
                                        "opencode2api server restart --unmanaged",
                                    ],
                                );
                            }
                        }
                        crate::supervisor::STOP_REFUSED_UNMANAGED_EXIT_CODE
                    }
                    error => {
                        if fmt == OutputFormat::Json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "status": "error",
                                    "operation": "restart",
                                    "message": error.to_string(),
                                })
                            );
                        } else if fmt == OutputFormat::Quiet {
                            eprintln!("error");
                        } else {
                            print_error(
                                "Could not restart the gateway",
                                &error.to_string(),
                                &["opencode2api server status"],
                            );
                        }
                        1
                    }
                };
                exit_with_status(exit_code);
            }
            match sup.start() {
                Ok(()) => {
                    let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
                    match fmt {
                        OutputFormat::Json => {
                            let info = status_info(Ok(status)).await;
                            match serde_json::to_string_pretty(&info) {
                                Ok(json) => println!("{json}"),
                                Err(error) => eprintln!("{error}"),
                            }
                        }
                        OutputFormat::Quiet => println!("restarted"),
                        OutputFormat::Human => {
                            print_brand_header("Gateway restart", "Service lifecycle");
                            print_success("Gateway restarted");
                            print_tip(&status.to_string());
                            maybe_print_proxy_table(fmt).await;
                            println!();
                        }
                    }
                }
                Err(error) => {
                    if fmt == OutputFormat::Json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "status": "error",
                                "operation": "restart",
                                "message": error.to_string(),
                            })
                        );
                    } else if fmt == OutputFormat::Quiet {
                        eprintln!("error");
                    } else {
                        print_error(
                            "Could not restart the gateway",
                            &error.to_string(),
                            &["opencode2api server status", "opencode2api server start"],
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
                    dashboard_auth_configured: bool,
                    shell_policy: String,
                    model: String,
                    egress_mode: String,
                    upstream_base_url: String,
                    max_body_size: usize,
                    stream_buffer_size: usize,
                    channel_capacity: usize,
                    max_search_loops: u32,
                    search_chain_budget_secs: u64,
                    metrics_enabled: bool,
                    history_enabled: bool,
                    config_path: String,
                    runtime_dir: String,
                }
                let cfg = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                let paths = RuntimePaths::from_config(&cfg);
                let egress_mode = match cfg.egress.mode {
                    config::EgressMode::Direct => "direct",
                    config::EgressMode::Proxy => "proxy",
                    config::EgressMode::Hybrid => "hybrid",
                };
                let info = ConfigInfo {
                    bridge_port: cfg.bridge_port,
                    bridge_host: cfg.host.to_string(),
                    auth_enabled: cfg.auth_enabled(),
                    dashboard_auth_configured: cfg.management.dashboard_token().is_some(),
                    shell_policy: cfg.shell_policy.description().to_string(),
                    model: cfg.model.clone().unwrap_or_else(|| "auto".to_string()),
                    egress_mode: egress_mode.to_string(),
                    upstream_base_url: cfg.retry.upstream_base_url.clone(),
                    max_body_size: cfg.max_body_size,
                    stream_buffer_size: cfg.stream_buffer_size,
                    channel_capacity: cfg.channel_capacity,
                    max_search_loops: cfg.max_search_loops,
                    search_chain_budget_secs: cfg.search.chain_budget.as_secs(),
                    metrics_enabled: cfg.observability.metrics_enabled,
                    history_enabled: cfg.history.enabled,
                    config_path: cfg.management.config_path.display().to_string(),
                    runtime_dir: paths.runtime_dir().display().to_string(),
                };
                if let Ok(s) = serde_json::to_string_pretty(&info) {
                    println!("{s}");
                }
            } else if fmt == OutputFormat::Quiet {
                let cfg = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                println!("{}:{}", cfg.host, cfg.bridge_port);
            } else {
                cmd_print_config();
            }
        }
    }
}

pub(super) async fn start_daemon(args: cli::ServerStartArgs, fmt: OutputFormat) {
    let quiet = fmt != OutputFormat::Human;
    let initial = resolve_config_for_start(&args);
    if startup_proxy_policy(&initial, args.no_proxy) == StartupProxyPolicy::BlockingBootstrap {
        maybe_bootstrap_proxies(false, quiet).await;
    }

    // Re-resolve after strict proxy bootstrap because bootstrap may publish the
    // discovered proxy URLs into the environment for the child process.
    let sup = resolve_runtime_for_start(&args);

    if fmt == OutputFormat::Json {
        match sup.start() {
            Ok(())
            | Err(
                SupervisorError::AlreadyRunning(_) | SupervisorError::AlreadyRunningUnmanaged(_),
            ) => {
                let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
                let info = status_info(Ok(status)).await;
                match serde_json::to_string_pretty(&info) {
                    Ok(json) => println!("{json}"),
                    Err(error) => println!(
                        "{}",
                        serde_json::json!({"status":"error","message":error.to_string()})
                    ),
                }
            }
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "error",
                        "operation": "start",
                        "message": error.to_string(),
                    })
                );
                std::process::exit(1);
            }
        }
        return;
    }

    if fmt == OutputFormat::Human {
        print_brand_header("Starting gateway", "Service lifecycle");
    }

    let spinner = if fmt == OutputFormat::Human && animations_enabled() {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::with_template("  {spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        spinner.set_message("Starting gateway");
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        Some(spinner)
    } else {
        None
    };

    match sup.start() {
        Ok(()) => {
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
            if fmt == OutputFormat::Quiet {
                println!("running");
            } else {
                print_success("Gateway started");
                println!();
                let resolved = resolve_config_for_start(&args);
                print_start_summary(&status, &resolved).await;
                println!();
            }
        }
        Err(SupervisorError::AlreadyRunning(_) | SupervisorError::AlreadyRunningUnmanaged(_)) => {
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
            if fmt == OutputFormat::Quiet {
                println!("running");
            } else {
                print_success("Gateway is already running");
                println!();
                let resolved = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                print_start_summary(&status, &resolved).await;
                println!();
            }
        }
        Err(error) => {
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            if fmt == OutputFormat::Quiet {
                eprintln!("error");
            } else {
                print_error(
                    "Could not start the gateway",
                    &error.to_string(),
                    &["opencode2api server status", "opencode2api server stop"],
                );
            }
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
        tavily_api_key: None,
        exa_api_key: None,
        serper_api_key: None,
        searxng_url: args.searxng_url,
        searxng_api_key: None,
        egress_mode: None,
        upstream_base_url: None,
        upstream_api_key: None,
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
        let status_result = sup.status().map_err(|e| e.to_string());
        let exit_code = status_result.as_ref().map_or(1, |s| s.exit_code());
        let status_info = status_info(status_result).await;
        if let Ok(s) = serde_json::to_string_pretty(&status_info) {
            println!("{s}");
        }
        exit_with_status(exit_code);
    } else {
        match sup.status() {
            Ok(status) => {
                let exit_code = status.exit_code();
                let resolved = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
                cmd_print_status(status, fmt, &resolved).await;
                exit_with_status(exit_code);
            }
            Err(e) => {
                eprintln!("{} bridge: status failed — {}.", "✗".red().bold(), e);
                exit_with_status(1);
            }
        }
    }
}

pub(super) fn cmd_stop_legacy(args: cli::StopArgs) {
    let sup = resolve_runtime(args.port, args.host);
    match sup.stop() {
        Ok(()) => println!("Bridge stopped."),
        Err(e @ SupervisorError::UnmanagedListener { .. }) => {
            eprintln!("{} bridge: stop refused — {}", "✗".red().bold(), e);
            eprintln!(
                "   Hint: `oc2api server stop --unmanaged` verifies and adopts an untracked listener."
            );
            std::process::exit(crate::supervisor::STOP_REFUSED_UNMANAGED_EXIT_CODE);
        }
        Err(e) => {
            eprintln!("{} bridge: stop failed — {}", "✗".red().bold(), e);
            eprintln!("   Hint: Try `oc2api server status` to check if the bridge is running.");
            std::process::exit(1);
        }
    }
}

pub(super) async fn cmd_restart_legacy(fmt: OutputFormat) {
    let sup = resolve_runtime(None, None);
    // Same honest-lifecycle contract as `server restart`: never swallow a
    // stop-refusal and then fail confusingly inside start.
    if let Err(e) = restart_stop_phase(&sup, false) {
        match &e {
            SupervisorError::UnmanagedListener { .. } => {
                let message = e
                    .refusal_message("restart")
                    .unwrap_or_else(|| e.to_string());
                if fmt == OutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "refused",
                            "operation": "restart",
                            "message": message,
                        })
                    );
                } else if fmt == OutputFormat::Quiet {
                    eprintln!("refused: {message}");
                } else {
                    eprintln!("{} bridge: restart refused — {message}", "✗".red().bold());
                    eprintln!(
                        "   Hint: `oc2api server restart --unmanaged` verifies and adopts an untracked listener, then restarts over it."
                    );
                }
                exit_with_status(crate::supervisor::STOP_REFUSED_UNMANAGED_EXIT_CODE);
            }
            e => {
                if fmt == OutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "error",
                            "operation": "restart",
                            "message": e.to_string(),
                        })
                    );
                } else {
                    eprintln!(
                        "{} bridge: restart failed during its stop phase — {e}",
                        "✗".red().bold()
                    );
                    eprintln!("   Hint: Check the PID file or run `oc2api server status`");
                }
                exit_with_status(1);
            }
        }
    }
    match sup.start() {
        Ok(()) => {
            let status = sup.status().unwrap_or(SupervisorStatus::Stopped);
            if fmt == OutputFormat::Json {
                let info = status_info(Ok(status)).await;
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
        eprintln!(
            "   Use `o2a server status`, `o2a server stop`, or `o2a server start -f --port <port>`."
        );
        return;
    }
    if let Err(error) = run_server(args).await {
        eprintln!("{} {}", "✗".red().bold(), error);
        std::process::exit(1);
    }
}

// ── Runtime helpers ──

/// Stop phase shared by every restart entry point (`server restart`, legacy
/// `restart`). Honest-lifecycle contract for the whole restart flow:
///
/// - stop phase refused over an untracked listener (no `--unmanaged`): abort
///   BEFORE starting anything — one actionable message naming
///   `server restart --unmanaged`, exit code
///   [`STOP_REFUSED_UNMANAGED_EXIT_CODE`] (4), matching the stop contract;
/// - any other stop-phase failure: abort with exit code 1 rather than falling
///   through to `start`, which could only fail again with a confusing
///   secondary error;
/// - stop succeeded (including "was already stopped"): proceed to start.
fn restart_stop_phase(sup: &Supervisor, adopt_unmanaged: bool) -> Result<(), SupervisorError> {
    if adopt_unmanaged {
        sup.stop_adopting_unmanaged()
    } else {
        sup.stop()
    }
}

/// Flush pending stdout and terminate with the given status code so the
/// `server status` exit-code contract is observable by shell callers.
fn exit_with_status(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

/// Exit-code contract for `server stop`:
/// - `0`: stopped (or was already stopped — nothing answered the probe);
/// - `4`: refusal — an untracked listener answered on the configured port;
/// - `1`: any other failure while attempting to stop.
fn stop_exit_code(result: &Result<(), SupervisorError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(SupervisorError::UnmanagedListener { .. }) => {
            crate::supervisor::STOP_REFUSED_UNMANAGED_EXIT_CODE
        }
        Err(_) => 1,
    }
}

async fn status_info(result: Result<SupervisorStatus, String>) -> ServerStatusInfo {
    let port = match &result {
        Ok(SupervisorStatus::Running { port, .. }) => Some(*port),
        _ => None,
    };
    let config = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
    let mut info = ServerStatusInfo::from_status(result, &config);
    if let Some(port) = port {
        info.enrich_runtime(port).await;
    }
    info
}

pub(super) fn resolve_runtime(port: Option<u16>, host: Option<String>) -> Supervisor {
    let resolved = resolve_config(port, host);
    supervisor_from_config(&resolved)
}

fn resolve_config(port: Option<u16>, host: Option<String>) -> BridgeConfig {
    BridgeConfig::from_env_and_cli(config::CliOverrides {
        bridge_port: port,
        host,
        ..Default::default()
    })
}

fn supervisor_from_config(resolved: &BridgeConfig) -> Supervisor {
    let paths = RuntimePaths::from_config(resolved);
    Supervisor::new(paths, resolved.bridge_port, resolved.host.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupProxyPolicy {
    Skip,
    BlockingBootstrap,
    BackgroundReconcile,
}

fn startup_proxy_policy(config: &BridgeConfig, no_proxy: bool) -> StartupProxyPolicy {
    if no_proxy {
        return StartupProxyPolicy::Skip;
    }
    match config.egress.mode {
        config::EgressMode::Direct => StartupProxyPolicy::Skip,
        config::EgressMode::Proxy => StartupProxyPolicy::BlockingBootstrap,
        config::EgressMode::Hybrid => StartupProxyPolicy::BackgroundReconcile,
    }
}

fn should_manage_proxy_containers(resolved: &BridgeConfig) -> bool {
    resolved.egress.mode == config::EgressMode::Proxy
        && resolved
            .primary_proxies
            .as_ref()
            .is_some_and(|nodes| !nodes.is_empty())
}

fn resolve_config_for_start(args: &cli::ServerStartArgs) -> BridgeConfig {
    BridgeConfig::from_env_and_cli(config::CliOverrides {
        bridge_port: args.port,
        host: args.host.clone(),
        config_path: args.config.clone(),
        model: args.model.clone(),
        shell_policy: args.shell_policy.map(|value| value.to_string()),
        max_body_size: args.max_body_size,
        tavily_api_key: None,
        exa_api_key: None,
        serper_api_key: None,
        searxng_url: args.searxng_url.clone(),
        searxng_api_key: None,
        egress_mode: args.no_proxy.then(|| "direct".to_string()),
        upstream_base_url: args.upstream_base_url.clone(),
        upstream_api_key: None,
        clear_upstream_api_key: args.upstream_base_url.is_some(),
    })
}

fn daemon_child_launch(args: &cli::ServerStartArgs, port: u16, host: &str) -> Vec<String> {
    let mut child_args = vec![
        "--port".to_string(),
        port.to_string(),
        "--host".to_string(),
        host.to_string(),
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
        "--searxng-url",
        args.searxng_url.as_deref(),
    );
    push_opt_arg(
        &mut child_args,
        "--upstream-base-url",
        args.upstream_base_url.as_deref(),
    );
    push_opt_arg_usize(&mut child_args, "--max-body-size", args.max_body_size);
    if args.no_proxy {
        child_args.push("--egress-mode".to_string());
        child_args.push("direct".to_string());
    }
    child_args
}

pub(super) fn resolve_runtime_for_start(args: &cli::ServerStartArgs) -> Supervisor {
    let resolved = resolve_config_for_start(args);
    let p = resolved.bridge_port;
    let h = resolved.host.to_string();
    let child_args = daemon_child_launch(args, p, &h);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::process::{ProcessIdentity, ProcessManager, ProcessSpec};
    use crate::pidfile::PidFile;
    use std::net::TcpListener;

    fn temp_runtime_root(name: &str) -> std::path::PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut root = std::env::temp_dir();
        root.push(format!(
            "opencode2api-app-server-test-{}-{name}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&root);
        root
    }

    /// Grab a fresh ephemeral port and release it so probes against it are
    /// refused immediately (silent port).
    fn grab_free_port() -> u16 {
        TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A live listener owned by this very test process: enough to make
    /// supervisor port probes report "something answers" without spawning any
    /// child process or sending any signal anywhere.
    fn bind_local_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[test]
    fn restart_stop_phase_refuses_plain_over_untracked_listener() {
        let (_listener, port) = bind_local_listener();
        let root = temp_runtime_root("restart-refuse");
        let paths = RuntimePaths::from_root(&root);
        let sup = Supervisor::new(paths.clone(), port, "127.0.0.1");

        let error = restart_stop_phase(&sup, false)
            .expect_err("plain restart must refuse over an untracked listener");

        match &error {
            SupervisorError::UnmanagedListener {
                port: reported_port,
                ..
            } => assert_eq!(*reported_port, port),
            other => panic!("expected UnmanagedListener refusal, got: {other:?}"),
        }
        // Refusal must happen BEFORE anything is started or adopted.
        assert!(!paths.pid_file().exists());

        drop(_listener);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restart_stop_phase_override_engages_adoption_instead_of_refusal() {
        let (_listener, port) = bind_local_listener();
        let root = temp_runtime_root("restart-adopt-route");
        let paths = RuntimePaths::from_root(&root);
        let sup = Supervisor::new(paths.clone(), port, "127.0.0.1");

        // The only listener is this test process itself; adoption skips
        // self-adoption by design, so the override surfaces an AdoptionFailed
        // — proving the flag routed through the adoption path rather than the
        // plain-refusal path, with no signal ever sent anywhere.
        let error = restart_stop_phase(&sup, true)
            .expect_err("self-owned listener cannot be adopted into supervisor state");

        match &error {
            SupervisorError::AdoptionFailed { port: failed, .. } => assert_eq!(*failed, port),
            other => panic!(
                "expected AdoptionFailed from the --unmanaged adoption route, got: {other:?}"
            ),
        }
        assert!(!paths.pid_file().exists());

        drop(_listener);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restart_stop_phase_proceeds_when_port_is_silent() {
        let port = grab_free_port();
        let root = temp_runtime_root("restart-silent");
        let sup = Supervisor::new(RuntimePaths::from_root(&root), port, "127.0.0.1");

        restart_stop_phase(&sup, false).expect("plain restart proceeds over a silent port");
        restart_stop_phase(&sup, true)
            .expect("--unmanaged restart also proceeds over a silent port");

        let _ = std::fs::remove_dir_all(root);
    }

    /// Minimal fake: the recorded identity stays alive until `terminate`
    /// clears it, so the supervisor's post-TERM wait loop exits immediately
    /// and the destructive KILL batch must never fire.
    #[derive(Debug)]
    struct TerminatesOnSignalManager {
        identity: std::sync::Mutex<Option<ProcessIdentity>>,
        terminate_calls: std::sync::Mutex<Vec<u32>>,
        force_calls: std::sync::Mutex<Vec<u32>>,
    }

    impl ProcessManager for TerminatesOnSignalManager {
        fn spawn_detached(&self, _spec: &ProcessSpec) -> std::io::Result<ProcessIdentity> {
            Err(std::io::Error::other("not used"))
        }

        fn identity(&self, _pid: u32) -> std::io::Result<Option<ProcessIdentity>> {
            Ok(self.identity.lock().unwrap().clone())
        }

        fn terminate(&self, pid: u32) -> std::io::Result<()> {
            self.terminate_calls.lock().unwrap().push(pid);
            *self.identity.lock().unwrap() = None;
            Ok(())
        }

        fn force_kill(&self, pid: u32) -> std::io::Result<()> {
            self.force_calls.lock().unwrap().push(pid);
            Ok(())
        }
    }

    #[test]
    fn restart_stop_phase_stops_managed_process_via_verified_flow() {
        let port = grab_free_port();
        let root = temp_runtime_root("restart-managed-stop");
        let paths = RuntimePaths::from_root(&root);
        paths.ensure_dirs().unwrap();
        let identity = ProcessIdentity {
            pid: 4747,
            executable: Some(std::env::current_exe().unwrap()),
            start_marker: Some("restart-managed".to_string()),
        };
        PidFile::with_identity(identity.clone(), port, "127.0.0.1")
            .write(&paths.pid_file())
            .unwrap();

        let manager = std::sync::Arc::new(TerminatesOnSignalManager {
            identity: std::sync::Mutex::new(Some(identity)),
            terminate_calls: std::sync::Mutex::new(Vec::new()),
            force_calls: std::sync::Mutex::new(Vec::new()),
        });
        let sup =
            Supervisor::new(paths.clone(), port, "127.0.0.1").with_process_manager(manager.clone());

        restart_stop_phase(&sup, false).expect("managed stop must succeed before start phase");

        assert_eq!(*manager.terminate_calls.lock().unwrap(), vec![4747]);
        assert!(
            manager.force_calls.lock().unwrap().is_empty(),
            "a clean TERM must never escalate to KILL"
        );
        assert!(!paths.pid_file().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refusal_message_renders_requested_operation_from_one_source() {
        let error = SupervisorError::UnmanagedListener {
            port: 4000,
            detail: "a TCP listener accepted a connection on probed port 4000".to_string(),
        };
        // Restart flows must name the restart override, never the stop flag.
        let restart_message = error
            .refusal_message("restart")
            .expect("refusal message for this variant");
        assert!(restart_message.contains("server restart --unmanaged"));
        assert!(
            !restart_message.contains("stop --unmanaged"),
            "restart guidance must not send users to the stop flag: {restart_message}"
        );
        assert!(restart_message.contains("refusing to restart"));
        // The stop flow renders its own flag from the same single template.
        let stop_message = error
            .refusal_message("stop")
            .expect("refusal message for this variant");
        assert!(stop_message.contains("refusing to stop"));
        assert!(stop_message.contains("server stop --unmanaged"));
        assert_eq!(
            error.to_string(),
            stop_message,
            "Display must stay the stop-flavored rendering of the shared template"
        );
        assert_eq!(
            SupervisorError::NotRunning.refusal_message("restart"),
            None,
            "only the unmanaged-listener refusal has dedicated wording"
        );
    }

    #[test]
    fn stop_exit_code_contract_distinguishes_stopped_refused_failed() {
        let stopped: Result<(), SupervisorError> = Ok(());
        assert_eq!(stop_exit_code(&stopped), 0);

        let refused = Err(SupervisorError::UnmanagedListener {
            port: 4000,
            detail: "a TCP listener accepted a connection on probed port 4000".to_string(),
        });
        assert_eq!(stop_exit_code(&refused), 4);

        let failed = Err(SupervisorError::OwnershipMismatch(4242));
        assert_eq!(stop_exit_code(&failed), 1);
        assert_eq!(
            stop_exit_code(&Err(SupervisorError::NotRunning)),
            1,
            "generic failures stay at exit code 1"
        );
    }

    #[test]
    fn hybrid_startup_never_blocks_on_bootstrap() {
        let mut config = BridgeConfig::default();
        config.egress.mode = config::EgressMode::Hybrid;
        assert_eq!(
            startup_proxy_policy(&config, false),
            StartupProxyPolicy::BackgroundReconcile
        );
    }

    #[test]
    fn startup_proxy_policy_preserves_direct_proxy_and_no_proxy_semantics() {
        let mut config = BridgeConfig::default();
        config.egress.mode = config::EgressMode::Direct;
        assert_eq!(
            startup_proxy_policy(&config, false),
            StartupProxyPolicy::Skip
        );

        config.egress.mode = config::EgressMode::Proxy;
        assert_eq!(
            startup_proxy_policy(&config, false),
            StartupProxyPolicy::BlockingBootstrap
        );
        assert_eq!(
            startup_proxy_policy(&config, true),
            StartupProxyPolicy::Skip
        );
    }

    #[test]
    fn direct_mode_never_manages_proxy_containers() {
        let config = BridgeConfig {
            egress: config::EgressConfig {
                mode: config::EgressMode::Direct,
                ..BridgeConfig::default().egress
            },
            primary_proxies: Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
            ..Default::default()
        };
        assert!(!should_manage_proxy_containers(&config));
    }

    #[test]
    fn proxy_mode_manages_only_nonempty_primary_pool() {
        let mut config = BridgeConfig::default();
        config.egress.mode = config::EgressMode::Proxy;
        config.primary_proxies = None;
        assert!(!should_manage_proxy_containers(&config));
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        assert!(should_manage_proxy_containers(&config));
    }
}
