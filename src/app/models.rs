use crate::application::models::{
    model_default_output_tokens, model_pricing, resolve_model_profile,
};
use crate::application::prober::{
    all_models_rejected_for_auth, check_upstream_health, fetch_and_probe_models, ModelStatus,
    ProbedModel,
};
use crate::cli::{
    ListArgs, ModelArgs, ModelSetArgs, ModelSubcommand, ProviderApiArgs, ProviderArgs,
    ProviderOpenCodeArgs, ProviderSubcommand, UpstreamArgs, UpstreamSetArgs, UpstreamSubcommand,
};
use crate::config::{BridgeConfig, CliOverrides};
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use crate::output::OutputFormat;
use crate::presentation;
use crate::runtime::RuntimePaths;
use comfy_table::presets::NOTHING;
use comfy_table::{Cell as CtCell, Color as CtColor, ContentArrangement, Table};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
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

#[derive(Debug, Serialize, Deserialize)]
struct ModelProbeCache {
    upstream_base_url: String,
    probed_at_unix_secs: u64,
    models: Vec<ProbedModel>,
}

fn read_model_probe_cache(path: &Path, upstream_base_url: &str) -> Option<Vec<ProbedModel>> {
    let bytes = AtomicFileStore.read(path).ok()?;
    let cache = serde_json::from_slice::<ModelProbeCache>(&bytes).ok()?;
    (cache.upstream_base_url == upstream_base_url).then_some(cache.models)
}

fn write_model_probe_cache(
    path: &Path,
    upstream_base_url: &str,
    models: &[ProbedModel],
) -> Result<(), String> {
    let cache = ModelProbeCache {
        upstream_base_url: upstream_base_url.to_string(),
        probed_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        models: models.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&cache)
        .map_err(|error| format!("failed to serialize model probe cache: {error}"))?;
    AtomicFileStore
        .atomic_write(path, &bytes, false)
        .map_err(|error| {
            format!(
                "failed to persist model probe cache {}: {error}",
                path.display()
            )
        })
}

fn resolved_config_path(explicit: Option<String>) -> PathBuf {
    BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: explicit,
        ..Default::default()
    })
    .management
    .config_path
}

fn load_editable_config(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    if !path.exists() {
        return Ok(toml_edit::DocumentMut::new());
    }
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;
    raw.parse::<toml_edit::DocumentMut>().map_err(|error| {
        format!(
            "refusing to overwrite invalid TOML {}: {error}",
            path.display()
        )
    })
}

fn atomic_write_sensitive(path: &Path, content: &str) -> Result<(), String> {
    AtomicFileStore
        .atomic_write(path, content.as_bytes(), true)
        .map_err(|error| format!("failed to persist {}: {error}", path.display()))
}

fn exit_cli_error(fmt: OutputFormat, message: impl AsRef<str>) -> ! {
    let message = message.as_ref();
    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({"status":"error","error":message}));
        }
        OutputFormat::Quiet | OutputFormat::Human => eprintln!("error: {message}"),
    }
    std::process::exit(1);
}

fn validate_upstream_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|error| format!("invalid upstream URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("upstream URL must use http or https and include a host".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("upstream URL must not contain embedded credentials".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("upstream base URL must not contain a query string or fragment".to_string());
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn persist_model_config(path: &Path, model: &str) -> Result<(), String> {
    let mut doc = load_editable_config(path)?;
    doc["model"] = toml_edit::value(model);
    atomic_write_sensitive(path, &doc.to_string())
}

fn sync_model_dotenv(model: &str) -> Result<(), String> {
    let path = Path::new(".env");
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut lines = Vec::new();
    let mut replaced = false;
    for line in raw.lines() {
        if line.starts_with("OPENCODE_MODEL=") {
            lines.push(format!("OPENCODE_MODEL={model}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        lines.push(format!("OPENCODE_MODEL={model}"));
    }
    atomic_write_sensitive(path, &(lines.join("\n") + "\n"))
}

fn build_upstream_dotenv(
    path: &Path,
    upstream_url: Option<&str>,
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut lines: Vec<String> = raw
        .lines()
        .filter(|line| {
            !(line.starts_with("OPENCODE_UPSTREAM_BASE_URL=")
                || line.starts_with("BRIDGE_UPSTREAM_BASE_URL=")
                || line.starts_with("OPENCODE_UPSTREAM_API_KEY=")
                || line.starts_with("BRIDGE_UPSTREAM_API_KEY=")
                || model.is_some() && line.starts_with("OPENCODE_MODEL="))
        })
        .map(str::to_string)
        .collect();

    if let Some(url) = upstream_url {
        lines.push(format!("OPENCODE_UPSTREAM_BASE_URL=\"{url}\""));
    }
    if let Some(key) = api_key {
        lines.push(format!("OPENCODE_UPSTREAM_API_KEY=\"{key}\""));
    }
    if let Some(model) = model {
        lines.push(format!("OPENCODE_MODEL={model}"));
    }
    Ok(Some(lines.join("\n") + "\n"))
}

fn apply_upstream_configuration(
    config_path: &Path,
    upstream_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<(), String> {
    apply_upstream_configuration_at(config_path, Path::new(".env"), upstream_url, api_key, None)
}

fn apply_provider_configuration(
    config_path: &Path,
    upstream_url: Option<&str>,
    api_key: Option<&str>,
    model: &str,
) -> Result<(), String> {
    apply_upstream_configuration_at(
        config_path,
        Path::new(".env"),
        upstream_url,
        api_key,
        Some(model),
    )
}

fn apply_upstream_configuration_at(
    config_path: &Path,
    env_path: &Path,
    upstream_url: Option<&str>,
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let config_existed = config_path.exists();
    let original_config =
        if config_existed {
            Some(fs::read(config_path).map_err(|error| {
                format!("failed to read config {}: {error}", config_path.display())
            })?)
        } else {
            None
        };

    let mut doc = load_editable_config(config_path)?;
    match upstream_url {
        Some(url) => {
            doc["upstream_base_url"] = toml_edit::value(url);
            if let Some(key) = api_key {
                doc["upstream_api_key"] = toml_edit::value(key);
            } else {
                doc.remove("upstream_api_key");
            }
            // The credential pool is TOML-only and provider-scoped; switching
            // providers must not leak stale keys to the new upstream.
            doc.remove("upstream_api_keys");
        }
        None => {
            doc.remove("upstream_base_url");
            doc.remove("upstream_api_key");
            doc.remove("upstream_api_keys");
        }
    }

    if let Some(model) = model {
        doc["model"] = toml_edit::value(model);
    }

    let dotenv = build_upstream_dotenv(env_path, upstream_url, api_key, model)?;

    atomic_write_sensitive(config_path, &doc.to_string())?;

    if let Some(dotenv) = dotenv {
        if let Err(error) = atomic_write_sensitive(env_path, &dotenv) {
            let store = AtomicFileStore;
            let rollback = match original_config {
                Some(original) => store
                    .atomic_write(config_path, &original, true)
                    .map_err(|rollback_error| {
                        format!(
                            "{error}; additionally failed to roll back {}: {rollback_error}",
                            config_path.display()
                        )
                    }),
                None if !config_existed => store
                    .remove_if_exists(config_path)
                    .map_err(|rollback_error| {
                        format!(
                            "{error}; additionally failed to remove new {} during rollback: {rollback_error}",
                            config_path.display()
                        )
                    }),
                None => Ok(()),
            };
            rollback?;
            return Err(error);
        }
    }
    Ok(())
}

pub async fn cmd_list(args: ListArgs, fmt: OutputFormat) {
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    let upstream_url = args
        .upstream_base_url
        .as_deref()
        .unwrap_or(config.retry.upstream_base_url.as_str());
    let api_key = if args.upstream_base_url.is_some() {
        None
    } else {
        config.retry.upstream_api_key.as_ref().map(|k| k.expose())
    };
    if let Err(error) = crate::config::validate_upstream_transport(upstream_url, api_key.is_some())
    {
        exit_cli_error(fmt, error);
    }

    let cache_path = RuntimePaths::from_config(&config).model_probe_cache();
    let should_probe = args.probe;
    let mut cache_hit = false;
    let (probed, api_health) = if should_probe {
        let client = Client::new();
        if fmt == OutputFormat::Human {
            eprintln!("{}", "Probing upstream model availability...".cyan().dim());
        }
        let health = check_upstream_health(&client, upstream_url, api_key).await;
        let models = match &health {
            Ok(_) => fetch_and_probe_models(&client, upstream_url, api_key, true).await,
            Err(_) => Vec::new(),
        };
        // A completed explicit probe replaces the snapshot even when no model
        // survived. Retaining a previous non-empty result after an outage or a
        // provider catalog change would present stale availability as current.
        if let Err(error) = write_model_probe_cache(&cache_path, upstream_url, &models) {
            eprintln!("warning: {error}");
        }
        (models, Some(health))
    } else {
        let cached = read_model_probe_cache(&cache_path, upstream_url);
        cache_hit = cached.is_some();
        (cached.unwrap_or_default(), None)
    };

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
            println!("\n{}", "◆ Available Upstream Models".bold());
            match &api_health {
                Some(Ok(latency_ms)) => {
                    println!(
                        "  {} {}\n",
                        format!("Upstream: {upstream_url}").cyan().dim(),
                        format!("[API ONLINE • {latency_ms} ms]").green().bold()
                    );
                }
                Some(Err(err)) => {
                    println!(
                        "  {} {}\n",
                        format!("Upstream: {upstream_url}").cyan().dim(),
                        format!("[API OFFLINE: {err}]").red().bold()
                    );
                }
                None => {
                    println!(
                        "  {} {}\n",
                        format!("Upstream: {upstream_url}").cyan().dim(),
                        "[LIVE CHECKS SKIPPED]".dim()
                    );
                }
            }

            let has_probe_results = should_probe || cache_hit;
            let display_models: Vec<&ProbedModel> = if has_probe_results && !args.all {
                probed
                    .iter()
                    .filter(|m| {
                        m.status == ModelStatus::Online || m.status == ModelStatus::RateLimited
                    })
                    .collect()
            } else {
                probed.iter().collect()
            };

            if display_models.is_empty() {
                let message = if !should_probe && !cache_hit {
                    "No cached probe result for this upstream. Run `opencode2api provider models --probe` to check live availability."
                } else if all_models_rejected_for_auth(&probed) {
                    "Upstream credential was rejected for every curated model. Reconfigure the provider/key before retrying."
                } else {
                    "No online models currently available on upstream endpoint."
                };
                println!("  {} {message}\n", "⚠".yellow().bold());
            } else {
                let mut table = Table::new();
                table
                    .load_preset(NOTHING)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_width(presentation::content_width() as u16);

                let mut headers = vec![
                    "MODEL ID",
                    "PROVIDER",
                    "CONTEXT",
                    "AUTO-COMPACT (80%)",
                    "MAX OUT",
                    "THINKING",
                ];
                if has_probe_results {
                    headers.push("STATUS");
                    headers.push("LATENCY");
                }
                table.set_header(
                    headers
                        .into_iter()
                        .map(|h| CtCell::new(h).fg(CtColor::Cyan)),
                );

                for m in &display_models {
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

                    let autocompact_str =
                        format!("{} tokens", format_number(m.auto_compact_window));
                    let max_out_str = format_number(m.max_output_tokens);
                    let thinking_str = if m.supports_thinking {
                        "✓ Yes".green().to_string()
                    } else {
                        "- No".dim().to_string()
                    };

                    let mut row = vec![
                        CtCell::new(id_display),
                        CtCell::new(m.provider.clone()).fg(CtColor::DarkGrey),
                        CtCell::new(context_str),
                        CtCell::new(autocompact_str),
                        CtCell::new(max_out_str),
                        CtCell::new(thinking_str),
                    ];

                    if should_probe {
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
            }

            println!("  {} Active model is marked with `*`", "ℹ".cyan().dim());
            if has_probe_results {
                let online_count = probed
                    .iter()
                    .filter(|m| m.status == ModelStatus::Online)
                    .count();
                let busy_count = probed
                    .iter()
                    .filter(|m| m.status == ModelStatus::RateLimited)
                    .count();
                let total_count = probed.len();
                let dead_count = total_count.saturating_sub(online_count + busy_count);

                if all_models_rejected_for_auth(&probed) {
                    println!(
                        "  {} {}/{} curated models usable (upstream credential rejected)",
                        "ℹ".cyan().dim(),
                        online_count + busy_count,
                        total_count
                    );
                } else if !args.all && dead_count > 0 {
                    println!(
                        "  {} {}/{} models usable ({} dead/restricted hidden - use `--all` to view all)",
                        "ℹ".cyan().dim(),
                        online_count + busy_count,
                        total_count,
                        dead_count
                    );
                } else {
                    println!(
                        "  {} {}/{} models online and ready to use",
                        "ℹ".cyan().dim(),
                        online_count,
                        total_count
                    );
                }
            }
            if !should_probe && cache_hit {
                println!(
                    "  {} Using cached probe snapshot; no upstream network requests were sent",
                    "ℹ".cyan().dim()
                );
            } else if !should_probe {
                println!(
                    "  {} Cached model availability only; no upstream network requests were sent",
                    "ℹ".cyan().dim()
                );
            }
            println!(
                "  {} Use provider opencode/api to switch provider and model together\n",
                "ℹ".cyan().dim()
            );

            // ── OpenCode Free Models (always shown alongside custom API) ──
            let is_opencode = crate::application::prober::is_opencode_upstream(upstream_url);
            if !is_opencode {
                use crate::application::models::FREE_MODELS;

                println!("{}", "◆ OpenCode Free Models (opencode.ai/zen)".bold());
                println!(
                    "  {} {}\n",
                    "Provider: https://opencode.ai/zen/v1".cyan().dim(),
                    "[NO API KEY REQUIRED]".green().dim()
                );

                let mut oc_table = Table::new();
                oc_table
                    .load_preset(NOTHING)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_width(presentation::content_width() as u16);

                oc_table.set_header(
                    ["MODEL ID", "CONTEXT", "MAX OUT", "THINKING", "TIER", "SWITCH COMMAND"]
                        .into_iter()
                        .map(|h| CtCell::new(h).fg(CtColor::Cyan)),
                );

                for m in FREE_MODELS {
                    let clean_id = m.id.strip_prefix("opencode/").unwrap_or(m.id);
                    let context_str = if m.context_window >= 1_000_000 {
                        format!("{}M", m.context_window / 1_000_000)
                    } else {
                        format!("{}k", m.context_window / 1_000)
                    };
                    let max_out_str = format_number(m.max_output_tokens);
                    let thinking_str = if m.supports_thinking {
                        "✓ Yes".green().to_string()
                    } else {
                        "- No".dim().to_string()
                    };
                    let tier_str = if m.context_window >= 1_000_000 {
                        "1M (Opus)".green().bold().to_string()
                    } else {
                        "Sub-1M (Sonnet)".to_string()
                    };
                    let cmd = format!("opencode2api provider opencode {}", clean_id);

                    oc_table.add_row(vec![
                        CtCell::new(format!("  {}", clean_id)),
                        CtCell::new(context_str),
                        CtCell::new(max_out_str),
                        CtCell::new(thinking_str),
                        CtCell::new(tier_str),
                        CtCell::new(cmd).fg(CtColor::DarkGrey),
                    ]);
                }
                println!("{oc_table}\n");

                let one_m_count = FREE_MODELS.iter().filter(|m| m.context_window >= 1_000_000).count();
                let sub_m_count = FREE_MODELS.len() - one_m_count;
                println!(
                    "  {} {} free models ({} × 1M context, {} × sub-1M)",
                    "ℹ".cyan().dim(),
                    FREE_MODELS.len(),
                    one_m_count,
                    sub_m_count
                );
                println!(
                    "  {} Switch to OpenCode: opencode2api provider opencode <MODEL>\n",
                    "ℹ".cyan().dim()
                );
            }
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
    let config_path = resolved_config_path(args.config);

    if let Err(error) =
        persist_model_config(&config_path, raw_model).and_then(|_| sync_model_dotenv(raw_model))
    {
        exit_cli_error(fmt, error);
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
                "config_file": config_path.display().to_string()
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
            println!("  Saved to:            {}\n", config_path.display());
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

pub async fn cmd_provider(args: ProviderArgs, fmt: OutputFormat) {
    match args.command {
        Some(ProviderSubcommand::Opencode(args)) => cmd_provider_opencode(args, fmt).await,
        Some(ProviderSubcommand::Api(args)) => cmd_provider_api(args, fmt).await,
        Some(ProviderSubcommand::Models(args)) => cmd_list(args, fmt).await,
        None | Some(ProviderSubcommand::Status) => cmd_provider_status(fmt).await,
    }
}

fn ensure_provider_env_is_persistable(fmt: OutputFormat) {
    if crate::config::pre_dotenv_upstream_env_override_present() {
        exit_cli_error(
            fmt,
            "cannot persist provider changes while provider URL/key overrides are set in the parent shell; unset OPENCODE_UPSTREAM_BASE_URL, BRIDGE_UPSTREAM_BASE_URL, OPENCODE_UPSTREAM_API_KEY, and BRIDGE_UPSTREAM_API_KEY first",
        );
    }
}

fn normalize_opencode_model(raw: &str) -> String {
    let clean = raw.trim().strip_prefix("opencode/").unwrap_or(raw.trim());
    format!("opencode/{clean}")
}

fn read_api_key_from_stdin(enabled: bool, fmt: OutputFormat) -> Option<String> {
    if !enabled {
        return None;
    }
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_line(&mut input) {
        exit_cli_error(fmt, format!("failed to read API key from stdin: {error}"));
    }
    let key = input.trim().to_string();
    if key.is_empty() {
        exit_cli_error(fmt, "API key read from stdin is empty");
    }
    Some(key)
}

async fn cmd_provider_opencode(args: ProviderOpenCodeArgs, fmt: OutputFormat) {
    ensure_provider_env_is_persistable(fmt);
    let model = normalize_opencode_model(&args.model);
    if !crate::application::models::is_supported_free_model(&model) {
        exit_cli_error(
            fmt,
            format!(
                "unknown OpenCode free model '{}'; run 'opencode2api provider models' to inspect the catalog",
                args.model
            ),
        );
    }

    let config_path = resolved_config_path(args.config);
    if let Err(error) = apply_provider_configuration(&config_path, None, None, &model) {
        exit_cli_error(fmt, error);
    }

    render_provider_changed(
        fmt,
        "opencode",
        "https://opencode.ai/zen/v1",
        false,
        &model,
        &config_path,
    );
}

async fn cmd_provider_api(args: ProviderApiArgs, fmt: OutputFormat) {
    ensure_provider_env_is_persistable(fmt);
    let upstream_url = match validate_upstream_url(&args.url) {
        Ok(url) => url,
        Err(error) => exit_cli_error(fmt, error),
    };
    let model = args.model.trim().to_string();
    if model.is_empty() {
        exit_cli_error(fmt, "API model id must not be empty");
    }

    let api_key_owned = read_api_key_from_stdin(args.api_key_stdin, fmt);
    let api_key = api_key_owned.as_deref();
    if let Err(error) = crate::config::validate_upstream_transport(&upstream_url, api_key.is_some())
    {
        exit_cli_error(fmt, error);
    }

    let config_path = resolved_config_path(args.config);
    if let Err(error) =
        apply_provider_configuration(&config_path, Some(&upstream_url), api_key, &model)
    {
        exit_cli_error(fmt, error);
    }

    render_provider_changed(
        fmt,
        "api",
        &upstream_url,
        api_key.is_some(),
        &model,
        &config_path,
    );
}

fn render_provider_changed(
    fmt: OutputFormat,
    mode: &str,
    endpoint: &str,
    api_key_set: bool,
    model: &str,
    config_path: &Path,
) {
    let profile = resolve_model_profile(model);
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "provider_mode": mode,
                "endpoint": endpoint,
                "api_key_configured": api_key_set,
                "model": model,
                "context_window": profile.context_window,
                "auto_compact_window": profile.auto_compact_window(),
                "max_output_tokens": profile.max_output_tokens,
                "default_output_tokens": model_default_output_tokens(model),
                "pricing": model_pricing(model),
                "config_file": config_path.display().to_string(),
            })
        ),
        OutputFormat::Quiet => println!("{mode}:{model}"),
        OutputFormat::Human => {
            println!("\n{}", "✓ Provider configuration updated".green().bold());
            println!("  Mode:                {}", mode.cyan().bold());
            println!("  Endpoint:            {}", endpoint.cyan());
            println!("  Model:               {}", model.green().bold());
            println!(
                "  Context:             {} tokens",
                format_number(profile.context_window)
            );
            println!(
                "  Auto-Compact (80%):  {} tokens",
                format_number(profile.auto_compact_window())
            );
            println!(
                "  Max Output:          {} tokens",
                format_number(profile.max_output_tokens)
            );
            if let Some(default_output) = model_default_output_tokens(model) {
                println!(
                    "  Default Output:      {} tokens",
                    format_number(default_output)
                );
            }
            println!("  Pricing:             {}", model_pricing(model));
            println!(
                "  API Key:             {}",
                if api_key_set { "Configured" } else { "None" }
            );
            println!("  Saved To:            {}\n", config_path.display());
        }
    }
}

async fn cmd_provider_status(fmt: OutputFormat) {
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    let upstream_url = config.retry.upstream_base_url.as_str();
    let mode = if crate::application::prober::is_opencode_upstream(upstream_url) {
        "opencode"
    } else {
        "api"
    };
    let model = config.model.as_deref().unwrap_or("opencode/mimo-v2.5-free");
    let profile = resolve_model_profile(model);
    let api_key_set = config.retry.upstream_api_key.is_some();

    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "provider_mode": mode,
                "endpoint": upstream_url,
                "api_key_configured": api_key_set,
                "model": model,
                "context_window": profile.context_window,
                "auto_compact_window": profile.auto_compact_window(),
                "max_output_tokens": profile.max_output_tokens,
                "default_output_tokens": model_default_output_tokens(model),
                "pricing": model_pricing(model),
            })
        ),
        OutputFormat::Quiet => println!("{mode}:{model}"),
        OutputFormat::Human => {
            println!("\n{}", "◆ Active Provider".bold());
            println!("  Mode:                {}", mode.cyan().bold());
            println!("  Endpoint:            {}", upstream_url.cyan());
            println!("  Model:               {}", model.green().bold());
            println!(
                "  Context:             {} tokens",
                format_number(profile.context_window)
            );
            println!(
                "  Auto-Compact (80%):  {} tokens",
                format_number(profile.auto_compact_window())
            );
            println!(
                "  Max Output:          {} tokens",
                format_number(profile.max_output_tokens)
            );
            if let Some(default_output) = model_default_output_tokens(model) {
                println!(
                    "  Default Output:      {} tokens",
                    format_number(default_output)
                );
            }
            println!("  Pricing:             {}", model_pricing(model));
            println!(
                "  API Key:             {}\n",
                if api_key_set { "Configured" } else { "None" }
            );
        }
    }
}

pub async fn cmd_upstream(args: UpstreamArgs, fmt: OutputFormat) {
    match args.command {
        Some(UpstreamSubcommand::Set(set_args)) => cmd_upstream_set(set_args, fmt).await,
        Some(UpstreamSubcommand::Reset) => cmd_upstream_reset(fmt).await,
        None | Some(UpstreamSubcommand::Status) => cmd_upstream_status(fmt).await,
    }
}

async fn cmd_upstream_set(args: UpstreamSetArgs, fmt: OutputFormat) {
    if crate::config::pre_dotenv_upstream_env_override_present() {
        exit_cli_error(
            fmt,
            "cannot persist an upstream provider change while an upstream URL or API key is set in the parent shell environment; unset OPENCODE_UPSTREAM_BASE_URL, BRIDGE_UPSTREAM_BASE_URL, OPENCODE_UPSTREAM_API_KEY, and BRIDGE_UPSTREAM_API_KEY first",
        );
    }

    let upstream_url = match validate_upstream_url(&args.url) {
        Ok(url) => url,
        Err(error) => exit_cli_error(fmt, error),
    };
    let api_key_owned = if args.api_key_stdin {
        let mut input = String::new();
        if let Err(error) = std::io::stdin().read_line(&mut input) {
            exit_cli_error(fmt, format!("failed to read API key from stdin: {error}"));
        }
        let key = input.trim().to_string();
        if key.is_empty() {
            exit_cli_error(fmt, "API key read from stdin is empty");
        }
        Some(key)
    } else {
        None
    };
    let api_key = api_key_owned.as_deref();
    if let Err(error) = crate::config::validate_upstream_transport(&upstream_url, api_key.is_some())
    {
        exit_cli_error(fmt, error);
    }
    let config_path = resolved_config_path(args.config);

    if let Err(error) = apply_upstream_configuration(&config_path, Some(&upstream_url), api_key) {
        exit_cli_error(fmt, error);
    }

    let client = Client::new();
    let health = check_upstream_health(&client, &upstream_url, api_key).await;

    match fmt {
        OutputFormat::Json => {
            let res = serde_json::json!({
                "status": "ok",
                "upstream_base_url": upstream_url,
                "api_key_set": api_key.is_some(),
                "health": health.is_ok(),
                "config_file": config_path.display().to_string()
            });
            println!("{res}");
        }
        OutputFormat::Quiet => {
            println!("{upstream_url}");
        }
        OutputFormat::Human => {
            println!("\n{}", "✓ Upstream API endpoint configured".green().bold());
            println!("  Endpoint URL:        {}", upstream_url.cyan().bold());
            println!(
                "  API Key:             {}",
                if api_key.is_some() {
                    "Configured [Bearer Token]".green()
                } else {
                    "Cleared / None".dim()
                }
            );
            match health {
                Ok(latency_ms) => {
                    println!(
                        "  Live Health:         {}",
                        format!("ONLINE • {latency_ms} ms").green().bold()
                    );
                }
                Err(err) => {
                    println!(
                        "  Live Health:         {}",
                        format!("OFFLINE / UNREACHABLE ({err})").red().bold()
                    );
                }
            }
            println!("  Saved To:            {}\n", config_path.display());
            println!(
                "  {} Run opencode2api provider models to discover models from this API\n",
                "ℹ".cyan().dim()
            );
        }
    }
}

async fn cmd_upstream_reset(fmt: OutputFormat) {
    if crate::config::pre_dotenv_upstream_env_override_present() {
        exit_cli_error(
            fmt,
            "cannot reset the persisted upstream provider while an upstream URL or API key is set in the parent shell environment; unset OPENCODE_UPSTREAM_BASE_URL, BRIDGE_UPSTREAM_BASE_URL, OPENCODE_UPSTREAM_API_KEY, and BRIDGE_UPSTREAM_API_KEY first",
        );
    }

    let default_url = "https://opencode.ai/zen/v1";
    let config_path = resolved_config_path(None);

    if let Err(error) = apply_upstream_configuration(&config_path, None, None) {
        exit_cli_error(fmt, error);
    }

    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"status": "ok", "upstream_base_url": default_url})
        ),
        OutputFormat::Quiet => println!("{default_url}"),
        OutputFormat::Human => {
            println!(
                "\n{}",
                "✓ Upstream endpoint reset to default (OpenCode Zen)"
                    .green()
                    .bold()
            );
            println!("  Endpoint URL:        {}\n", default_url.cyan().bold());
        }
    }
}

async fn cmd_upstream_status(fmt: OutputFormat) {
    let config = BridgeConfig::from_env_and_cli(CliOverrides::default());
    let client = Client::new();
    let upstream_url = config.retry.upstream_base_url.as_str();
    let api_key = config.retry.upstream_api_key.as_ref().map(|k| k.expose());
    if let Err(error) = crate::config::validate_upstream_transport(upstream_url, api_key.is_some())
    {
        exit_cli_error(fmt, error);
    }
    let health = check_upstream_health(&client, upstream_url, api_key).await;

    match fmt {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "upstream_base_url": upstream_url,
                    "api_key_configured": api_key.is_some(),
                    "health": health.is_ok(),
                })
            );
        }
        OutputFormat::Quiet => println!("{upstream_url}"),
        OutputFormat::Human => {
            println!("\n{}", "◆ Upstream Endpoint Configuration".bold());
            println!("  Endpoint URL:        {}", upstream_url.cyan().bold());
            println!(
                "  API Key:             {}",
                if api_key.is_some() {
                    "Configured [Bearer Token]".green()
                } else {
                    "None (Free tier / Open Access)".dim()
                }
            );
            match health {
                Ok(latency_ms) => {
                    println!(
                        "  Live Health:         {}\n",
                        format!("ONLINE • {latency_ms} ms").green().bold()
                    );
                }
                Err(err) => {
                    println!(
                        "  Live Health:         {}\n",
                        format!("OFFLINE / UNREACHABLE ({err})").red().bold()
                    );
                }
            }
            println!(
                "  {} Run `opencode2api list` to inspect model availability\n",
                "ℹ".cyan().dim()
            );
        }
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "opencode2api-models-{name}-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn switching_upstream_without_key_clears_stale_credentials_and_secures_files() {
        let root = temp_dir("clear-stale");
        let config = root.join("opencode2api.toml");
        let env = root.join(".env");
        std::fs::write(
            &config,
            "upstream_base_url = \"https://old.example/v1\"\nupstream_api_key = \"OLD_SECRET\"\nmodel = \"keep-me\"\n",
        )
        .unwrap();
        std::fs::write(
            &env,
            "BRIDGE_UPSTREAM_BASE_URL=\"https://old.example/v1\"\nBRIDGE_UPSTREAM_API_KEY=\"OLD_SECRET\"\nOTHER=keep\n",
        )
        .unwrap();

        apply_upstream_configuration_at(&config, &env, Some("https://new.example/v1"), None, None)
            .unwrap();

        let config_text = std::fs::read_to_string(&config).unwrap();
        assert!(config_text.contains("upstream_base_url = \"https://new.example/v1\""));
        assert!(config_text.contains("model = \"keep-me\""));
        assert!(!config_text.contains("upstream_api_key"));
        assert!(!config_text.contains("OLD_SECRET"));

        let env_text = std::fs::read_to_string(&env).unwrap();
        assert!(env_text.contains("OPENCODE_UPSTREAM_BASE_URL=\"https://new.example/v1\""));
        assert!(env_text.contains("OTHER=keep"));
        assert!(!env_text.contains("UPSTREAM_API_KEY"));
        assert!(!env_text.contains("OLD_SECRET"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&env).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provider_switch_and_reset_clear_the_multi_key_pool() {
        let root = temp_dir("clear-multi-key");
        let config = root.join("opencode2api.toml");
        let env = root.join(".env");
        std::fs::write(
            &config,
            "upstream_base_url = \"https://old.example/v1\"\nupstream_api_keys = [\"OLD_SECRET_A\", \"OLD_SECRET_B\"]\nmodel = \"keep-me\"\n",
        )
        .unwrap();
        std::fs::write(&env, "OTHER=keep\n").unwrap();

        // Switching provider clears the TOML-only credential pool as well as
        // the singular key, so stale credentials cannot leak to a new upstream.

        apply_upstream_configuration_at(&config, &env, Some("https://new.example/v1"), None, None)
            .unwrap();

        let config_text = std::fs::read_to_string(&config).unwrap();
        assert!(config_text.contains("model = \"keep-me\""));
        assert!(!config_text.contains("upstream_api_keys"));
        assert!(!config_text.contains("OLD_SECRET_A"));
        assert!(!config_text.contains("OLD_SECRET_B"));

        // Resetting the provider must also clear any remaining pool entries..
        apply_upstream_configuration_at(&config, &env, None, None, None).unwrap();

        let config_text = std::fs::read_to_string(&config).unwrap();
        assert!(!config_text.contains("upstream_api_keys"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_probe_cache_round_trips_only_for_its_upstream() {
        let root = temp_dir("model-probe-cache");
        let cache = root.join("model-probe-cache.json");
        let models = vec![ProbedModel {
            id: "deepseek-v4-flash".to_string(),
            label: "DeepSeek V4 Flash".to_string(),
            provider: "b.ai".to_string(),
            context_window: 1_000_000,
            auto_compact_window: 800_000,
            max_output_tokens: 384_000,
            supports_thinking: true,
            status: ModelStatus::Online,
            latency_ms: Some(42),
            error: None,
        }];

        write_model_probe_cache(&cache, "https://api.b.ai/v1", &models).unwrap();

        assert_eq!(
            read_model_probe_cache(&cache, "https://api.b.ai/v1").unwrap(),
            models
        );
        assert!(read_model_probe_cache(&cache, "https://opencode.ai/zen/v1").is_none());

        write_model_probe_cache(&cache, "https://api.b.ai/v1", &[]).unwrap();
        assert_eq!(
            read_model_probe_cache(&cache, "https://api.b.ai/v1").unwrap(),
            Vec::<ProbedModel>::new()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provider_switch_updates_url_key_and_model_atomically() {
        let root = temp_dir("provider-atomic");
        let config = root.join("opencode2api.toml");
        let env = root.join(".env");
        std::fs::write(
            &config,
            "upstream_base_url = \"https://old.example/v1\"\nupstream_api_key = \"OLD_SECRET\"\nmodel = \"old-model\"\n",
        )
        .unwrap();
        std::fs::write(
            &env,
            "OPENCODE_UPSTREAM_BASE_URL=\"https://old.example/v1\"\nOPENCODE_UPSTREAM_API_KEY=\"OLD_SECRET\"\nOPENCODE_MODEL=old-model\nOTHER=keep\n",
        )
        .unwrap();

        apply_upstream_configuration_at(
            &config,
            &env,
            Some("https://api.example/v1"),
            Some("NEW_SECRET"),
            Some("deepseek-v4-flash"),
        )
        .unwrap();

        let config_text = std::fs::read_to_string(&config).unwrap();
        assert!(config_text.contains("upstream_base_url = \"https://api.example/v1\""));
        assert!(config_text.contains("upstream_api_key = \"NEW_SECRET\""));
        assert!(config_text.contains("model = \"deepseek-v4-flash\""));
        assert!(!config_text.contains("OLD_SECRET"));

        let env_text = std::fs::read_to_string(&env).unwrap();
        assert!(env_text.contains("OPENCODE_UPSTREAM_BASE_URL=\"https://api.example/v1\""));
        assert!(env_text.contains("OPENCODE_UPSTREAM_API_KEY=\"NEW_SECRET\""));
        assert!(env_text.contains("OPENCODE_MODEL=deepseek-v4-flash"));
        assert!(env_text.contains("OTHER=keep"));
        assert!(!env_text.contains("OLD_SECRET"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_toml_is_never_replaced_with_a_default_document() {
        let root = temp_dir("invalid-toml");
        let config = root.join("opencode2api.toml");
        let env = root.join(".env");
        let original =
            "host = \"127.0.0.1\"\ndashboard_admin_token = \"KEEP_ME\"\nthis is invalid toml !!!\n";
        std::fs::write(&config, original).unwrap();

        let error = apply_upstream_configuration_at(
            &config,
            &env,
            Some("https://new.example/v1"),
            None,
            None,
        )
        .expect_err("invalid TOML must fail closed");

        assert!(error.contains("refusing to overwrite invalid TOML"));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persistence_errors_are_returned_instead_of_being_ignored() {
        let root = temp_dir("write-error");
        let blocker = root.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        let config = blocker.join("opencode2api.toml");
        let env = root.join(".env");

        let error = apply_upstream_configuration_at(
            &config,
            &env,
            Some("https://new.example/v1"),
            Some("FAKE_SECRET"),
            None,
        )
        .expect_err("write failure must be visible");

        assert!(error.contains("failed to persist"));
        assert!(!config.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upstream_url_validation_rejects_non_http_credentials_and_query_state() {
        assert!(validate_upstream_url("ftp://example.com/v1").is_err());
        assert!(validate_upstream_url("https://user:pass@example.com/v1").is_err());
        assert!(validate_upstream_url("https://example.com/v1?token=x").is_err());
        assert_eq!(
            validate_upstream_url(" https://example.com/v1/ ").unwrap(),
            "https://example.com/v1"
        );
    }
}
