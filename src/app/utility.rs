//! Standalone utility commands that do not own server lifecycle.

use super::view::{cmd_print_env, shell_export_lines};
use crate::cli::{self, CompletionArgs, InitArgs, UpdateArgs};
use crate::config;
use crate::doctor;
use crate::output::OutputFormat;
use clap::CommandFactory;
use clap_complete::generate;
use yansi::Paint;

pub(super) async fn cmd_doctor(fmt: OutputFormat) {
    let report = doctor::run_diagnostics().await;
    match fmt {
        OutputFormat::Json => {
            if let Ok(s) = serde_json::to_string_pretty(&report) {
                println!("{s}");
            }
        }
        OutputFormat::Quiet => {
            let mut warnings = 0;
            let mut failures = 0;
            for c in &report.checks {
                match c.status {
                    doctor::CheckStatus::Warn => warnings += 1,
                    doctor::CheckStatus::Fail => failures += 1,
                    doctor::CheckStatus::Pass => {}
                }
            }
            println!("warnings={} failures={}", warnings, failures);
        }
        _ => {
            println!("{}", report);
        }
    }
    std::process::exit(report.summary.exit_code());
}

pub(super) fn cmd_completion(args: CompletionArgs) {
    let mut cmd = cli::Cli::command();
    let name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, name, &mut std::io::stdout());
}

pub(super) async fn cmd_update(args: UpdateArgs) {
    use crate::update::{self, fetch_latest_release, find_matching_asset, has_update};

    let client = reqwest::Client::builder()
        .user_agent(concat!("opencode2api/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();

    match fetch_latest_release(&client).await {
        Ok(release) => {
            let current = update::current_version();
            let available = has_update(current, &release);

            if args.check {
                // Just check mode — don't download
                if available {
                    eprintln!(
                        "{} Update available: {} → {}",
                        "↑".green().bold(),
                        current,
                        release.version
                    );
                    if !release.body.is_empty() {
                        eprintln!("\nRelease notes:\n{}", release.body);
                    }
                } else {
                    eprintln!("{} You are up-to-date ({})", "✓".green().bold(), current);
                }
                return;
            }

            if !available && !args.force {
                eprintln!(
                    "{} You are up-to-date ({}) — use --force to reinstall",
                    "✓".green().bold(),
                    current
                );
                return;
            }

            // Find matching asset for this platform
            let asset = match find_matching_asset(&release) {
                Some(a) => a,
                None => {
                    eprintln!(
                        "{} No binary available for {}/{}",
                        "✗".red().bold(),
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    );
                    eprintln!("   Supported platforms: linux (amd64, arm64), macOS (amd64, arm64)");
                    std::process::exit(1);
                }
            };

            eprintln!(
                "{} Updating {} → {} (downloading {})...",
                "↓".cyan().bold(),
                current,
                release.version,
                asset.name
            );

            match update::apply_update(&client, asset).await {
                Ok(path) => {
                    eprintln!(
                        "{} Updated to {} — binary replaced at {}",
                        "✓".green().bold(),
                        release.version,
                        path.display()
                    );
                    eprintln!("   Restart the bridge if it was running.");
                }
                Err(e) => {
                    eprintln!("{} Update failed: {}", "✗".red().bold(), e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{} Failed to check for updates: {}", "✗".red().bold(), e);
            std::process::exit(1);
        }
    }
}

pub(super) async fn cmd_init(args: InitArgs) {
    use crate::init::generate_config;

    let path = std::path::Path::new(&args.output);
    match generate_config(path, args.force).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{} Init failed: {}", "✗".red().bold(), e);
            std::process::exit(1);
        }
    }
}

pub(super) fn cmd_env(fmt: OutputFormat) {
    if fmt == OutputFormat::Quiet {
        for line in shell_export_lines() {
            println!("{}", line);
        }
        return;
    }

    if fmt == OutputFormat::Json {
        #[derive(serde::Serialize)]
        struct EnvInfo {
            anthropic_api_key: String,
            anthropic_base_url: String,
            opencode_model: Option<String>,
        }
        let port = std::env::var("BRIDGE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(config::DEFAULT_BRIDGE_PORT);
        let model = std::env::var("OPENCODE_MODEL").ok();
        let info = EnvInfo {
            anthropic_api_key: "opencode-bridge".to_string(),
            anthropic_base_url: format!("http://127.0.0.1:{}/v1", port),
            opencode_model: model,
        };
        if let Ok(s) = serde_json::to_string_pretty(&info) {
            println!("{s}");
        }
        return;
    }

    cmd_print_env();
}
