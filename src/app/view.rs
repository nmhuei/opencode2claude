//! CLI presentation helpers.
//!
//! Rendering is isolated from command orchestration so operational logic can be
//! tested without parsing terminal output.

use crate::config::{self, BridgeConfig};
use crate::docker;
use crate::output::OutputFormat;
use crate::proxy_pool;
use crate::runtime::RuntimePaths;
use crate::supervisor::SupervisorStatus;
use comfy_table::{
    modifiers, presets, Cell as CtCell, Color as CtColor, ContentArrangement, Table,
};
use yansi::Paint;

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

pub(super) fn print_brand_header(title: &str, subtitle: &str) {
    println!();
    println!(
        "{}",
        "╭────────────────────────────────────────────────────────────╮"
            .cyan()
            .bold()
    );
    println!(
        "{} {} {} {}",
        "│".cyan().bold(),
        title.bold(),
        subtitle.dim(),
        "│".cyan().bold()
    );
    println!(
        "{}",
        "╰────────────────────────────────────────────────────────────╯"
            .cyan()
            .bold()
    );
}

pub(super) fn print_section(title: &str) {
    println!();
    println!("{} {}", "◆".cyan().bold(), title.bold());
}

pub(super) fn print_tip(message: &str) {
    println!("{} {}", "➜".cyan().bold(), message.dim());
}

fn status_cell(label: &str, color: CtColor) -> CtCell {
    CtCell::new(label).fg(color)
}

pub(super) fn key_value_table(headers: (&str, &str), rows: Vec<(&str, String)>) -> Table {
    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            CtCell::new(headers.0).fg(CtColor::Cyan),
            CtCell::new(headers.1).fg(CtColor::Cyan),
        ]);
    for (key, value) in rows {
        table.add_row(vec![CtCell::new(key).fg(CtColor::Blue), CtCell::new(value)]);
    }
    table
}

pub(super) fn masked_configured_label(value: &str) -> String {
    if value.trim().is_empty() {
        "not configured".yellow().bold().to_string()
    } else {
        "configured".green().bold().to_string()
    }
}

pub(super) fn shell_export_lines() -> Vec<String> {
    let port = std::env::var("BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(config::DEFAULT_BRIDGE_PORT);
    let mut lines = vec![
        "export ANTHROPIC_API_KEY=\"opencode-bridge\"".to_string(),
        format!("export ANTHROPIC_BASE_URL=\"http://127.0.0.1:{}/v1\"", port),
    ];
    if let Ok(model) = std::env::var("OPENCODE_MODEL") {
        if !model.trim().is_empty() {
            lines.push(format!(
                "export OPENCODE_MODEL=\"{}\"",
                model.replace('"', "\\\"")
            ));
        }
    }
    lines
}

/// Print proxy pool status table (used by `server status` and `proxy ps`).
pub(super) async fn print_proxy_table() -> Table {
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
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            CtCell::new("Node").fg(CtColor::Cyan),
            CtCell::new("Role").fg(CtColor::Cyan),
            CtCell::new("Status").fg(CtColor::Cyan),
            CtCell::new("Port").fg(CtColor::Cyan),
        ]);

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
            CtCell::new(short_name).fg(CtColor::Blue),
            CtCell::new(role),
            status_cell(status_str, status_color),
            CtCell::new(port.to_string()).fg(CtColor::Magenta),
        ]);
    }

    table
}

/// Print proxy pool table in Human mode; no-op in Json/Quiet.
pub(super) async fn maybe_print_proxy_table(fmt: OutputFormat) {
    if fmt == OutputFormat::Human {
        print_section("Proxy pool");
        let proxy_table = print_proxy_table().await;
        println!("{}", proxy_table);
    }
}

/// Bridge status dashboard with uptime and proxy pool table.
pub(super) async fn cmd_print_status(status: SupervisorStatus, fmt: OutputFormat) {
    if fmt == OutputFormat::Quiet {
        match status {
            SupervisorStatus::Running { .. } => println!("running"),
            SupervisorStatus::Stopped => println!("stopped"),
        }
        return;
    }

    match status {
        SupervisorStatus::Running {
            pid,
            port,
            started_at,
            managed,
        } => {
            let uptime = uptime_str(started_at);
            let model = std::env::var("OPENCODE_MODEL").unwrap_or_else(|_| "auto".into());
            let bridge_auth = if std::env::var("BRIDGE_AUTH_TOKEN")
                .ok()
                .filter(|t| !t.is_empty())
                .is_some()
            {
                "enabled".green().bold().to_string()
            } else {
                "disabled".yellow().bold().to_string()
            };
            let dashboard_auth = masked_configured_label(
                &std::env::var("DASHBOARD_ADMIN_TOKEN").unwrap_or_default(),
            );
            let managed_label = if managed {
                "supervisor-managed".green().bold().to_string()
            } else {
                "unmanaged / recovered by health probe"
                    .yellow()
                    .bold()
                    .to_string()
            };

            print_brand_header("OpenCode2API Bridge", "local Anthropic-compatible gateway");
            println!("{} {}", "●".green().bold(), "ONLINE".green().bold());
            let table = key_value_table(
                ("Runtime", "Value"),
                vec![
                    ("Endpoint", format!("http://127.0.0.1:{}/v1", port)),
                    ("Dashboard", format!("http://127.0.0.1:{}/dashboard/", port)),
                    (
                        "PID",
                        pid.map(|p| p.to_string())
                            .unwrap_or_else(|| "unmanaged".to_string()),
                    ),
                    ("Supervisor", managed_label),
                    ("Uptime", uptime),
                    ("Model", model),
                    ("Bridge auth", bridge_auth),
                    ("Dashboard auth", dashboard_auth),
                ],
            );
            println!("{}", table);
            maybe_print_proxy_table(fmt).await;
            print_tip("Use `eval \"$(opencode2api --quiet env)\"` to configure Claude Code.");
        }
        SupervisorStatus::Stopped => {
            print_brand_header("OpenCode2API Bridge", "daemon status");
            println!("{} {}", "●".red().bold(), "STOPPED".red().bold());
            let table = key_value_table(
                ("Check", "Value"),
                vec![
                    ("Bridge", "not running".red().bold().to_string()),
                    ("Next step", "opencode2api server start".to_string()),
                    ("Dashboard", "opencode2api dashboard start".to_string()),
                ],
            );
            println!("{}", table);
        }
    }
    println!();
}
#[derive(serde::Serialize)]
pub(super) struct ServerStatusInfo {
    status: String,
    pid: Option<u32>,
    uptime: Option<String>,
    message: Option<String>,
}

impl From<Result<SupervisorStatus, String>> for ServerStatusInfo {
    fn from(result: Result<SupervisorStatus, String>) -> Self {
        match result {
            Ok(SupervisorStatus::Running {
                pid,
                started_at,
                managed,
                ..
            }) => Self {
                status: "running".to_string(),
                pid,
                uptime: Some(uptime_str(started_at)),
                message: if managed {
                    None
                } else {
                    Some("running but not tracked by supervisor PID file".to_string())
                },
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

pub(super) fn cmd_print_env() {
    let port = std::env::var("BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(config::DEFAULT_BRIDGE_PORT);
    let model = std::env::var("OPENCODE_MODEL").unwrap_or_else(|_| "auto".to_string());

    print_brand_header(
        "Claude Code Environment",
        "copy these values into your shell session",
    );
    let table = key_value_table(
        ("Variable", "Value"),
        vec![
            (
                "ANTHROPIC_API_KEY",
                "opencode-bridge".green().dim().to_string(),
            ),
            (
                "ANTHROPIC_BASE_URL",
                format!("http://127.0.0.1:{}/v1", port)
                    .cyan()
                    .bold()
                    .to_string(),
            ),
            ("OPENCODE_MODEL", model.yellow().bold().to_string()),
        ],
    );
    println!("{}", table);
    print_section("Shell setup");
    println!("{}", "eval \"$(opencode2api --quiet env)\"".green().bold());
    print_tip("Human mode is for reading; --quiet prints eval-safe export lines.");
    println!();
}

pub(super) fn cmd_print_config() {
    let config = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| "auto (claude-3-5-sonnet)".to_string());
    let auth = if config.auth_enabled() {
        "enabled".green().bold().to_string()
    } else {
        "disabled".yellow().bold().to_string()
    };

    print_brand_header("Server Configuration", "effective runtime settings");
    let table = key_value_table(
        ("Setting", "Value"),
        vec![
            (
                "Bridge host",
                config.host.to_string().cyan().bold().to_string(),
            ),
            (
                "Bridge port",
                config.bridge_port.to_string().cyan().bold().to_string(),
            ),
            ("API auth", auth),
            (
                "Shell policy",
                config.shell_policy.description().cyan().bold().to_string(),
            ),
            ("Model", model.yellow().bold().to_string()),
            (
                "Max body size",
                format!("{} bytes", config.max_body_size)
                    .cyan()
                    .bold()
                    .to_string(),
            ),
            (
                "Search loops",
                config
                    .max_search_loops
                    .to_string()
                    .cyan()
                    .bold()
                    .to_string(),
            ),
        ],
    );
    println!("{}", table);
    print_tip("Use `opencode2api init --force` to regenerate a config template.");
    println!();
}

pub(super) fn show_logs(fmt: OutputFormat) {
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

            if fmt == OutputFormat::Human {
                print_brand_header("Bridge Logs", "last 100 daemon lines");
                println!("{} {}", "File".cyan().bold(), log_path.display());
                println!();
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
