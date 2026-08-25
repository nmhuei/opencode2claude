//! Shared presentation helpers for the line-oriented CLI.
//!
//! Human output is intentionally simple: a two-line brand header, whitespace,
//! borderless facts/tables, semantic status colors, and actionable hints.

use crate::config::{self, BridgeConfig};
use crate::docker;
use crate::output::OutputFormat;
use crate::presentation;
use crate::proxy_pool;
use crate::runtime::RuntimePaths;
use crate::supervisor::SupervisorStatus;
use comfy_table::{presets, Cell as CtCell, Color as CtColor, ContentArrangement, Table};
use yansi::Paint;

fn uptime_str(started_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let elapsed_secs = now.saturating_sub(started_at) / 1000;
    let hours = elapsed_secs / 3600;
    let mins = (elapsed_secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn proxy_container_state_text(running: bool) -> &'static str {
    if running {
        "● running"
    } else {
        "○ offline"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadinessSummary {
    ready: bool,
    workers_ready: bool,
    egress_ready: bool,
    egress_mode: String,
    verified_unique_exit_ips: u64,
    minimum_unique_exit_ips: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RuntimeSnapshot {
    version: Option<String>,
    health_latency_ms: Option<u128>,
    readiness: Option<ReadinessSummary>,
}

fn parse_readiness_summary(value: &serde_json::Value) -> Option<ReadinessSummary> {
    Some(ReadinessSummary {
        ready: value.get("status")?.as_str()? == "ready",
        workers_ready: value.get("checks")?.get("critical_workers")?.as_bool()?,
        egress_ready: value.get("checks")?.get("egress")?.as_bool()?,
        egress_mode: value.get("egress")?.get("mode")?.as_str()?.to_string(),
        verified_unique_exit_ips: value
            .get("egress")?
            .get("verified_unique_exit_ips")?
            .as_u64()?,
        minimum_unique_exit_ips: value
            .get("egress")?
            .get("minimum_unique_exit_ips")?
            .as_u64()?,
    })
}

async fn fetch_runtime_snapshot(port: u16) -> RuntimeSnapshot {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return RuntimeSnapshot::default();
    };

    let health_url = format!("http://127.0.0.1:{port}/health/live");
    let readiness_url = format!("http://127.0.0.1:{port}/health/ready");
    let health_started = std::time::Instant::now();
    let health = client.get(health_url).send().await;
    let health_latency_ms = health
        .as_ref()
        .ok()
        .map(|_| health_started.elapsed().as_millis());
    let version = match health {
        Ok(response) => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body.get("version")?.as_str().map(ToOwned::to_owned)),
        Err(_) => None,
    };
    let readiness = match client.get(readiness_url).send().await {
        Ok(response) => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| parse_readiness_summary(&body)),
        Err(_) => None,
    };

    RuntimeSnapshot {
        version,
        health_latency_ms,
        readiness,
    }
}

fn configured_egress_mode(config: &BridgeConfig) -> &'static str {
    match config.egress.mode {
        config::EgressMode::Direct => "direct",
        config::EgressMode::Proxy => "proxy",
        config::EgressMode::Hybrid => "hybrid",
    }
}

fn model_route_summary(config: &BridgeConfig) -> String {
    let requested = config
        .model
        .as_deref()
        .unwrap_or(crate::config::DEFAULT_MODEL);
    let upstream = crate::opencode::mapper::map_model_name(requested);
    if config.model.is_none() {
        format!("auto · {requested} → {upstream}")
    } else if requested == upstream {
        requested.to_string()
    } else {
        format!("{requested} → {upstream}")
    }
}

fn human_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.1} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.1} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Print the product mark and one concise descriptor line.
pub(super) fn print_brand_header(title: &str, subtitle: &str) {
    println!();
    println!(
        "{} {}",
        presentation::BRAND_SYMBOL.cyan().bold(),
        "OpenCode2API".bold()
    );

    let descriptor = match (title.trim(), subtitle.trim()) {
        ("", "") => String::new(),
        (title, "") => title.to_string(),
        ("", subtitle) => subtitle.to_string(),
        (title, subtitle) => format!("{title} · {subtitle}"),
    };
    if !descriptor.is_empty() {
        println!(
            "{}{}",
            " ".repeat(presentation::INDENT),
            presentation::truncate(&descriptor, presentation::content_width()).dim()
        );
    }
    println!();
}

pub(super) fn print_section(title: &str) {
    println!();
    print_section_first(title);
}

pub(super) fn print_section_first(title: &str) {
    println!(
        "{}{}",
        " ".repeat(presentation::INDENT),
        presentation::truncate(title, presentation::content_width()).bold()
    );
}

pub(super) fn print_tip(message: &str) {
    let width = presentation::content_width()
        .saturating_sub(presentation::INDENT + 2)
        .max(20);
    for (index, line) in presentation::wrap(message, width).into_iter().enumerate() {
        let prefix = if index == 0 { "›" } else { " " };
        println!(
            "{}{} {}",
            " ".repeat(presentation::INDENT),
            prefix.dim(),
            line.dim()
        );
    }
}

pub(super) fn print_success(message: &str) {
    print_status_message("✓", message, |value| value.green().bold().to_string());
}

pub(super) fn print_warning(message: &str) {
    print_status_message("▲", message, |value| value.yellow().bold().to_string());
}

fn print_status_message<F>(symbol: &str, message: &str, style_symbol: F)
where
    F: Fn(&str) -> String,
{
    let width = presentation::content_width()
        .saturating_sub(presentation::INDENT + 2)
        .max(20);
    for (index, line) in presentation::wrap(message, width).into_iter().enumerate() {
        if index == 0 {
            println!(
                "{}{} {}",
                " ".repeat(presentation::INDENT),
                style_symbol(symbol),
                line
            );
        } else {
            println!("{}{}", " ".repeat(presentation::INDENT * 2), line);
        }
    }
}

pub(super) fn print_error(title: &str, cause: &str, suggestions: &[&str]) {
    eprintln!();
    eprintln!(
        "{}{} {}",
        " ".repeat(presentation::INDENT),
        "×".red().bold(),
        title.bold()
    );
    for line in presentation::wrap(
        cause,
        presentation::content_width().saturating_sub(presentation::INDENT * 2),
    ) {
        eprintln!("{}{}", " ".repeat(presentation::INDENT * 2), line.dim());
    }
    if !suggestions.is_empty() {
        eprintln!();
        eprintln!("{}Try:", " ".repeat(presentation::INDENT * 2));
        for suggestion in suggestions {
            eprintln!(
                "{}{}",
                " ".repeat(presentation::INDENT * 3),
                suggestion.cyan()
            );
        }
    }
    eprintln!();
}

/// Borderless key/value table retained for command views that need a `Table`.
pub(super) fn print_table(table: &Table) {
    for line in table.to_string().lines() {
        let line = line.trim();
        if !line.is_empty() {
            println!("{}{}", " ".repeat(presentation::INDENT), line);
        }
    }
}

pub(super) fn key_value_table(_headers: (&str, &str), rows: Vec<(&str, String)>) -> Table {
    let mut table = Table::new();
    table
        .load_preset(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_width(presentation::content_width() as u16);

    for (key, value) in rows {
        table.add_row(vec![
            CtCell::new(key).fg(CtColor::DarkGrey),
            CtCell::new(value),
        ]);
    }
    table
}

pub(super) fn masked_configured_label(value: &str) -> String {
    if value.trim().is_empty() {
        "not configured".yellow().to_string()
    } else {
        "configured".green().to_string()
    }
}

pub(super) fn claude_code_base_url(config: &BridgeConfig) -> String {
    crate::application::integration::base_url(config)
}

pub(super) fn shell_export_lines(config: &BridgeConfig) -> Vec<String> {
    crate::application::integration::environment(config).shell_exports
}

/// Render the proxy pool using a borderless table.
pub(super) async fn print_proxy_table_for_ports(primary_ports: &[u16], ws_ports: &[u16]) -> Table {
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
        .set_width(presentation::content_width() as u16);

    if presentation::compact() {
        table.set_header(vec![
            CtCell::new("PORT").fg(CtColor::DarkGrey),
            CtCell::new("ROLE").fg(CtColor::DarkGrey),
            CtCell::new("STATE").fg(CtColor::DarkGrey),
        ]);
    } else {
        table.set_header(vec![
            CtCell::new("PORT").fg(CtColor::DarkGrey),
            CtCell::new("ROLE").fg(CtColor::DarkGrey),
            CtCell::new("STATE").fg(CtColor::DarkGrey),
            CtCell::new("CONTAINER").fg(CtColor::DarkGrey),
        ]);
    }

    for (port, name, running) in &containers {
        let role = if primary_ports.contains(port) {
            "primary"
        } else {
            "standby"
        };
        let status = if *running {
            CtCell::new(proxy_container_state_text(true)).fg(CtColor::Cyan)
        } else {
            CtCell::new(proxy_container_state_text(false)).fg(CtColor::DarkGrey)
        };

        let mut row = vec![CtCell::new(port.to_string()), CtCell::new(role), status];
        if !presentation::compact() {
            row.push(CtCell::new(name));
        }
        table.add_row(row);
    }

    table
}

pub(super) async fn print_proxy_table() -> Table {
    let resolved = BridgeConfig::from_env_and_cli(crate::config::CliOverrides::default());
    let primary_ports = proxy_pool::configured_primary_ports(&resolved);
    let standby_ports = proxy_pool::configured_warm_standby_ports(&resolved);
    print_proxy_table_for_ports(&primary_ports, &standby_ports).await
}

pub(super) async fn maybe_print_proxy_table(fmt: OutputFormat) {
    if fmt == OutputFormat::Human {
        print_section("Proxies");
        let table = print_proxy_table().await;
        print_table(&table);
    }
}

pub(super) async fn print_start_summary(status: &SupervisorStatus, config: &BridgeConfig) {
    let SupervisorStatus::Running { pid, port, .. } = status else {
        return;
    };
    let snapshot = fetch_runtime_snapshot(*port).await;
    let readiness = snapshot
        .readiness
        .as_ref()
        .map(|value| {
            if value.ready {
                "ready".green().to_string()
            } else {
                "starting".yellow().to_string()
            }
        })
        .unwrap_or_else(|| "checking".yellow().to_string());
    let egress = snapshot
        .readiness
        .as_ref()
        .map(|value| value.egress_mode.clone())
        .unwrap_or_else(|| configured_egress_mode(config).to_string());
    println!(
        "{}",
        presentation::facts(&[
            ("Endpoint", format!("http://127.0.0.1:{port}")),
            ("Model route", model_route_summary(config)),
            ("Egress", egress),
            ("Readiness", readiness),
            (
                "Process",
                pid.map(|value| value.to_string())
                    .unwrap_or_else(|| "unmanaged".to_string()),
            ),
        ])
    );
    println!();
    print_tip("Claude Code: `opencode2api set env` · Logs: `opencode2api server logs` · Diagnose: `opencode2api doctor`");
}

pub(super) async fn cmd_print_status(
    status: SupervisorStatus,
    fmt: OutputFormat,
    config: &BridgeConfig,
) {
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
            let snapshot = fetch_runtime_snapshot(port).await;
            let observed_egress = snapshot
                .readiness
                .as_ref()
                .map(|value| value.egress_mode.as_str())
                .unwrap_or_else(|| configured_egress_mode(config));

            print_brand_header("Gateway status", "Runtime, routing and health snapshot");
            let headline = if snapshot.readiness.as_ref().is_some_and(|value| value.ready) {
                "Running · ready".green().bold().to_string()
            } else if snapshot.readiness.is_some() {
                "Running · degraded".yellow().bold().to_string()
            } else {
                "Running · health unknown".yellow().bold().to_string()
            };
            println!(
                "{}{} {}",
                " ".repeat(presentation::INDENT),
                "●".green().bold(),
                headline
            );

            print_section("Connection");
            println!(
                "{}",
                presentation::facts(&[
                    ("Base URL", format!("http://127.0.0.1:{port}")),
                    ("Anthropic", format!("http://127.0.0.1:{port}/v1/messages")),
                    (
                        "OpenAI",
                        format!("http://127.0.0.1:{port}/v1/chat/completions"),
                    ),
                    ("Dashboard", format!("http://127.0.0.1:{port}/dashboard")),
                ])
            );

            print_section("Runtime");
            let health = match snapshot.health_latency_ms {
                Some(ms) => format!("live · {ms} ms"),
                None => "unavailable".yellow().to_string(),
            };
            println!(
                "{}",
                presentation::facts(&[
                    (
                        "Version",
                        snapshot
                            .version
                            .clone()
                            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
                    ),
                    ("Health", health),
                    ("Model route", model_route_summary(config)),
                    ("Egress", observed_egress.to_string()),
                    (
                        "Process",
                        pid.map(|value| value.to_string())
                            .unwrap_or_else(|| "unmanaged".to_string()),
                    ),
                    ("Uptime", uptime_str(started_at)),
                    (
                        "Supervisor",
                        if managed {
                            "managed".green().to_string()
                        } else {
                            "unmanaged".yellow().to_string()
                        },
                    ),
                ])
            );

            print_section("Readiness");
            if let Some(readiness) = &snapshot.readiness {
                let mut rows = vec![
                    (
                        "Gateway",
                        if readiness.ready {
                            "ready".green().to_string()
                        } else {
                            "not ready".red().to_string()
                        },
                    ),
                    (
                        "Workers",
                        if readiness.workers_ready {
                            "ready".green().to_string()
                        } else {
                            "degraded".yellow().to_string()
                        },
                    ),
                    (
                        "Egress path",
                        if readiness.egress_ready {
                            format!("{} · ready", readiness.egress_mode)
                                .green()
                                .to_string()
                        } else {
                            format!("{} · not ready", readiness.egress_mode)
                                .red()
                                .to_string()
                        },
                    ),
                ];
                match readiness.egress_mode.as_str() {
                    "proxy" => rows.push((
                        "Exit identities",
                        format!(
                            "{} verified · minimum {}",
                            readiness.verified_unique_exit_ips, readiness.minimum_unique_exit_ips
                        ),
                    )),
                    "hybrid" => rows.push((
                        "Proxy identities",
                        format!(
                            "{} verified · direct fallback available",
                            readiness.verified_unique_exit_ips
                        ),
                    )),
                    _ => {}
                }
                println!("{}", presentation::facts(&rows));
            } else {
                println!(
                    "{}",
                    presentation::facts(&[(
                        "Gateway",
                        "readiness endpoint unavailable".yellow().to_string(),
                    )])
                );
            }

            print_section("Security & files");
            let paths = RuntimePaths::from_config(config);
            println!(
                "{}",
                presentation::facts(&[
                    (
                        "Bridge API auth",
                        if config.auth_enabled() {
                            "enabled".green().to_string()
                        } else {
                            "disabled".yellow().to_string()
                        },
                    ),
                    (
                        "Dashboard auth",
                        if config.management.dashboard_token().is_some() {
                            "configured".green().to_string()
                        } else {
                            "not configured".yellow().to_string()
                        },
                    ),
                    (
                        "Config",
                        config.management.config_path.display().to_string()
                    ),
                    ("Runtime", paths.runtime_dir().display().to_string()),
                    ("Log", paths.bridge_log().display().to_string()),
                ])
            );

            if observed_egress != "direct" {
                maybe_print_proxy_table(fmt).await;
            }
            println!();
            print_tip("Claude Code: `opencode2api set env` · Live logs: `opencode2api server logs` · Full checks: `opencode2api doctor`");
        }
        SupervisorStatus::Stopped => {
            print_brand_header("Gateway status", "Runtime, routing and health snapshot");
            println!(
                "{}{} {}",
                " ".repeat(presentation::INDENT),
                "○".dim(),
                "Stopped".bold()
            );
            println!();
            let paths = RuntimePaths::from_config(config);
            println!(
                "{}",
                presentation::facts(&[
                    ("Gateway", "not running".to_string()),
                    ("Configured model", model_route_summary(config)),
                    (
                        "Configured egress",
                        configured_egress_mode(config).to_string()
                    ),
                    ("Runtime", paths.runtime_dir().display().to_string()),
                    ("Start", "opencode2api server start".cyan().to_string()),
                ])
            );
            println!();
            print_tip("Run `opencode2api doctor` if the gateway does not start cleanly.");
        }
    }
    println!();
}

#[derive(Debug, serde::Serialize)]
pub(super) struct ServerStatusInfo {
    status: String,
    endpoint: Option<String>,
    dashboard_url: Option<String>,
    pid: Option<u32>,
    uptime: Option<String>,
    model: Option<String>,
    model_route: String,
    configured_egress_mode: String,
    observed_egress_mode: Option<String>,
    version: Option<String>,
    health_latency_ms: Option<u128>,
    ready: Option<bool>,
    managed: Option<bool>,
    auth_enabled: bool,
    dashboard_auth_configured: bool,
    config_path: String,
    runtime_dir: String,
    message: Option<String>,
}

impl ServerStatusInfo {
    pub(super) fn from_status(
        result: Result<SupervisorStatus, String>,
        config: &BridgeConfig,
    ) -> Self {
        let paths = RuntimePaths::from_config(config);
        let model_route = model_route_summary(config);
        let configured_egress_mode = configured_egress_mode(config).to_string();
        let config_path = config.management.config_path.display().to_string();
        let runtime_dir = paths.runtime_dir().display().to_string();
        match result {
            Ok(SupervisorStatus::Running {
                pid,
                port,
                started_at,
                managed,
            }) => Self {
                status: "running".to_string(),
                endpoint: Some(format!("http://127.0.0.1:{port}")),
                dashboard_url: Some(format!("http://127.0.0.1:{port}/dashboard")),
                pid,
                uptime: Some(uptime_str(started_at)),
                model: config.model.clone(),
                model_route: model_route.clone(),
                configured_egress_mode: configured_egress_mode.clone(),
                observed_egress_mode: None,
                version: None,
                health_latency_ms: None,
                ready: None,
                managed: Some(managed),
                auth_enabled: config.auth_enabled(),
                dashboard_auth_configured: config.management.dashboard_token().is_some(),
                config_path: config_path.clone(),
                runtime_dir: runtime_dir.clone(),
                message: if managed {
                    None
                } else {
                    Some("running but not tracked by supervisor PID file".to_string())
                },
            },
            Ok(SupervisorStatus::Stopped) => Self {
                status: "stopped".to_string(),
                endpoint: None,
                dashboard_url: None,
                pid: None,
                uptime: None,
                model: config.model.clone(),
                model_route: model_route.clone(),
                configured_egress_mode: configured_egress_mode.clone(),
                observed_egress_mode: None,
                version: None,
                health_latency_ms: None,
                ready: None,
                managed: None,
                auth_enabled: config.auth_enabled(),
                dashboard_auth_configured: config.management.dashboard_token().is_some(),
                config_path: config_path.clone(),
                runtime_dir: runtime_dir.clone(),
                message: None,
            },
            Err(error) => Self {
                status: "error".to_string(),
                endpoint: None,
                dashboard_url: None,
                pid: None,
                uptime: None,
                model: config.model.clone(),
                model_route,
                configured_egress_mode,
                observed_egress_mode: None,
                version: None,
                health_latency_ms: None,
                ready: None,
                managed: None,
                auth_enabled: config.auth_enabled(),
                dashboard_auth_configured: config.management.dashboard_token().is_some(),
                config_path,
                runtime_dir,
                message: Some(error),
            },
        }
    }

    pub(super) async fn enrich_runtime(&mut self, port: u16) {
        let snapshot = fetch_runtime_snapshot(port).await;
        self.version = snapshot.version;
        self.health_latency_ms = snapshot.health_latency_ms;
        self.ready = snapshot.readiness.as_ref().map(|value| value.ready);
        self.observed_egress_mode = snapshot
            .readiness
            .as_ref()
            .map(|value| value.egress_mode.clone());
    }
}

pub(super) fn cmd_print_env(config: &BridgeConfig) {
    print_brand_header(
        "Claude Code environment",
        "Connection values and shell setup",
    );

    let model = config.model.clone().unwrap_or_else(|| "auto".to_string());
    let api_key_status = if config.auth_enabled() {
        "configured; hidden in human output"
    } else {
        "compatibility key; authentication disabled"
    };
    let base_url = claude_code_base_url(config);
    let mut rows = vec![
        ("ANTHROPIC_API_KEY", api_key_status.to_string()),
        ("ANTHROPIC_BASE_URL", base_url.clone().cyan().to_string()),
        ("OPENAI_API_KEY", api_key_status.to_string()),
        (
            "OPENAI_BASE_URL",
            format!("{base_url}/v1").cyan().to_string(),
        ),
        ("OPENCODE_MODEL", model.clone()),
    ];
    if model == crate::application::integration::OX_ALPHA_MODEL {
        rows.extend([
            (
                "ANTHROPIC_MODEL",
                crate::application::integration::OX_ALPHA_CLAUDE_MODEL.to_string(),
            ),
            ("CLAUDE_CODE_DISABLE_1M_CONTEXT", "0".to_string()),
            (
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
                crate::application::integration::OX_ALPHA_MAX_OUTPUT_TOKENS.to_string(),
            ),
            (
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
                crate::application::integration::OX_ALPHA_AUTO_COMPACT_WINDOW.to_string(),
            ),
            ("CLAUDE_CODE_DISABLE_THINKING", "0".to_string()),
            ("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "0".to_string()),
            ("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT", "1".to_string()),
            ("CLAUDE_CODE_EFFORT_LEVEL", "max".to_string()),
            (
                "MAX_THINKING_TOKENS",
                crate::application::integration::OX_ALPHA_MAX_THINKING_TOKENS.to_string(),
            ),
        ]);
    }
    println!("{}", presentation::facts(&rows));

    print_section("Shell setup");
    println!(
        "{}{}",
        " ".repeat(presentation::INDENT),
        "opencode2api set env".cyan()
    );
    println!();
    print_tip("Release installs add a managed bash/zsh hook. Quiet mode still prints eval-safe exports for automation.");
    println!();
}

pub(super) fn cmd_print_config() {
    let config = BridgeConfig::from_env_and_cli(config::CliOverrides::default());
    let paths = RuntimePaths::from_config(&config);
    let primary_ports = proxy_pool::configured_primary_ports(&config);
    let standby_ports = proxy_pool::configured_warm_standby_ports(&config);

    print_brand_header(
        "Server configuration",
        "Effective routing, limits and runtime paths",
    );

    print_section_first("Server");
    println!(
        "{}",
        presentation::facts(&[
            (
                "Listen",
                format!("http://{}:{}", config.host, config.bridge_port)
            ),
            ("Max request", human_bytes(config.max_body_size)),
            ("Stream buffer", human_bytes(config.stream_buffer_size)),
            ("Channel capacity", config.channel_capacity.to_string()),
        ])
    );

    print_section("Routing");
    println!(
        "{}",
        presentation::facts(&[
            ("Model route", model_route_summary(&config)),
            ("Upstream", config.retry.upstream_base_url.clone()),
            ("Egress", configured_egress_mode(&config).to_string()),
            (
                "Proxy topology",
                format!(
                    "{} primary · {} standby",
                    primary_ports.len(),
                    standby_ports.len()
                ),
            ),
            (
                "Retries",
                format!(
                    "{} network · {} provider",
                    config.retry.max_network_attempts, config.retry.max_provider_attempts
                ),
            ),
        ])
    );

    print_section("Features & security");
    println!(
        "{}",
        presentation::facts(&[
            (
                "Bridge API auth",
                if config.auth_enabled() {
                    "enabled".green().to_string()
                } else {
                    "disabled".yellow().to_string()
                },
            ),
            (
                "Dashboard auth",
                if config.management.dashboard_token().is_some() {
                    "configured".green().to_string()
                } else {
                    "not configured".yellow().to_string()
                },
            ),
            (
                "Shell policy",
                config.shell_policy.description().to_string()
            ),
            ("Search loops", config.max_search_loops.to_string()),
            (
                "Metrics",
                if config.observability.metrics_enabled {
                    "enabled".green().to_string()
                } else {
                    "disabled".dim().to_string()
                },
            ),
            (
                "Request history",
                if config.history.enabled {
                    format!("enabled · {}", config.history.capture_mode)
                } else {
                    "disabled".dim().to_string()
                },
            ),
        ])
    );

    print_section("Files");
    println!(
        "{}",
        presentation::facts(&[
            (
                "Config",
                config.management.config_path.display().to_string()
            ),
            ("Runtime", paths.runtime_dir().display().to_string()),
            ("PID", paths.pid_file().display().to_string()),
            ("Log", paths.bridge_log().display().to_string()),
            ("History DB", paths.history_database().display().to_string()),
        ])
    );

    println!();
    print_tip("Use `opencode2api server status` for observed runtime health; configuration can differ from a daemon that was started with CLI overrides.");
    println!();
}

pub(super) fn show_logs(fmt: OutputFormat) {
    let log_path = RuntimePaths::new().bridge_log();
    if !log_path.exists() {
        print_error(
            "No bridge log file found",
            "The daemon has not created its log file yet.",
            &["opencode2api server start", "opencode2api server status"],
        );
        std::process::exit(1);
    }

    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            let clean_lines: Vec<String> = content.lines().map(crate::tui::strip_ansi).collect();
            let start = clean_lines.len().saturating_sub(100);
            let tail = &clean_lines[start..];

            if fmt == OutputFormat::Json {
                #[derive(serde::Serialize)]
                struct LogEntry<'a> {
                    line: &'a str,
                    line_number: usize,
                }
                let entries: Vec<LogEntry<'_>> = tail
                    .iter()
                    .enumerate()
                    .map(|(index, line)| LogEntry {
                        line,
                        line_number: start + index + 1,
                    })
                    .collect();
                match serde_json::to_string_pretty(&entries) {
                    Ok(json) => println!("{json}"),
                    Err(error) => print_error("Could not serialize logs", &error.to_string(), &[]),
                }
                return;
            }

            if fmt == OutputFormat::Human {
                print_brand_header("Bridge logs", "Last 100 daemon lines");
                println!(
                    "{}",
                    presentation::facts(&[("File", log_path.display().to_string())])
                );
                println!();
            }

            for line in tail {
                if fmt == OutputFormat::Quiet {
                    println!("{line}");
                    continue;
                }

                let styled = if line.contains("ERROR") {
                    line.replace("ERROR", &"ERROR".red().bold().to_string())
                } else if line.contains("WARN") {
                    line.replace("WARN", &"WARN".yellow().bold().to_string())
                } else if line.contains("INFO") {
                    line.replace("INFO", &"INFO".cyan().bold().to_string())
                } else {
                    line.clone()
                };
                println!("{styled}");
            }
        }
        Err(error) => {
            print_error(
                "Could not read bridge logs",
                &error.to_string(),
                &["opencode2api server status"],
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_exports_use_resolved_token_and_root_base_url() {
        let config = BridgeConfig {
            bridge_port: 4010,
            auth_tokens: Some(vec!["configured-secret".into()]),
            model: Some("opencode/deepseek-v4-flash-free".to_string()),
            ..Default::default()
        };

        let lines = shell_export_lines(&config);
        assert_eq!(lines[0], "export ANTHROPIC_API_KEY='configured-secret'");
        assert_eq!(
            lines[1],
            "export ANTHROPIC_BASE_URL='http://127.0.0.1:4010'"
        );
        assert!(!lines[1].ends_with("/v1'"));
    }

    #[test]
    fn shell_exports_quote_single_quotes_safely() {
        let config = BridgeConfig {
            auth_tokens: Some(vec!["token'with-quote".into()]),
            ..Default::default()
        };

        let lines = shell_export_lines(&config);
        assert_eq!(
            lines[0],
            "export ANTHROPIC_API_KEY='token'\"'\"'with-quote'"
        );
    }

    #[test]
    fn proxy_container_state_does_not_claim_end_to_end_health() {
        assert_eq!(proxy_container_state_text(true), "● running");
        assert_eq!(proxy_container_state_text(false), "○ offline");
    }

    #[test]
    fn readiness_summary_distinguishes_running_process_from_unusable_egress() {
        let summary = parse_readiness_summary(&serde_json::json!({
            "status": "not_ready",
            "checks": {"critical_workers": true, "egress": false},
            "egress": {
                "mode": "proxy",
                "verified_unique_exit_ips": 4,
                "minimum_unique_exit_ips": 1
            }
        }))
        .expect("valid readiness response");
        assert!(!summary.ready);
        assert!(summary.workers_ready);
        assert!(!summary.egress_ready);
        assert_eq!(summary.egress_mode, "proxy");
        assert_eq!(summary.verified_unique_exit_ips, 4);
        assert_eq!(summary.minimum_unique_exit_ips, 1);
    }

    #[test]
    fn model_route_summary_explains_default_claude_mapping() {
        let config = BridgeConfig::default();
        assert_eq!(
            model_route_summary(&config),
            "auto · claude-3-5-sonnet → x-preview-f-free"
        );
    }

    #[test]
    fn human_bytes_uses_readable_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(64 * 1024), "64.0 KiB");
        assert_eq!(human_bytes(64 * 1024 * 1024), "64.0 MiB");
    }

    #[test]
    fn key_value_table_has_no_box_borders() {
        let table = key_value_table(("Key", "Value"), vec![("Port", "4000".into())]);
        let rendered = table.to_string();
        assert!(!rendered.contains('│'));
        assert!(!rendered.contains('┌'));
    }
}
