//! Dashboard CLI commands. The HTTP dashboard implementation remains in
//! `crate::dashboard`; this module only manages command-line lifecycle.

use super::server::{resolve_runtime, start_daemon};
use super::view::{key_value_table, masked_configured_label, print_brand_header, print_tip};
use crate::cli::{DashboardCommand, ServerStartArgs};
use crate::config;
use crate::output::OutputFormat;
use crate::supervisor::SupervisorStatus;
use yansi::Paint;

pub(super) async fn cmd_dashboard(cmd: DashboardCommand, fmt: OutputFormat) {
    // Load .env if present
    let _ = dotenvy::dotenv();

    let supervisor = resolve_runtime(None, None);
    let status = supervisor.status().unwrap_or(SupervisorStatus::Stopped);
    let default_port = match &status {
        SupervisorStatus::Running { port, .. } => *port,
        SupervisorStatus::Stopped => std::env::var("BRIDGE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(config::DEFAULT_BRIDGE_PORT),
    };

    let is_running = status.is_running();

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
                let token = std::env::var("DASHBOARD_ADMIN_TOKEN").unwrap_or_default();
                print_brand_header("Dashboard", "admin control plane");
                let table = key_value_table(
                    ("Item", "Value"),
                    vec![
                        ("Status", "ready".green().bold().to_string()),
                        ("URL", url.cyan().bold().to_string()),
                        ("Admin auth", masked_configured_label(&token)),
                    ],
                );
                println!("{}", table);
                if token.is_empty() {
                    print_tip("Set DASHBOARD_ADMIN_TOKEN before exposing or using the dashboard.");
                }

                print_tip("Open the Dashboard URL manually when you need the UI.");
            } else if fmt == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ready",
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
                print_brand_header("Dashboard", "admin control plane");
                if is_running {
                    let table = key_value_table(
                        ("Item", "Value"),
                        vec![
                            ("Bridge", "running".green().bold().to_string()),
                            ("URL", url.cyan().bold().to_string()),
                            ("Admin auth", masked_configured_label(&token)),
                        ],
                    );
                    println!("{}", table);
                    if token.is_empty() {
                        print_tip(
                            "Dashboard is fail-closed until DASHBOARD_ADMIN_TOKEN is configured.",
                        );
                    }
                } else {
                    let table = key_value_table(
                        ("Item", "Value"),
                        vec![
                            ("Bridge", "stopped".red().bold().to_string()),
                            ("Next step", "opencode2api dashboard start".to_string()),
                        ],
                    );
                    println!("{}", table);
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
