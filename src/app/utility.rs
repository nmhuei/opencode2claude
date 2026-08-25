//! Standalone utility commands that do not own server lifecycle.

use super::view::{
    claude_code_base_url, cmd_print_env, print_brand_header, print_error, print_section,
    print_success, print_tip, print_warning, shell_export_lines,
};
use crate::cli::{
    self, ApiKeyCommand, ApiKeyGenerateArgs, CompletionArgs, InitArgs, SetCommand, ShellCommand,
    UpdateArgs,
};
use crate::config;
use crate::doctor;
use crate::output::OutputFormat;
use crate::presentation;
use clap::CommandFactory;
use clap_complete::generate;
use std::io::Write;
use yansi::Paint;

pub(super) async fn cmd_doctor(fmt: OutputFormat) {
    let report = doctor::run_diagnostics().await;
    match fmt {
        OutputFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => println!(
                "{}",
                serde_json::json!({"status":"error","message":error.to_string()})
            ),
        },
        OutputFormat::Quiet => {
            let warnings = report
                .checks
                .iter()
                .filter(|check| check.status == doctor::CheckStatus::Warn)
                .count();
            let failures = report
                .checks
                .iter()
                .filter(|check| check.status == doctor::CheckStatus::Fail)
                .count();
            println!("warnings={warnings} failures={failures}");
        }
        OutputFormat::Human => println!("{report}"),
    }
    std::process::exit(report.summary.exit_code());
}

pub(super) fn cmd_completion(args: CompletionArgs, fmt: OutputFormat) {
    let mut command = cli::Cli::command();
    let name = command.get_name().to_string();
    let mut buffer = Vec::new();
    generate(args.shell, &mut command, name, &mut buffer);
    let script = String::from_utf8_lossy(&buffer);

    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "shell": format!("{:?}", args.shell).to_ascii_lowercase(),
                "script": script,
            })
        ),
        OutputFormat::Human | OutputFormat::Quiet => {
            print!("{script}");
            let _ = std::io::stdout().flush();
        }
    }
}

pub(super) async fn cmd_update(args: UpdateArgs, fmt: OutputFormat) {
    use crate::update::{self, fetch_latest_release, find_matching_asset, has_update};

    let client = reqwest::Client::builder()
        .user_agent(concat!("opencode2api/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();

    let release = match fetch_latest_release(&client).await {
        Ok(release) => release,
        Err(error) => {
            emit_error(
                fmt,
                "update-check",
                "Could not check for updates",
                &error.to_string(),
                &["Check your network connection and release repository configuration."],
            );
            std::process::exit(1);
        }
    };

    let current = update::current_version();
    let available = has_update(current, &release);

    if args.check {
        match fmt {
            OutputFormat::Json => println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "current_version": current,
                    "latest_version": release.version,
                    "update_available": available,
                })
            ),
            OutputFormat::Quiet => println!("{}", release.version),
            OutputFormat::Human => {
                print_brand_header("Update check", "CLI release channel");
                if available {
                    print_warning(&format!(
                        "Update available: {current} → {}",
                        release.version
                    ));
                    print_tip("Run `opencode2api update` to install it.");
                } else {
                    print_success(&format!("Up to date ({current})"));
                }
                println!();
            }
        }
        return;
    }

    if !available && !args.force {
        let shell_hook = crate::application::shell_integration::install_hook("auto", None).ok();
        match fmt {
            OutputFormat::Json => println!(
                "{}",
                serde_json::json!({
                    "status": "up-to-date",
                    "version": current,
                    "updated": false,
                    "shell_hook": shell_hook.as_ref().map(|result| result.path.display().to_string()),
                })
            ),
            OutputFormat::Quiet => println!("{current}"),
            OutputFormat::Human => {
                print_brand_header("CLI update", "Release installer");
                print_success(&format!("Already up to date ({current})"));
                print_tip("Use `--force` to reinstall the current release.");
                println!();
            }
        }
        return;
    }

    let asset = match find_matching_asset(&release) {
        Some(asset) => asset,
        None => {
            let cause = format!(
                "No release binary is available for {}/{}.",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            emit_error(
                fmt,
                "update",
                "Unsupported update platform",
                &cause,
                &["Supported release targets: Linux amd64 and Linux arm64."],
            );
            std::process::exit(1);
        }
    };

    if fmt == OutputFormat::Human {
        print_brand_header("CLI update", "Release installer");
        print_tip(&format!(
            "Installing {current} → {} from {}.",
            release.version, asset.name
        ));
        println!();
    }

    match update::apply_update(&client, asset).await {
        Ok(path) => {
            let shell_hook = crate::application::shell_integration::install_hook("auto", None).ok();
            match fmt {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::json!({
                        "status": "updated",
                        "from_version": current,
                        "to_version": release.version,
                        "path": path,
                        "restart_required": true,
                        "shell_hook": shell_hook.as_ref().map(|result| result.path.display().to_string()),
                    })
                ),
                OutputFormat::Quiet => println!("{}", release.version),
                OutputFormat::Human => {
                    print_success(&format!("Updated to {}", release.version));
                    println!(
                        "{}",
                        presentation::facts(&[("Binary", path.display().to_string())])
                    );
                    if let Some(result) = shell_hook {
                        print_tip(&format!(
                            "Shell integration is installed at {}.",
                            result.path.display()
                        ));
                    }
                    println!();
                    print_tip("Restart the gateway if it is currently running.");
                    println!();
                }
            }
        }
        Err(error) => {
            emit_error(
                fmt,
                "update",
                "Could not update the CLI",
                &error.to_string(),
                &[],
            );
            std::process::exit(1);
        }
    }
}

pub(super) async fn cmd_init(args: InitArgs, fmt: OutputFormat) {
    use crate::init::generate_config;

    let path = std::path::Path::new(&args.output);
    match generate_config(path, args.force).await {
        Ok(()) => match fmt {
            OutputFormat::Json => println!(
                "{}",
                serde_json::json!({
                    "status": "created",
                    "path": path,
                    "overwritten": args.force,
                })
            ),
            OutputFormat::Quiet => println!("{}", path.display()),
            OutputFormat::Human => {
                print_brand_header("Configuration created", "Starter TOML template");
                print_success("Configuration template written");
                println!(
                    "{}",
                    presentation::facts(&[("Path", path.display().to_string())])
                );
                println!();
                print_tip(&format!(
                    "Edit the file, then run `opencode2api server start -c {}`.",
                    path.display()
                ));
                println!();
            }
        },
        Err(error) => {
            emit_error(
                fmt,
                "init",
                "Could not create the configuration",
                &error.to_string(),
                &["Use `--force` only when replacing the existing file is intended."],
            );
            std::process::exit(1);
        }
    }
}

pub(super) fn cmd_env(fmt: OutputFormat) {
    let resolved = config::BridgeConfig::from_env_and_cli(config::CliOverrides::default());
    match fmt {
        OutputFormat::Quiet => {
            for line in shell_export_lines(&resolved) {
                println!("{line}");
            }
        }
        OutputFormat::Json => {
            let base_url = claude_code_base_url(&resolved);
            println!(
                "{}",
                serde_json::json!({
                    "anthropic_api_key": "<redacted>",
                    "anthropic_base_url": base_url,
                    "openai_api_key": "<redacted>",
                    "openai_base_url": format!("{base_url}/v1"),
                    "opencode_model": resolved.model,
                    "auth_enabled": resolved.auth_enabled(),
                })
            );
        }
        OutputFormat::Human => cmd_print_env(&resolved),
    }
}

pub(super) fn cmd_set(command: SetCommand, fmt: OutputFormat) {
    match command {
        SetCommand::Env => {
            match fmt {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::json!({
                        "status": "shell-hook-required",
                        "command": "opencode2api set env",
                        "install": "opencode2api shell install",
                        "fallback": "eval \"$(opencode2api --quiet env)\"",
                    })
                ),
                OutputFormat::Quiet => {
                    eprintln!("shell integration is not loaded; run: opencode2api shell install");
                }
                OutputFormat::Human => {
                    print_error(
                        "Shell integration is not loaded",
                        "A child process cannot change its parent shell environment. Install the managed hook once, then open a new shell or source your rc file.",
                        &[
                            "opencode2api shell install",
                            "eval \"$(opencode2api --quiet env)\"",
                        ],
                    );
                }
            }
            std::process::exit(1);
        }
    }
}

pub(super) fn cmd_shell(command: ShellCommand, fmt: OutputFormat) {
    use crate::application::shell_integration;
    use std::path::Path;

    match command {
        ShellCommand::Hook(args) => {
            if let Err(error) = shell_integration::resolve_shell(&args.shell.to_string()) {
                emit_error(
                    fmt,
                    "shell-hook",
                    "Could not resolve shell",
                    &error.to_string(),
                    &[],
                );
                std::process::exit(1);
            }
            println!("{}", shell_integration::render_hook());
        }
        ShellCommand::Install(args) => {
            let rc = args.rc.as_deref().map(Path::new);
            match shell_integration::install_hook(&args.shell.to_string(), rc) {
                Ok(result) => match fmt {
                    OutputFormat::Json => println!(
                        "{}",
                        serde_json::json!({
                            "status": "ok",
                            "action": "installed",
                            "shell": result.shell.name(),
                            "path": result.path,
                            "changed": result.changed,
                        })
                    ),
                    OutputFormat::Quiet => println!("{}", result.path.display()),
                    OutputFormat::Human => {
                        print_brand_header("Shell integration", "Parent-shell environment hook");
                        if result.changed {
                            print_success("Shell hook installed");
                        } else {
                            print_success("Shell hook already up to date");
                        }
                        println!(
                            "{}",
                            presentation::facts(&[
                                ("Shell", result.shell.name().to_string()),
                                ("RC file", result.path.display().to_string()),
                            ])
                        );
                        println!();
                        print_tip(&format!(
                            "Open a new terminal or run `source {}` once; then `opencode2api set env` updates that terminal session.",
                            result.path.display()
                        ));
                        println!();
                    }
                },
                Err(error) => {
                    emit_error(
                        fmt,
                        "shell-install",
                        "Could not install shell integration",
                        &error.to_string(),
                        &["Use --shell bash or --shell zsh, or pass --rc PATH explicitly."],
                    );
                    std::process::exit(1);
                }
            }
        }
        ShellCommand::Uninstall(args) => {
            let rc = args.rc.as_deref().map(Path::new);
            match shell_integration::uninstall_hook(&args.shell.to_string(), rc) {
                Ok(result) => match fmt {
                    OutputFormat::Json => println!(
                        "{}",
                        serde_json::json!({
                            "status": "ok",
                            "action": "uninstalled",
                            "shell": result.shell.name(),
                            "path": result.path,
                            "changed": result.changed,
                        })
                    ),
                    OutputFormat::Quiet => println!("{}", result.path.display()),
                    OutputFormat::Human => {
                        print_brand_header("Shell integration", "Parent-shell environment hook");
                        if result.changed {
                            print_success("Shell hook removed");
                        } else {
                            print_success("No managed shell hook was present");
                        }
                        println!(
                            "{}",
                            presentation::facts(&[("RC file", result.path.display().to_string())])
                        );
                        println!();
                    }
                },
                Err(error) => {
                    emit_error(
                        fmt,
                        "shell-uninstall",
                        "Could not remove shell integration",
                        &error.to_string(),
                        &[],
                    );
                    std::process::exit(1);
                }
            }
        }
    }
}

pub(super) fn cmd_api_key(command: ApiKeyCommand, fmt: OutputFormat) {
    match command {
        ApiKeyCommand::Generate(args) => cmd_generate_api_key(args, fmt),
    }
}

fn cmd_generate_api_key(args: ApiKeyGenerateArgs, fmt: OutputFormat) {
    let keys = match crate::api_key::generate_api_keys(args.count, args.bytes, &args.prefix) {
        Ok(keys) => keys,
        Err(error) => {
            emit_error(
                fmt,
                "api-key-generate",
                "Could not generate API keys",
                &error.to_string(),
                &[],
            );
            std::process::exit(1);
        }
    };

    let resolved = config::BridgeConfig::from_env_and_cli(config::CliOverrides {
        config_path: args.config.clone(),
        ..Default::default()
    });
    let config_path = args
        .config
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| resolved.management.config_path.clone());

    if args.save {
        if let Err(error) = crate::api_key::save_auth_tokens(&config_path, &keys, args.replace) {
            emit_error(
                fmt,
                "api-key-save",
                "Could not save API keys",
                &error.to_string(),
                &[],
            );
            std::process::exit(1);
        }
    }

    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "keys": keys,
                "saved": args.save,
                "config_path": args.save.then(|| config_path.display().to_string()),
                "restart_required": args.save,
                "authorization_header": "Authorization: Bearer <key>",
            })
        ),
        OutputFormat::Quiet => {
            for key in &keys {
                println!("{key}");
            }
        }
        OutputFormat::Human => {
            print_brand_header("API key created", "Bridge authentication credential");
            print_success(&format!(
                "Generated {} API key{}",
                keys.len(),
                if keys.len() == 1 { "" } else { "s" }
            ));
            print_section(if keys.len() == 1 {
                "API key"
            } else {
                "API keys"
            });
            for key in &keys {
                println!(
                    "{}{}",
                    " ".repeat(presentation::INDENT * 2),
                    key.cyan().bold()
                );
            }

            if args.save {
                println!();
                println!(
                    "{}",
                    presentation::facts(&[("Saved to", config_path.display().to_string())])
                );
                print_tip("Restart the gateway to activate the updated credentials.");
            } else {
                println!();
                print_warning("Store these credentials now; generated values are not recoverable.");
                print_tip("Use `--save` to append them to the active TOML configuration.");
            }
            println!();
        }
    }
}

fn emit_error(fmt: OutputFormat, operation: &str, title: &str, cause: &str, suggestions: &[&str]) {
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "status": "error",
                "operation": operation,
                "message": cause,
            })
        ),
        OutputFormat::Quiet => eprintln!("error"),
        OutputFormat::Human => print_error(title, cause, suggestions),
    }
}
