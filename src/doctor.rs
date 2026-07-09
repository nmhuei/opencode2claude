//! Doctor module — diagnostic checks for the OpenCode2Claude bridge.
//!
//! The `doctor` subcommand runs a suite of checks to diagnose common issues:
//! Docker daemon, port availability, proxy container status, config file, auth,
//! and upstream reachability.

use crate::config;
use crate::docker;
use crate::proxy_pool;
use crate::tui;
use serde::Serialize;
use std::fmt;
use yansi::Paint;

/// Result of a single diagnostic check.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// Machine-readable check name (e.g. "docker-daemon").
    pub name: String,
    /// Human-readable label (e.g. "Docker Daemon").
    pub label: String,
    /// Check status.
    pub status: CheckStatus,
    /// Human-readable detail message.
    pub message: String,
}

/// Status of a single diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// Full doctor report with all check results and a summary.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    pub summary: DoctorSummary,
}

/// Aggregate summary of all diagnostic checks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum DoctorSummary {
    AllPass,
    Warnings(u8),
    Failures(u8),
}

impl DoctorSummary {
    pub fn exit_code(&self) -> i32 {
        match self {
            DoctorSummary::AllPass => 0,
            DoctorSummary::Warnings(_) => 0,
            DoctorSummary::Failures(_) => 1,
        }
    }
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{}",
            "╭────────────────────────────────────────────────────────────╮"
                .cyan()
                .bold()
        )?;
        writeln!(
            f,
            "{} {} {}",
            "│".cyan().bold(),
            "Doctor".bold(),
            "dependency and runtime diagnostics".dim()
        )?;
        writeln!(
            f,
            "{}",
            "╰────────────────────────────────────────────────────────────╯"
                .cyan()
                .bold()
        )?;
        writeln!(f)?;
        for check in &self.checks {
            writeln!(f, " {}", check)?;
        }
        writeln!(f)?;
        match self.summary {
            DoctorSummary::AllPass => {
                writeln!(f, " {} All checks passed.", "✓".green().bold())?;
            }
            DoctorSummary::Warnings(n) => {
                writeln!(
                    f,
                    " {} {} warning{} — bridge should still operate",
                    "⚠".yellow().bold(),
                    n,
                    if n == 1 { "" } else { "s" }
                )?;
            }
            DoctorSummary::Failures(n) => {
                writeln!(
                    f,
                    " {} {} failure{} — bridge may not operate correctly",
                    "✗".red().bold(),
                    n,
                    if n == 1 { "" } else { "s" }
                )?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for CheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let icon = match self.status {
            CheckStatus::Pass => "✓".green().bold(),
            CheckStatus::Warn => "⚠".yellow().bold(),
            CheckStatus::Fail => "✗".red().bold(),
        };
        write!(
            f,
            "{} {} {}",
            icon,
            tui::pad_to_width(&self.label.cyan().bold().to_string(), 22),
            self.message.dim()
        )
    }
}

/// Run all diagnostic checks and return a report.
pub async fn run_diagnostics() -> DoctorReport {
    let mut checks = Vec::new();

    // 1. Docker daemon
    checks.push(check_docker().await);

    // 2. Port availability
    checks.push(check_port().await);

    // 3. Proxy containers
    checks.push(check_proxies().await);

    // 4. Config file
    checks.push(check_config());

    // 5. Auth status
    checks.push(check_auth());

    // 6. Upstream DNS / egress
    checks.push(check_upstream().await);

    // Compute summary
    let mut warnings = 0u8;
    let mut failures = 0u8;
    for c in &checks {
        match c.status {
            CheckStatus::Warn => warnings += 1,
            CheckStatus::Fail => failures += 1,
            _ => {}
        }
    }

    let summary = if failures > 0 {
        DoctorSummary::Failures(failures)
    } else if warnings > 0 {
        DoctorSummary::Warnings(warnings)
    } else {
        DoctorSummary::AllPass
    };

    DoctorReport { checks, summary }
}

async fn check_docker() -> CheckResult {
    let name = "docker-daemon".to_string();
    let label = "Docker Daemon".to_string();

    match docker::check_daemon().await {
        Ok(version) => CheckResult {
            name,
            label,
            status: CheckStatus::Pass,
            message: format!("Docker is running ({})", version),
        },
        Err(e) => CheckResult {
            name,
            label,
            status: CheckStatus::Fail,
            message: format!(
                "Docker daemon not reachable: {}. Try: sudo systemctl start docker",
                e
            ),
        },
    }
}

async fn check_port() -> CheckResult {
    let name = "port-bridge".to_string();
    let label = "Bridge Port".to_string();
    let port = config::DEFAULT_BRIDGE_PORT;

    match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => {
            drop(listener);
            CheckResult {
                name,
                label,
                status: CheckStatus::Pass,
                message: format!("Port {} is available", port),
            }
        }
        Err(_) => CheckResult {
            name,
            label,
            status: CheckStatus::Warn,
            message: format!(
                "Port {} is already in use. Try: use -p <port> or stop the other process",
                port
            ),
        },
    }
}

async fn check_proxies() -> CheckResult {
    let name = "proxy-containers".to_string();
    let label = "Proxy Containers".to_string();
    let primary_ports = proxy_pool::get_primary_ports();

    let containers = docker::list_containers(&primary_ports).await;
    let running = containers.iter().filter(|(_, _, r)| *r).count();
    let total = containers.len();

    if running == total && total > 0 {
        CheckResult {
            name,
            label,
            status: CheckStatus::Pass,
            message: format!("All {} primary containers running", total),
        }
    } else if running > 0 {
        CheckResult {
            name,
            label,
            status: CheckStatus::Warn,
            message: format!(
                "{}/{} primary containers running ({} missing)",
                running,
                total,
                total - running
            ),
        }
    } else {
        CheckResult {
            name,
            label,
            status: CheckStatus::Warn,
            message: "No primary proxy containers running. Try: proxy restart".to_string(),
        }
    }
}

fn check_config() -> CheckResult {
    let name = "config-file".to_string();
    let label = "Config File".to_string();
    let config_paths = ["opencode2api.toml"];

    for path_str in &config_paths {
        let path = std::path::Path::new(path_str);
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(contents) => match toml::from_str::<config::TomlConfig>(&contents) {
                    Ok(_) => {
                        return CheckResult {
                            name,
                            label,
                            status: CheckStatus::Pass,
                            message: format!("{} parsed OK", path_str),
                        };
                    }
                    Err(e) => {
                        return CheckResult {
                            name,
                            label,
                            status: CheckStatus::Fail,
                            message: format!("{} parse error: {}", path_str, e),
                        };
                    }
                },
                Err(e) => {
                    return CheckResult {
                        name,
                        label,
                        status: CheckStatus::Fail,
                        message: format!("{} read error: {}", path_str, e),
                    };
                }
            }
        }
    }

    CheckResult {
        name,
        label,
        status: CheckStatus::Pass,
        message: "No config file — using defaults".to_string(),
    }
}

fn check_auth() -> CheckResult {
    let name = "auth-status".to_string();
    let label = "Auth Status".to_string();

    let auth_token = std::env::var("BRIDGE_AUTH_TOKEN").ok();
    match auth_token {
        Some(token) if !token.is_empty() => {
            let count = token.split(',').count();
            CheckResult {
                name,
                label,
                status: CheckStatus::Pass,
                message: format!(
                    "Auth enabled ({} token{} configured)",
                    count,
                    if count == 1 { "" } else { "s" }
                ),
            }
        }
        _ => CheckResult {
            name,
            label,
            status: CheckStatus::Warn,
            message: "Auth disabled. Set BRIDGE_AUTH_TOKEN for security.".to_string(),
        },
    }
}

async fn check_upstream() -> CheckResult {
    let name = "upstream-egress".to_string();
    let label = "Upstream Reachable".to_string();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    match client.get("https://opencode.ai/zen/v1/models").send().await {
        Ok(resp) if resp.status().is_success() || resp.status().is_client_error() => CheckResult {
            name,
            label,
            status: CheckStatus::Pass,
            message: "opencode.ai is reachable".to_string(),
        },
        Ok(resp) => CheckResult {
            name,
            label,
            status: CheckStatus::Warn,
            message: format!("opencode.ai returned {}", resp.status()),
        },
        Err(_) => CheckResult {
            name,
            label,
            status: CheckStatus::Warn,
            message:
                "opencode.ai unreachable (DNS/network). Bridge may still work with cached data."
                    .to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_doctor_summary_exit_code() {
        assert_eq!(DoctorSummary::AllPass.exit_code(), 0);
        assert_eq!(DoctorSummary::Warnings(2).exit_code(), 0);
        assert_eq!(DoctorSummary::Failures(1).exit_code(), 1);
    }

    #[test]
    fn test_check_result_display_has_icon() {
        let cr = CheckResult {
            name: "test".into(),
            label: "Test Check".into(),
            status: CheckStatus::Pass,
            message: "everything ok".into(),
        };
        let display = cr.to_string();
        assert!(display.contains('✓'));
    }

    #[test]
    fn test_check_result_status_partial_eq() {
        assert_eq!(CheckStatus::Pass, CheckStatus::Pass);
        assert_ne!(CheckStatus::Pass, CheckStatus::Warn);
    }

    #[test]
    fn test_check_result_warn_display() {
        let cr = CheckResult {
            name: "warn-test".into(),
            label: "Warn Check".into(),
            status: CheckStatus::Warn,
            message: "something degraded".into(),
        };
        let display = cr.to_string();
        assert!(display.contains('⚠'));
    }

    #[test]
    fn test_check_result_fail_display() {
        let cr = CheckResult {
            name: "fail-test".into(),
            label: "Fail Check".into(),
            status: CheckStatus::Fail,
            message: "something broken".into(),
        };
        let display = cr.to_string();
        assert!(display.contains('✗'));
    }

    #[test]
    fn test_doctor_report_all_pass_display() {
        let report = DoctorReport {
            checks: vec![CheckResult {
                name: "test".into(),
                label: "Test".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
            }],
            summary: DoctorSummary::AllPass,
        };
        let display = report.to_string();
        assert!(display.contains("All checks passed"));
    }

    #[test]
    fn test_doctor_report_warnings_display() {
        let report = DoctorReport {
            checks: vec![],
            summary: DoctorSummary::Warnings(1),
        };
        let display = report.to_string();
        assert!(display.contains("warning"));
    }

    #[test]
    fn test_doctor_report_failures_display() {
        let report = DoctorReport {
            checks: vec![],
            summary: DoctorSummary::Failures(2),
        };
        let display = report.to_string();
        assert!(display.contains("failure"));
    }

    #[test]
    fn test_check_result_serialize_pass() {
        let cr = CheckResult {
            name: "test".into(),
            label: "Test".into(),
            status: CheckStatus::Pass,
            message: "ok".into(),
        };
        let json = serde_json::to_string(&cr).unwrap();
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"status\":\"Pass\""));
    }

    #[test]
    fn test_check_result_serialize_fail() {
        let cr = CheckResult {
            name: "fail".into(),
            label: "Fail".into(),
            status: CheckStatus::Fail,
            message: "broken".into(),
        };
        let json = serde_json::to_string(&cr).unwrap();
        assert!(json.contains("\"status\":\"Fail\""));
    }

    #[test]
    fn test_doctor_report_serialize() {
        let report = DoctorReport {
            checks: vec![],
            summary: DoctorSummary::AllPass,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"summary\":\"AllPass\""));
    }
}
