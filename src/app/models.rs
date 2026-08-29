//! CLI command handlers for `list` and `model`.

use crate::application::models::resolve_model_profile;
use crate::application::prober::{fetch_and_probe_free_models, ModelStatus};
use crate::cli::{ListArgs, ModelArgs, ModelSetArgs, ModelSubcommand};
use crate::config::{BridgeConfig, CliOverrides};
use crate::output::OutputFormat;
use crate::presentation;
use comfy_table::presets::NOTHING;
use comfy_table::{Cell as CtCell, Color as CtColor, ContentArrangement, Table};
use reqwest::Client;
use std::fs;
use std::path::Path;
use yansi::Paint;

fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(*c);
    }
    result
}

pub async fn cmd_list(args: ListArgs, fmt: OutputFormat) {
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    let client = Client::new();
    let upstream_url = config.retry.upstream_base_url.as_str();

    if fmt == OutputFormat::Human && args.probe {
        eprintln!(
            "{}",
            "Probing live availability of OpenCode free models..."
                .cyan()
                .dim()
        );
    }

    let probed = fetch_and_probe_free_models(&client, upstream_url, args.probe).await;
    let active_model = config.model.as_deref().unwrap_or("opencode/mimo-v2.5-free");
    let active_clean = active_model
        .strip_prefix("opencode/")
        .unwrap_or(active_model);

    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&probed).unwrap_or_else(|_| "[]".to_string());
            println!("{json}");
        }
        OutputFormat::Quiet => {
            for m in &probed {
                println!("{}", m.id);
            }
        }
        OutputFormat::Human => {
            println!("\n{}", "◆ OpenCode Free Models".bold());
            println!("  {}\n", format!("Upstream: {upstream_url}").cyan().dim());

            let mut table = Table::new();
            table
                .load_preset(NOTHING)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_width(presentation::content_width() as u16);

            let mut headers = vec![
                "MODEL ID",
                "CONTEXT",
                "AUTO-COMPACT (80%)",
                "MAX OUT",
                "THINKING",
            ];
            if args.probe {
                headers.push("STATUS");
                headers.push("LATENCY");
            }
            table.set_header(
                headers
                    .into_iter()
                    .map(|h| CtCell::new(h).fg(CtColor::Cyan)),
            );

            for m in &probed {
                let is_active = m.id == active_model
                    || m.id == active_clean
                    || m.id.strip_prefix("opencode/").unwrap_or(&m.id) == active_clean;

                let clean_id = m.id.strip_prefix("opencode/").unwrap_or(&m.id);
                let id_display = if is_active {
                    format!("* {}", clean_id).green().bold().to_string()
                } else {
                    format!("  {}", clean_id)
                };

                let context_str = if m.context_window >= 1_000_000 {
                    format!("{}M", m.context_window / 1_000_000)
                } else {
                    format!("{}k", m.context_window / 1_000)
                };

                let autocompact_str = format!("{} tokens", format_number(m.auto_compact_window));
                let max_out_str = format_number(m.max_output_tokens);
                let thinking_str = if m.supports_thinking {
                    "✓ Yes".green().to_string()
                } else {
                    "- No".dim().to_string()
                };

                let mut row = vec![
                    CtCell::new(id_display),
                    CtCell::new(context_str),
                    CtCell::new(autocompact_str),
                    CtCell::new(max_out_str),
                    CtCell::new(thinking_str),
                ];

                if args.probe {
                    let status_cell = match m.status {
                        ModelStatus::Online => CtCell::new("ONLINE").fg(CtColor::Green),
                        ModelStatus::RateLimited => CtCell::new("BUSY/429").fg(CtColor::Yellow),
                        ModelStatus::Unavailable => CtCell::new("OFFLINE").fg(CtColor::Red),
                        ModelStatus::Unknown => CtCell::new("UNKNOWN").fg(CtColor::DarkGrey),
                    };
                    let latency_str = m
                        .latency_ms
                        .map(|l| format!("{l} ms"))
                        .unwrap_or_else(|| "-".to_string());
                    row.push(status_cell);
                    row.push(CtCell::new(latency_str));
                }

                table.add_row(row);
            }

            println!("{table}\n");
            println!("  {} Active model is marked with `*`", "ℹ".cyan().dim());
            println!(
                "  {} Run `opencode2api list --probe` to check live upstream availability",
                "ℹ".cyan().dim()
            );
            println!(
                "  {} Run `opencode2api model set <model>` to switch active model\n",
                "ℹ".cyan().dim()
            );
        }
    }
}

pub async fn cmd_model(args: ModelArgs, fmt: OutputFormat) {
    match args.command {
        Some(ModelSubcommand::List(list_args)) => cmd_list(list_args, fmt).await,
        Some(ModelSubcommand::Set(set_args)) => cmd_model_set(set_args, fmt),
        None | Some(ModelSubcommand::Status) => cmd_model_status(fmt),
    }
}

fn cmd_model_set(args: ModelSetArgs, fmt: OutputFormat) {
    let raw_model = args.model.trim();
    let profile = resolve_model_profile(raw_model);

    let config_path = args
        .config
        .or_else(|| std::env::var("BRIDGE_CONFIG_PATH").ok())
        .unwrap_or_else(|| "opencode2api.toml".to_string());

    let path = Path::new(&config_path);
    let mut toml_doc = if path.exists() {
        fs::read_to_string(path)
            .unwrap_or_default()
            .parse::<toml_edit::DocumentMut>()
            .unwrap_or_default()
    } else {
        toml_edit::DocumentMut::new()
    };

    toml_doc["model"] = toml_edit::value(raw_model);
    let _ = fs::write(path, toml_doc.to_string());

    // Also update .env if it exists in the current directory
    let env_path = Path::new(".env");
    if env_path.exists() {
        if let Ok(content) = fs::read_to_string(env_path) {
            let mut updated_lines = Vec::new();
            let mut found = false;
            for line in content.lines() {
                if line.starts_with("OPENCODE_MODEL=") {
                    updated_lines.push(format!("OPENCODE_MODEL={raw_model}"));
                    found = true;
                } else {
                    updated_lines.push(line.to_string());
                }
            }
            if !found {
                updated_lines.push(format!("OPENCODE_MODEL={raw_model}"));
            }
            let _ = fs::write(env_path, updated_lines.join("\n") + "\n");
        }
    }

    match fmt {
        OutputFormat::Json => {
            let res = serde_json::json!({
                "status": "ok",
                "model": raw_model,
                "context_window": profile.context_window,
                "auto_compact_window": profile.auto_compact_window(),
                "max_output_tokens": profile.max_output_tokens,
                "supports_thinking": profile.supports_thinking,
                "config_file": config_path
            });
            println!("{res}");
        }
        OutputFormat::Quiet => {
            println!("{raw_model}");
        }
        OutputFormat::Human => {
            println!("\n{}", "✓ Model configuration updated".green().bold());
            println!("  Model:               {}", raw_model.cyan().bold());
            println!(
                "  Context Window:      {} tokens",
                format_number(profile.context_window)
            );
            println!(
                "  Auto-Compact Window: {} ({} tokens)",
                "80%".yellow().bold(),
                format_number(profile.auto_compact_window())
            );
            println!(
                "  Max Output Tokens:   {} tokens",
                format_number(profile.max_output_tokens)
            );
            println!(
                "  Thinking Support:    {}",
                if profile.supports_thinking {
                    "Enabled".green()
                } else {
                    "Disabled".dim()
                }
            );
            println!("  Saved to:            {}\n", config_path.dim());
        }
    }
}

fn cmd_model_status(fmt: OutputFormat) {
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    let active_model = config.model.as_deref().unwrap_or("opencode/mimo-v2.5-free");
    let profile = resolve_model_profile(active_model);

    match fmt {
        OutputFormat::Json => {
            let res = serde_json::json!({
                "model": active_model,
                "label": profile.label,
                "provider": profile.provider,
                "context_window": profile.context_window,
                "auto_compact_window": profile.auto_compact_window(),
                "max_output_tokens": profile.max_output_tokens,
                "supports_thinking": profile.supports_thinking,
                "anthropic_alias": profile.anthropic_alias
            });
            println!("{res}");
        }
        OutputFormat::Quiet => {
            println!("{active_model}");
        }
        OutputFormat::Human => {
            println!("\n{}", "◆ Current Model Configuration".bold());
            println!("  Active Model:        {}", active_model.green().bold());
            println!("  Label:               {}", profile.label);
            println!("  Provider:            {}", profile.provider);
            println!(
                "  Context Window:      {} tokens",
                format_number(profile.context_window)
            );
            println!(
                "  Auto-Compact Window: {} ({} tokens)",
                "80%".yellow().bold(),
                format_number(profile.auto_compact_window())
            );
            println!(
                "  Max Output Tokens:   {} tokens",
                format_number(profile.max_output_tokens)
            );
            println!(
                "  Thinking Support:    {}",
                if profile.supports_thinking {
                    "Enabled".green()
                } else {
                    "Disabled".dim()
                }
            );
            println!(
                "  Anthropic Alias:     {}\n",
                profile.anthropic_alias.cyan()
            );
        }
    }
}
