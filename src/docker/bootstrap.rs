//! Interactive bootstrap of the local WARP proxy pool.

use super::health::verify_proxy;
use super::lifecycle::{ensure_container, is_docker_available, ContainerSetupState};
use super::types::{container_name, DockerResult};
use yansi::Paint;

pub async fn bootstrap_proxy_pool(quiet: bool) -> DockerResult<(String, String)> {
    if !is_docker_available().await {
        if !quiet {
            println!(
                "{} Docker is not available. Skipping proxy pool bootstrap.",
                "ℹ".cyan()
            );
        }
        return Ok((String::new(), String::new()));
    }

    if !quiet {
        println!(
            "{} Docker is running. Automating SOCKS5 proxy pool setup for multi-agent support...",
            "✓".green().bold()
        );
    }

    let primary_ports = [40001, 40002, 40003];
    let standby_ports = [40004, 40005];
    let all_ports = [&primary_ports[..], &standby_ports[..]].concat();

    let mut setup_handles = Vec::new();
    for &port in &all_ports {
        setup_handles.push(tokio::spawn(
            async move { (port, ensure_container(port).await) },
        ));
    }

    let mut setup_results = Vec::new();
    for handle in setup_handles {
        if let Ok(res) = handle.await {
            setup_results.push(res);
        }
    }

    let mut new_count = 0;
    let mut migrated_count = 0;
    let mut resumed_count = 0;
    let mut running_count = 0;

    for (port, res) in &setup_results {
        match res {
            Ok(state) => match state {
                ContainerSetupState::New => new_count += 1,
                ContainerSetupState::Migrated => migrated_count += 1,
                ContainerSetupState::Resumed => resumed_count += 1,
                ContainerSetupState::Running => running_count += 1,
            },
            Err(e) => {
                if !quiet {
                    eprintln!(
                        "{} Failed to setup container on port {}: {}",
                        "✗".red().bold(),
                        port,
                        e
                    );
                }
            }
        }
    }

    if !quiet {
        if running_count > 0 {
            println!(
                "  {} {} container(s) already running",
                "✓".green(),
                running_count
            );
        }
        if resumed_count > 0 {
            println!(
                "  {} Resumed {} stopped container(s) (WARP cached — instant start)",
                "✓".green(),
                resumed_count
            );
        }
        if migrated_count > 0 {
            println!(
                "  {} Migrated {} container(s) to volume-cached mode (one-time WARP registration)",
                "ℹ".yellow(),
                migrated_count
            );
        }
        if new_count > 0 {
            println!(
                "  {} Created {} new container(s) (WARP registration required)",
                "ℹ".yellow(),
                new_count
            );
        }
    }

    let needs_registration = new_count + migrated_count;
    if needs_registration > 0 {
        if !quiet {
            println!(
                "  {} Waiting 20 seconds for Cloudflare WARP registration ({} new/migrated)...",
                "ℹ".yellow(),
                needs_registration
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    } else if resumed_count > 0 && !quiet {
        println!("  Cached WARP config detected — skipping wait...");
    }

    // Helper closure to verify proxies in parallel
    let verify_all = |ports_to_verify: Vec<u16>,
                      max_attempts: usize,
                      sleep_secs: u64,
                      label: &'static str| async move {
        if !quiet {
            println!(
                "  {} Verifying {} proxy(ies) in parallel{}...",
                "::".blue(),
                ports_to_verify.len(),
                label
            );
        }

        let mut verify_handles = Vec::new();
        for port in ports_to_verify {
            verify_handles.push(tokio::spawn(async move {
                let mut ok = false;
                for _ in 0..max_attempts {
                    if verify_proxy(port).await {
                        ok = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
                }
                (port, ok)
            }));
        }

        let mut failed = Vec::new();
        for handle in verify_handles {
            if let Ok((port, ok)) = handle.await {
                let c_name = container_name(port);
                if ok {
                    if !quiet {
                        println!("  {} {} (port {}) — Online", "✓".green(), c_name, port);
                    }
                } else {
                    if !quiet {
                        println!("  {} {} (port {}) — Failed", "✗".red(), c_name, port);
                    }
                    failed.push(port);
                }
            }
        }
        failed
    };

    let failed_ports = verify_all(all_ports.clone(), 15, 2, "").await;

    let final_failed_ports = if !failed_ports.is_empty() {
        if !quiet {
            println!(
                "\n  {} Recovering {} failed proxy(ies) — restarting containers...",
                "ℹ".yellow(),
                failed_ports.len()
            );
        }

        let mut restart_handles = Vec::new();
        for &port in &failed_ports {
            let name = container_name(port);
            restart_handles.push(tokio::spawn(async move {
                let _ = tokio::process::Command::new("docker")
                    .args(["restart", &name])
                    .output()
                    .await;
            }));
        }
        for h in restart_handles {
            let _ = h.await;
        }

        if !quiet {
            println!("  Waiting 15 seconds for WARP reconnection...");
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        let retry_ports = failed_ports.clone();
        verify_all(retry_ports, 10, 3, " (retry)").await
    } else {
        failed_ports
    };

    if !quiet && final_failed_ports.is_empty() {
        println!("  {} All proxies verified and online!", "✓".green());
    } else if !quiet && !final_failed_ports.is_empty() {
        println!(
            "  {} {} proxy(ies) still offline. Bridge will route around them.",
            "⚠".yellow(),
            final_failed_ports.len()
        );
    }

    let primary_str = primary_ports
        .iter()
        .map(|p| format!("socks5://127.0.0.1:{}", p))
        .collect::<Vec<_>>()
        .join(",");
    let standby_str = standby_ports
        .iter()
        .map(|p| format!("socks5://127.0.0.1:{}", p))
        .collect::<Vec<_>>()
        .join(",");

    Ok((primary_str, standby_str))
}
