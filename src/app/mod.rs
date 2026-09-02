//! CLI application orchestration.
//!
//! This module owns command dispatch only. Command implementations live in
//! focused submodules so the binary entry point stays trivial and testable.

mod dashboard;
mod models;
mod proxy;
mod server;
mod utility;
mod view;

use crate::cli::{self, Command};
use crate::command;
use crate::config::{BridgeConfig, CliOverrides};
use crate::output::{setup_color, OutputFormat};
use crate::supervisor::SupervisorStatus;
use clap::Parser;
use yansi::Paint;

pub async fn run_cli() {
    // Load environment variables from the working directory or from a `.env`
    // beside/above the executable. This keeps daemon launches from `$HOME`
    // consistent with direct launches from the repository.
    let _ = crate::config::load_dotenv();

    let cli = cli::Cli::parse();

    if cli.verbose && std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "opencode2api=debug");
    }

    // Initialize color support BEFORE any output
    setup_color(&cli.color);

    // Determine output format from global flags
    let fmt = if cli.json {
        OutputFormat::Json
    } else if cli.quiet {
        OutputFormat::Quiet
    } else {
        OutputFormat::Human
    };
    let global_config = cli.config.clone();

    match cli.command {
        // New server subcommand group
        Some(Command::Server(cmd)) => server::cmd_server(cmd, fmt).await,

        // New dashboard subcommand group
        Some(Command::Dashboard(cmd)) => dashboard::cmd_dashboard(cmd, fmt).await,

        // New commands
        Some(Command::Doctor) => utility::cmd_doctor(fmt).await,
        Some(Command::Provider(args)) => match args.command {
            Some(cli::ProviderSubcommand::Opencode(args)) => {
                models::cmd_provider(
                    cli::ProviderArgs {
                        command: Some(cli::ProviderSubcommand::Opencode(args)),
                    },
                    fmt,
                )
                .await
            }
            Some(cli::ProviderSubcommand::Api(args)) => {
                models::cmd_provider(
                    cli::ProviderArgs {
                        command: Some(cli::ProviderSubcommand::Api(args)),
                    },
                    fmt,
                )
                .await
            }
            Some(cli::ProviderSubcommand::Models(args)) => {
                models::cmd_provider(
                    cli::ProviderArgs {
                        command: Some(cli::ProviderSubcommand::Models(args)),
                    },
                    fmt,
                )
                .await
            }
            Some(cli::ProviderSubcommand::Status(args)) => {
                models::cmd_provider(
                    cli::ProviderArgs {
                        command: Some(cli::ProviderSubcommand::Status(args)),
                    },
                    fmt,
                )
                .await
            }
            Some(other) => command::provider::run_provider(other, global_config.clone(), fmt).await,
            None => models::cmd_provider(cli::ProviderArgs { command: None }, fmt).await,
        },
        Some(Command::Credential(args)) => {
            command::provider::run_credentials(args.command, global_config.clone(), fmt).await
        }
        Some(Command::Alias(args)) => {
            command::provider::run_aliases(args.command, global_config.clone(), fmt).await
        }
        Some(Command::Pool(args)) => {
            command::provider::run_pools(args.command, global_config.clone(), fmt).await
        }
        Some(Command::Route(args)) => {
            command::provider::run_route(args.command, global_config.clone(), fmt).await
        }
        Some(Command::Health(args)) => {
            command::provider::run_health(args.provider, global_config.clone(), fmt, args.watch)
                .await
        }
        Some(Command::Config(args)) => {
            command::config::run(args.command, global_config.clone(), fmt).await
        }
        Some(Command::List(args)) => models::cmd_list(args, fmt).await,
        Some(Command::Model(args)) => {
            let model_command = args.command;
            match model_command {
                Some(cli::ModelSubcommand::List(args)) => {
                    command::provider::run_model(
                        cli::ModelSubcommand::List(args),
                        global_config.clone(),
                        fmt,
                    )
                    .await
                }
                Some(cli::ModelSubcommand::Show(args)) => {
                    command::provider::run_model(
                        cli::ModelSubcommand::Show(args),
                        global_config.clone(),
                        fmt,
                    )
                    .await
                }
                Some(cli::ModelSubcommand::Discover(args)) => {
                    command::provider::run_model(
                        cli::ModelSubcommand::Discover(args),
                        global_config.clone(),
                        fmt,
                    )
                    .await
                }
                Some(cli::ModelSubcommand::Verify(args)) => {
                    command::provider::run_model(
                        cli::ModelSubcommand::Verify(args),
                        global_config.clone(),
                        fmt,
                    )
                    .await
                }
                Some(cli::ModelSubcommand::Set(args)) => {
                    models::cmd_model(
                        cli::ModelArgs {
                            command: Some(cli::ModelSubcommand::Set(args)),
                        },
                        fmt,
                    )
                    .await
                }
                Some(cli::ModelSubcommand::Status) => {
                    models::cmd_model(
                        cli::ModelArgs {
                            command: Some(cli::ModelSubcommand::Status),
                        },
                        fmt,
                    )
                    .await
                }
                None => models::cmd_model(cli::ModelArgs { command: None }, fmt).await,
            }
        }
        Some(Command::Upstream(args)) => models::cmd_upstream(args, fmt).await,
        Some(Command::Completion(args)) => utility::cmd_completion(args, fmt),
        Some(Command::Update(args)) => utility::cmd_update(args, fmt).await,
        Some(Command::Init(args)) => utility::cmd_init(args, fmt).await,
        Some(Command::Env) => utility::cmd_env(fmt),
        Some(Command::Set(cmd)) => utility::cmd_set(cmd, fmt),
        Some(Command::Shell(cmd)) => utility::cmd_shell(cmd, fmt),
        Some(Command::ApiKey(cmd)) => utility::cmd_api_key(cmd, fmt),

        // Proxy group (unchanged, but uses fmt)
        Some(Command::Proxy(cmd)) => proxy::cmd_proxy(cmd, fmt).await,

        // Legacy aliases (backward compatible) — show deprecation hint once
        Some(Command::Serve(args)) => {
            eprintln!(
                "{} `serve` is deprecated, use `server start -f` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_serve_legacy(args).await
        }
        Some(Command::Start(args)) => {
            eprintln!(
                "{} `start` is deprecated, use `server start` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_start_legacy(args, fmt).await
        }
        Some(Command::Status(args)) => {
            eprintln!(
                "{} `status` is deprecated, use `server status` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_status_legacy(args, fmt).await
        }
        Some(Command::Stop(args)) => {
            eprintln!(
                "{} `stop` is deprecated, use `server stop` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_stop_legacy(args)
        }
        Some(Command::Restart) => {
            eprintln!(
                "{} `restart` is deprecated, use `server restart` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_restart_legacy(fmt).await
        }
        Some(Command::Logs) => {
            eprintln!(
                "{} `logs` is deprecated, use `server logs` instead",
                "ℹ".cyan().dim()
            );
            server::cmd_logs_legacy(fmt)
        }

        // Default: the bridge lifecycle stays explicit. A bare invocation only
        // launches Claude Code after confirming the configured bridge is running.
        None => launch_claude_code(cli.continue_session, cli.resume.as_deref()),
    }
}

fn apply_claude_default_model(mut resolved: BridgeConfig) -> BridgeConfig {
    if resolved
        .model
        .as_deref()
        .is_none_or(|model| model.trim().is_empty())
    {
        if crate::application::prober::is_opencode_upstream(&resolved.retry.upstream_base_url) {
            resolved.model = Some("opencode/mimo-v2.5-free".to_string());
        } else {
            resolved.model = Some("glm-5.3-flash".to_string());
        }
    }
    resolved
}

fn launch_claude_code(continue_session: bool, resume: Option<&str>) {
    let resolved =
        apply_claude_default_model(BridgeConfig::from_env_and_cli(CliOverrides::default()));

    let supervisor = server::resolve_runtime_for_start(&cli::ServerStartArgs {
        model: resolved.model.clone(),
        ..Default::default()
    });

    match supervisor.status() {
        Ok(SupervisorStatus::Running { .. }) => {}
        Ok(SupervisorStatus::Stopped) => {
            eprintln!("opencode2api: bridge is not running, starting background daemon...");
            if let Err(error) = supervisor.start() {
                eprintln!("opencode2api: failed to start bridge daemon: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("opencode2api: could not determine bridge status: {error}");
            std::process::exit(1);
        }
    }

    let target_alias = crate::application::integration::client_model_alias(&resolved);
    let context_window = crate::application::integration::client_context_window(&resolved);

    match crate::infrastructure::process::run_foreground(
        "claude",
        claude_launch_args(
            continue_session,
            resume,
            Some(&target_alias),
            Some(context_window),
        ),
        crate::application::integration::process_environment(&resolved),
    ) {
        Ok(status) if status.success() => {}
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("opencode2api: Claude Code is not installed or not available in PATH.");
            std::process::exit(127);
        }
        Err(error) => {
            eprintln!("opencode2api: failed to launch Claude Code: {error}");
            std::process::exit(1);
        }
    }
}

fn claude_launch_args(
    continue_session: bool,
    resume: Option<&str>,
    model: Option<&str>,
    context_window: Option<usize>,
) -> Vec<String> {
    let mut args = vec![
        "--permission-mode".to_string(),
        "bypassPermissions".to_string(),
    ];
    if let Some(m) = model {
        if !m.is_empty() {
            args.push("--model".to_string());
            args.push(m.to_string());
        }
    }
    // Current Claude Code exposes a first-class token flag. Supplying it in
    // addition to the environment contract makes `/context` deterministic;
    // skip windows below the CLI's documented 100k minimum and let the
    // environment variables carry those legacy profiles.
    if let Some(context_window) = context_window {
        let auto_compact = crate::provider::types::auto_compact_window(context_window);
        if (100_000..=1_000_000).contains(&auto_compact) {
            args.push("--autocompact".to_string());
            args.push(auto_compact.to_string());
        }
    }
    if continue_session {
        args.push("--continue".to_string());
    } else if let Some(session) = resume {
        args.push("--resume".to_string());
        if !session.is_empty() {
            args.push(session.to_string());
        }
    }
    args
}

#[cfg(test)]
mod launcher_tests {
    use super::{apply_claude_default_model, claude_launch_args};
    use crate::config::BridgeConfig;

    #[test]
    fn launcher_default_model_is_mimo_but_explicit_models_are_preserved() {
        let defaulted = apply_claude_default_model(BridgeConfig::default());
        assert_eq!(defaulted.model.as_deref(), Some("opencode/mimo-v2.5-free"));

        let explicit = apply_claude_default_model(BridgeConfig {
            model: Some("opencode/x-preview-f-free".to_string()),
            ..Default::default()
        });
        assert_eq!(
            explicit.model.as_deref(),
            Some("opencode/x-preview-f-free"),
            "explicit model selection must never be silently replaced"
        );
    }

    #[test]
    fn bare_launcher_defaults_to_bypass_permissions() {
        assert_eq!(
            claude_launch_args(false, None, None, None),
            ["--permission-mode", "bypassPermissions"]
        );
        assert_eq!(
            claude_launch_args(false, None, Some("claude-opus-5"), None),
            [
                "--permission-mode",
                "bypassPermissions",
                "--model",
                "claude-opus-5"
            ]
        );
    }

    #[test]
    fn launcher_supports_continue_and_resume() {
        assert_eq!(
            claude_launch_args(true, None, None, None),
            ["--permission-mode", "bypassPermissions", "--continue"]
        );
        assert_eq!(
            claude_launch_args(false, Some(""), None, None),
            ["--permission-mode", "bypassPermissions", "--resume"]
        );
        assert_eq!(
            claude_launch_args(false, Some("session-123"), Some("claude-opus-5"), None),
            [
                "--permission-mode",
                "bypassPermissions",
                "--model",
                "claude-opus-5",
                "--resume",
                "session-123",
            ]
        );
    }

    #[test]
    fn launcher_passes_fixed_compaction_for_supported_context_windows() {
        assert_eq!(
            claude_launch_args(false, None, Some("sonnet[1m]"), Some(1_000_000)),
            [
                "--permission-mode",
                "bypassPermissions",
                "--model",
                "sonnet[1m]",
                "--autocompact",
                "800000"
            ]
        );
        assert_eq!(
            claude_launch_args(false, None, Some("small"), Some(64_000)),
            ["--permission-mode", "bypassPermissions", "--model", "small"]
        );
    }
}
