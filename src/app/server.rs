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
                    tavily_api_key: args.tavily_api_key,
                    exa_api_key: args.exa_api_key,
                    serper_api_key: args.serper_api_key,
                    searxng_url: args.searxng_url,
                    searxng_api_key: args.searxng_api_key,
                    egress_mode: args.no_proxy.then(|| "direct".to_string()),
                };
                cmd_run_server(bridge_args).await;
            } else {
                start_daemon(args, fmt).await;
            }
        }
        ServerCommand::Stop(args) => {
            let resolved = resolve_config(args.port, args.host);
            let sup = supervisor_from_config(&resolved);
            match sup.stop() {
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
                    std::process::exit(1);
                }
            }
        }
        ServerCommand::Status(args) => {
            let sup = resolve_runtime(args.port, args.host);
            if fmt == OutputFormat::Json {
                let status_info = status_info(sup.status().map_err(|e| e.to_string())).await;
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
        tavily_api_key: args.tavily_api_key,
        exa_api_key: args.exa_api_key,
        serper_api_key: args.serper_api_key,
        searxng_url: args.searxng_url,
        searxng_api_key: args.searxng_api_key,
        egress_mode: None,
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
        let status_info = status_info(sup.status().map_err(|e| e.to_string())).await;
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
        eprintln!("   Use `o2a server status`, `o2a server stop`, or `o2a server start -f --port <port>`.");
        return;
    }
    if let Err(error) = run_server(args).await {
        eprintln!("{} {}", "✗".red().bold(), error);
        std::process::exit(1);
    }
}

// ── Runtime helpers ──

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
        tavily_api_key: args.tavily_api_key.clone(),
        exa_api_key: args.exa_api_key.clone(),
        serper_api_key: args.serper_api_key.clone(),
        searxng_url: args.searxng_url.clone(),
        searxng_api_key: args.searxng_api_key.clone(),
        egress_mode: args.no_proxy.then(|| "direct".to_string()),
    })
}

pub(super) fn resolve_runtime_for_start(args: &cli::ServerStartArgs) -> Supervisor {
    let resolved = resolve_config_for_start(args);
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
    if args.no_proxy {
        child_args.push("--egress-mode".to_string());
        child_args.push("direct".to_string());
    }

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
