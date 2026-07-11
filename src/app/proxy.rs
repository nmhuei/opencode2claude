//! Proxy container operations exposed by the CLI.

use super::view::print_proxy_table;
use crate::cli::ProxyCommand;
use crate::docker;
use crate::output::OutputFormat;
use crate::proxy_pool;
use indicatif::{ProgressBar, ProgressStyle};
use yansi::Paint;

pub(super) async fn cmd_proxy(cmd: ProxyCommand, fmt: OutputFormat) {
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
        ProxyCommand::Restart { dry_run } => {
            if dry_run {
                let ports = proxy_pool::get_primary_ports();
                if fmt == OutputFormat::Json {
                    let planned = ports
                        .iter()
                        .map(|port| {
                            serde_json::json!({
                                "port": port,
                                "action": "restart",
                                "dry_run": true,
                            })
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&planned).unwrap_or_default()
                    );
                } else if fmt == OutputFormat::Quiet {
                    println!("dry-run restart ports=40001,40002,40003");
                } else {
                    println!("Dry run: would restart managed primary proxies {:?}", ports);
                    println!("Warm-standby proxies 40004 and 40005 remain protected.");
                }
                return;
            }

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
        ProxyCommand::Purge { yes, dry_run } => {
            if dry_run {
                let ports = proxy_pool::get_primary_ports();
                if fmt == OutputFormat::Json {
                    let planned = ports
                        .iter()
                        .flat_map(|port| {
                            [
                                serde_json::json!({"port":port,"action":"remove","dry_run":true}),
                                serde_json::json!({"port":port,"action":"create","dry_run":true}),
                            ]
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&planned).unwrap_or_default()
                    );
                } else if fmt == OutputFormat::Quiet {
                    println!("dry-run purge ports=40001,40002,40003");
                } else {
                    println!(
                        "Dry run: would remove and recreate managed primary proxies {:?}",
                        ports
                    );
                    println!("Warm-standby proxies 40004 and 40005 remain protected.");
                }
                return;
            }

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
