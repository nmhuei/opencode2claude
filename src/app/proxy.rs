//! Proxy container operations exposed by the CLI.

use super::view::{
    print_brand_header, print_error, print_proxy_table_for_ports, print_section, print_success,
    print_table, print_tip, print_warning,
};
use crate::cli::ProxyCommand;
use crate::docker;
use crate::output::{animations_enabled, OutputFormat};
use crate::presentation;
use crate::proxy_pool;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::io::Write;
use yansi::Paint;

#[derive(Debug, Serialize)]
struct ProxyOperationResult {
    port: u16,
    action: String,
    status: String,
    message: Option<String>,
}

pub(super) async fn cmd_proxy(cmd: ProxyCommand, fmt: OutputFormat) {
    let resolved =
        crate::config::BridgeConfig::from_env_and_cli(crate::config::CliOverrides::default());
    let primary_ports = proxy_pool::configured_primary_ports(&resolved);
    let standby_ports = proxy_pool::configured_warm_standby_ports(&resolved);

    match cmd {
        ProxyCommand::Ps | ProxyCommand::Status => {
            show_proxy_status(fmt, &primary_ports, &standby_ports).await;
        }
        ProxyCommand::Restart { dry_run } => {
            if dry_run {
                show_plan(fmt, "restart", &primary_ports);
                return;
            }

            if fmt == OutputFormat::Human {
                print_brand_header("Proxy restart", "Managed primary pool");
            }
            let spinner = operation_spinner(fmt, "Restarting primary proxies");
            let mut results = Vec::new();
            for port in &primary_ports {
                if let Some(spinner) = &spinner {
                    spinner.set_message(format!("Restarting proxy {port}"));
                }
                let result = match docker::create_container(*port).await {
                    Ok(()) => ProxyOperationResult {
                        port: *port,
                        action: "restart".into(),
                        status: "ok".into(),
                        message: None,
                    },
                    Err(docker::DockerError::Protected(message)) => ProxyOperationResult {
                        port: *port,
                        action: "restart".into(),
                        status: "skipped".into(),
                        message: Some(message),
                    },
                    Err(error) => ProxyOperationResult {
                        port: *port,
                        action: "restart".into(),
                        status: "error".into(),
                        message: Some(error.to_string()),
                    },
                };
                results.push(result);
            }
            finish_spinner(spinner);
            render_operation_results(fmt, "Proxy restart complete", &results);
        }
        ProxyCommand::Purge { yes, dry_run } => {
            if dry_run {
                show_plan(fmt, "purge and recreate", &primary_ports);
                return;
            }

            if fmt == OutputFormat::Human && !yes && !confirm_purge(&primary_ports) {
                println!("Aborted.");
                return;
            }

            if fmt == OutputFormat::Human {
                print_brand_header("Proxy purge", "Recreate managed primary pool");
            }
            let spinner = operation_spinner(fmt, "Purging primary proxies");
            let mut results = Vec::new();

            for port in &primary_ports {
                if let Some(spinner) = &spinner {
                    spinner.set_message(format!("Rotating proxy identity {port}"));
                }
                let result = match docker::rotate_container(*port).await {
                    Ok(()) => ProxyOperationResult {
                        port: *port,
                        action: "rotate".into(),
                        status: "ok".into(),
                        message: None,
                    },
                    Err(docker::DockerError::Protected(message)) => ProxyOperationResult {
                        port: *port,
                        action: "rotate".into(),
                        status: "skipped".into(),
                        message: Some(message),
                    },
                    Err(error) => ProxyOperationResult {
                        port: *port,
                        action: "rotate".into(),
                        status: "error".into(),
                        message: Some(error.to_string()),
                    },
                };
                results.push(result);
            }

            finish_spinner(spinner);
            render_operation_results(fmt, "Proxy purge complete", &results);
        }
        ProxyCommand::Logs => show_proxy_logs(fmt, &primary_ports).await,
    }
}

async fn show_proxy_status(fmt: OutputFormat, primary_ports: &[u16], standby_ports: &[u16]) {
    let all_ports: Vec<u16> = primary_ports
        .iter()
        .chain(standby_ports.iter())
        .copied()
        .collect();
    let containers = docker::list_containers(&all_ports).await;

    match fmt {
        OutputFormat::Json => {
            let nodes = containers
                .iter()
                .map(|(port, name, running)| {
                    serde_json::json!({
                        "port": port,
                        "name": name,
                        "role": if primary_ports.contains(port) { "primary" } else { "standby" },
                        "status": if *running { "running" } else { "offline" },
                        "running": running,
                    })
                })
                .collect::<Vec<_>>();
            match serde_json::to_string_pretty(&nodes) {
                Ok(json) => println!("{json}"),
                Err(error) => println!(
                    "{}",
                    serde_json::json!({"status":"error","message":error.to_string()})
                ),
            }
        }
        OutputFormat::Quiet => {
            let primary_running = containers
                .iter()
                .filter(|(port, _, running)| primary_ports.contains(port) && *running)
                .count();
            let standby_running = containers
                .iter()
                .filter(|(port, _, running)| standby_ports.contains(port) && *running)
                .count();
            println!(
                "primary={}/{} standby={}/{}",
                primary_running,
                primary_ports.len(),
                standby_running,
                standby_ports.len()
            );
        }
        OutputFormat::Human => {
            print_brand_header("Proxy pool", "Managed primary and protected standby nodes");
            let table = print_proxy_table_for_ports(primary_ports, standby_ports).await;
            print_table(&table);

            let running = containers.iter().filter(|(_, _, running)| *running).count();
            let offline = containers.len().saturating_sub(running);
            println!(
                "{}",
                presentation::summary(&[
                    format!("{} running", running.to_string().green()),
                    format!("{} offline", offline.to_string().dim()),
                    format!("{} primary", primary_ports.len()),
                    format!("{} standby", standby_ports.len()),
                ])
            );
            println!();
            print_tip("Standby proxies are protected and never modified by restart or purge.");
            println!();
        }
    }
}

fn show_plan(fmt: OutputFormat, action: &str, ports: &[u16]) {
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "dry_run": true,
                "action": action,
                "ports": ports,
            })
        ),
        OutputFormat::Quiet => println!(
            "dry-run action={} ports={}",
            action.replace(' ', "-"),
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        OutputFormat::Human => {
            print_brand_header("Proxy operation plan", "Dry run; no containers will change");
            println!(
                "{}",
                presentation::facts(&[
                    ("Action", action.to_string()),
                    (
                        "Ports",
                        ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                ])
            );
            println!();
            print_tip("Protected standby proxies are excluded.");
            println!();
        }
    }
}

fn operation_spinner(fmt: OutputFormat, message: &str) -> Option<ProgressBar> {
    if fmt != OutputFormat::Human || !animations_enabled() {
        return None;
    }
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    spinner.set_message(message.to_string());
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    Some(spinner)
}

fn finish_spinner(spinner: Option<ProgressBar>) {
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
}

fn render_operation_results(
    fmt: OutputFormat,
    success_title: &str,
    results: &[ProxyOperationResult],
) {
    let failed = results
        .iter()
        .filter(|result| result.status == "error")
        .count();
    match fmt {
        OutputFormat::Json => match serde_json::to_string_pretty(results) {
            Ok(json) => println!("{json}"),
            Err(error) => println!(
                "{}",
                serde_json::json!({"status":"error","message":error.to_string()})
            ),
        },
        OutputFormat::Quiet => println!("ok={} failed={}", results.len() - failed, failed),
        OutputFormat::Human => {
            if failed == 0 {
                print_success(success_title);
            } else {
                print_warning(&format!("Completed with {failed} failed operation(s)"));
            }
            println!();
            for result in results {
                let label = format!("{} {}", result.port, result.action);
                match result.status.as_str() {
                    "ok" => println!(
                        "{}{}  {}",
                        " ".repeat(presentation::INDENT),
                        "✓".green(),
                        label
                    ),
                    "skipped" => println!(
                        "{}{}  {}{}",
                        " ".repeat(presentation::INDENT),
                        "○".dim(),
                        label,
                        result
                            .message
                            .as_ref()
                            .map(|message| format!(" · {message}"))
                            .unwrap_or_default()
                            .dim()
                    ),
                    _ => println!(
                        "{}{}  {}{}",
                        " ".repeat(presentation::INDENT),
                        "×".red(),
                        label,
                        result
                            .message
                            .as_ref()
                            .map(|message| format!(" · {message}"))
                            .unwrap_or_default()
                            .red()
                    ),
                }
            }
            println!();
            print_tip("Protected standby proxies were not modified.");
            println!();
        }
    }
}

fn confirm_purge(ports: &[u16]) -> bool {
    print_warning(&format!(
        "This will recreate primary proxies: {}",
        ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    print!("  Continue? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

async fn show_proxy_logs(fmt: OutputFormat, ports: &[u16]) {
    let mut records = Vec::new();
    let mut failures = Vec::new();
    for port in ports {
        match docker::container_logs(*port, 50).await {
            Ok(logs) => records.push((
                *port,
                docker::container_name(*port),
                crate::tui::strip_ansi(&logs),
            )),
            Err(error) => failures.push((*port, error.to_string())),
        }
    }

    match fmt {
        OutputFormat::Json => {
            let logs = records
                .iter()
                .map(|(port, name, logs)| serde_json::json!({"port":port,"name":name,"logs":logs}))
                .collect::<Vec<_>>();
            println!("{}", serde_json::json!({"logs":logs,"errors":failures}));
        }
        OutputFormat::Quiet => {
            for (_, _, logs) in records {
                println!("{logs}");
            }
            for (port, error) in failures {
                eprintln!("{port}: {error}");
            }
        }
        OutputFormat::Human => {
            print_brand_header("Proxy logs", "Last 50 lines per primary container");
            for (port, name, logs) in records {
                print_section(&format!("{name} · {port}"));
                println!("{logs}");
            }
            for (port, error) in failures {
                print_error(
                    &format!("Could not read proxy {port} logs"),
                    &error,
                    &["opencode2api proxy ps"],
                );
            }
        }
    }
}
