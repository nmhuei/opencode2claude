//! Canonical provider-management commands.

use crate::cli::{
    ProviderAddArgs, ProviderAliasCommand, ProviderAliasListArgs, ProviderAliasSetArgs,
    ProviderAliasShowArgs, ProviderCredentialCommand, ProviderCredentialListArgs,
    ProviderCredentialRemoveArgs, ProviderCredentialSetArgs, ProviderModelAddArgs,
    ProviderModelCommand, ProviderModelRemoveArgs, ProviderRemoveArgs,
};
use crate::config::{BridgeConfig, CliOverrides};
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use crate::output::OutputFormat;
use crate::provider::config::load_provider_registry;
use crate::provider::credentials::default_store;
use crate::provider::types::*;
use crate::provider::ProviderRegistry;
use std::io::Read;
use std::path::{Path, PathBuf};
use yansi::Paint;

fn config_path(explicit: Option<String>) -> PathBuf {
    BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: explicit,
        ..Default::default()
    })
    .management
    .config_path
}
fn fail(fmt: OutputFormat, message: impl AsRef<str>) -> ! {
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"status":"error","error":message.as_ref()})
        ),
        _ => eprintln!("error: {}", message.as_ref()),
    }
    std::process::exit(1)
}
fn write_config(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), String> {
    AtomicFileStore
        .atomic_write(path, doc.to_string().as_bytes(), true)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}
fn editable(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    if !path.exists() {
        return Ok(toml_edit::DocumentMut::new());
    }
    std::fs::read_to_string(path)
        .map_err(|e| e.to_string())?
        .parse()
        .map_err(|e| format!("invalid TOML: {e}"))
}
fn registry(path: &Path) -> Result<ProviderRegistry, String> {
    if path.exists() {
        match load_provider_registry(path) {
            Ok(value) if value.providers().next().is_some() => return Ok(value),
            Ok(_) => {}
            Err(error) => {
                let raw = std::fs::read_to_string(path).unwrap_or_default();
                if raw.contains("schema_version") {
                    return Err(error.to_string());
                }
            }
        }
    }
    let config = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(path.display().to_string()),
        ..Default::default()
    });
    Ok(ProviderRegistry::from_legacy(
        config.retry.upstream_base_url,
        config
            .model
            .unwrap_or_else(|| "opencode/mimo-v2.5-free".to_string()),
        None,
    ))
}
fn kind(value: &str) -> Result<ProviderKind, String> {
    match value {
        "opencode" => Ok(ProviderKind::OpenCode),
        "kilo" => Ok(ProviderKind::Kilo),
        "bai" | "b.ai" => Ok(ProviderKind::Bai),
        "openai-compatible" | "generic" => Ok(ProviderKind::OpenAiCompatible),
        _ => Err(format!("unknown provider kind {value}")),
    }
}
fn protocol(value: &str) -> Result<ProviderProtocol, String> {
    match value {
        "openai_chat_completions" | "chat" => Ok(ProviderProtocol::OpenAiChatCompletions),
        "openai_responses" | "responses" => Ok(ProviderProtocol::OpenAiResponses),
        "anthropic_messages" | "messages" => Ok(ProviderProtocol::AnthropicMessages),
        _ => Err(format!("unknown provider protocol {value}")),
    }
}
fn auth_scheme(value: &str) -> Result<AuthScheme, String> {
    match value {
        "bearer" => Ok(AuthScheme::Bearer),
        "x-api-key" | "x_api_key" => Ok(AuthScheme::XApiKey),
        "none" => Ok(AuthScheme::None),
        _ => Err(format!("unknown auth scheme {value}")),
    }
}
fn provider_url(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(value).map_err(|error| format!("invalid provider URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "provider URL must be http(s), have a host, and contain no credentials/query/fragment"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

pub fn list(fmt: OutputFormat, explicit: Option<String>) {
    let path = config_path(explicit);
    let registry = match registry(&path) {
        Ok(v) => v,
        Err(e) => fail(fmt, e),
    };
    let values: Vec<_> = registry.providers().map(|p| serde_json::json!({"id":p.id,"name":p.name,"kind":p.kind,"base_url":p.base_url,"protocol":p.protocol,"enabled":p.enabled})).collect();
    match fmt {
        OutputFormat::Json => println!("{}", serde_json::json!({"providers":values})),
        OutputFormat::Quiet => {
            for value in values {
                println!("{}", value["id"].as_str().unwrap_or_default());
            }
        }
        OutputFormat::Human => {
            println!("\n{}", "◆ Configured Providers".bold());
            if values.is_empty() {
                println!("  No providers configured");
            }
            for value in values {
                println!(
                    "  {:<16} {} ({})",
                    value["id"].as_str().unwrap_or(""),
                    value["base_url"].as_str().unwrap_or(""),
                    value["kind"].as_str().unwrap_or("")
                );
            }
            println!();
        }
    }
}

pub fn add(args: ProviderAddArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let provider_kind = kind(&args.kind).unwrap_or_else(|e| fail(fmt, e));
    let provider_protocol = protocol(&args.protocol).unwrap_or_else(|e| fail(fmt, e));
    let provider_url = provider_url(&args.url).unwrap_or_else(|e| fail(fmt, e));
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    doc["schema_version"] = toml_edit::value(2);
    if doc
        .get("providers")
        .is_none_or(|item| !item.is_array_of_tables())
    {
        doc["providers"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let providers = doc["providers"].as_array_of_tables_mut().unwrap();
    if providers
        .iter()
        .any(|p| p.get("id").and_then(|v| v.as_str()) == Some(args.id.as_str()))
    {
        fail(fmt, format!("provider {} already exists", args.id));
    }
    let mut table = toml_edit::Table::new();
    table["id"] = toml_edit::value(args.id.clone());
    table["name"] = toml_edit::value(args.id.clone());
    table["kind"] = toml_edit::value(match provider_kind {
        ProviderKind::OpenCode => "opencode",
        ProviderKind::Kilo => "kilo",
        ProviderKind::Bai => "bai",
        ProviderKind::OpenAiCompatible => "openai-compatible",
    });
    table["base_url"] = toml_edit::value(provider_url);
    table["protocol"] = toml_edit::value(match provider_protocol {
        ProviderProtocol::OpenAiChatCompletions => "openai_chat_completions",
        ProviderProtocol::OpenAiResponses => "openai_responses",
        ProviderProtocol::AnthropicMessages => "anthropic_messages",
    });
    providers.push(table);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    match fmt {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"status":"ok","provider":args.id,"config_file":path})
        ),
        OutputFormat::Quiet => println!("{}", args.id),
        OutputFormat::Human => println!("✓ provider {} added", args.id),
    }
}

pub fn remove(args: ProviderRemoveArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let current = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    if current
        .credentials()
        .any(|credential| credential.provider_id.as_ref() == args.id)
        || current
            .models()
            .any(|model| model.provider_id.as_ref() == args.id)
        || current.aliases().any(|alias| {
            alias
                .candidates
                .iter()
                .any(|candidate| candidate.provider_id.as_ref() == args.id)
        })
    {
        fail(
            fmt,
            format!(
                "provider {} is still referenced by credentials, models, or aliases",
                args.id
            ),
        );
    }
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    let Some(providers) = doc["providers"].as_array_of_tables_mut() else {
        fail(fmt, "no schema-v2 providers configured")
    };
    let old = providers.len();
    let retained: Vec<_> = providers
        .iter()
        .filter(|p| p.get("id").and_then(|v| v.as_str()) != Some(args.id.as_str()))
        .cloned()
        .collect();
    if old == retained.len() {
        fail(fmt, format!("provider {} not found", args.id));
    }
    let mut replacement = toml_edit::ArrayOfTables::new();
    for p in retained {
        replacement.push(p);
    }
    doc["providers"] = toml_edit::Item::ArrayOfTables(replacement);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    if matches!(fmt, OutputFormat::Json) {
        println!("{}", serde_json::json!({"status":"ok","removed":args.id}));
    } else {
        println!("✓ provider {} removed", args.id);
    }
}

pub fn credentials(command: ProviderCredentialCommand, fmt: OutputFormat) {
    match command {
        ProviderCredentialCommand::List(args) => credential_list(args, fmt),
        ProviderCredentialCommand::Set(args) => credential_set(args, fmt),
        ProviderCredentialCommand::Remove(args) => credential_remove(args, fmt),
    }
}
fn credential_list(args: ProviderCredentialListArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    let values: Vec<_> = reg.credentials().map(|c| serde_json::json!({"id":c.id,"provider_id":c.provider_id,"source":c.source,"auth_scheme":c.auth_scheme})).collect();
    match fmt {
        OutputFormat::Json => println!("{}", serde_json::json!({"credentials":values})),
        _ => {
            for value in values {
                println!("{} -> {}", value["id"], value["provider_id"]);
            }
        }
    }
}
fn credential_set(args: ProviderCredentialSetArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    if reg.provider(&args.provider).is_none() {
        fail(fmt, format!("provider {} not found", args.provider));
    }
    let scheme = auth_scheme(&args.auth_scheme).unwrap_or_else(|e| fail(fmt, e));
    let mut value = String::new();
    if args.api_key_stdin {
        std::io::stdin()
            .read_to_string(&mut value)
            .unwrap_or_else(|e| fail(fmt, e.to_string()));
        value = value.trim().to_string();
    }
    let (source, secret) = if let Some(variable) = args.env {
        (SecretSource::Env { variable }, None)
    } else if args.api_key_stdin {
        if value.is_empty() {
            fail(fmt, "credential read from stdin is empty");
        }
        (
            SecretSource::Managed {
                id: args.id.clone(),
            },
            Some(value),
        )
    } else {
        fail(fmt, "provide --env NAME or --api-key-stdin")
    };
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    doc["schema_version"] = toml_edit::value(2);
    if doc
        .get("credentials")
        .is_none_or(|item| !item.is_array_of_tables())
    {
        doc["credentials"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let credentials = doc["credentials"].as_array_of_tables_mut().unwrap();
    let mut table = toml_edit::Table::new();
    table["id"] = toml_edit::value(args.id.clone());
    table["provider_id"] = toml_edit::value(args.provider.clone());
    table["auth_scheme"] = toml_edit::value(match scheme {
        AuthScheme::Bearer => "bearer",
        AuthScheme::XApiKey => "x-api-key",
        AuthScheme::None => "none",
    });
    match &source {
        SecretSource::Env { variable } => {
            table["env"] = toml_edit::value(variable.as_str());
        }
        SecretSource::Managed { id } => {
            table["managed"] = toml_edit::value(id.as_str());
        }
        _ => {}
    };
    credentials.push(table);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    if let Some(secret) = secret {
        default_store(&path)
            .put_file_secret(&args.id, &secret)
            .unwrap_or_else(|e| fail(fmt, e.to_string()));
    }
    if matches!(fmt, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({"status":"ok","credential_id":args.id,"provider_id":args.provider})
        );
    } else {
        println!("✓ credential {} configured for {}", args.id, args.provider);
    }
}
fn credential_remove(args: ProviderCredentialRemoveArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    let Some(credentials) = doc["credentials"].as_array_of_tables_mut() else {
        fail(fmt, "no credentials configured")
    };
    let retained: Vec<_> = credentials
        .iter()
        .filter(|p| p.get("id").and_then(|v| v.as_str()) != Some(args.id.as_str()))
        .cloned()
        .collect();
    let mut replacement = toml_edit::ArrayOfTables::new();
    for p in retained {
        replacement.push(p);
    }
    doc["credentials"] = toml_edit::Item::ArrayOfTables(replacement);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    let _ = default_store(&path).remove(&args.id);
    if matches!(fmt, OutputFormat::Json) {
        println!("{}", serde_json::json!({"status":"ok","removed":args.id}));
    } else {
        println!("✓ credential {} removed", args.id);
    }
}

pub fn aliases(command: ProviderAliasCommand, fmt: OutputFormat) {
    match command {
        ProviderAliasCommand::List(args) => alias_list(args, fmt),
        ProviderAliasCommand::Show(args) => alias_show(args, fmt),
        ProviderAliasCommand::Set(args) => alias_set(args, fmt),
    }
}

pub fn models(command: ProviderModelCommand, fmt: OutputFormat) {
    match command {
        ProviderModelCommand::Add(args) => model_add(args, fmt),
        ProviderModelCommand::Remove(args) => model_remove(args, fmt),
    }
}

fn model_add(args: ProviderModelAddArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let current = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    if current.provider(&args.provider).is_none() {
        fail(fmt, format!("provider {} not found", args.provider));
    }
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    doc["schema_version"] = toml_edit::value(2);
    if doc
        .get("models")
        .is_none_or(|item| !item.is_array_of_tables())
    {
        doc["models"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let models = doc["models"].as_array_of_tables_mut().unwrap();
    let retained: Vec<_> = models
        .iter()
        .filter(|model| {
            !(model.get("provider_id").and_then(|v| v.as_str()) == Some(args.provider.as_str())
                && model.get("model_id").and_then(|v| v.as_str()) == Some(args.model.as_str()))
        })
        .cloned()
        .collect();
    let mut replacement = toml_edit::ArrayOfTables::new();
    for model in retained {
        replacement.push(model);
    }
    let mut table = toml_edit::Table::new();
    table["provider_id"] = toml_edit::value(args.provider.clone());
    table["model_id"] = toml_edit::value(args.model.clone());
    table["context_window"] = toml_edit::value(args.context_window as i64);
    table["verified_context"] = toml_edit::value(true);
    table["free"] = toml_edit::value(args.free);
    if let Some(value) = args.max_output_tokens {
        table["max_output_tokens"] = toml_edit::value(value as i64);
    }
    if let Some(value) = args.wire_model {
        table["wire_model_id"] = toml_edit::value(value);
    }
    replacement.push(table);
    doc["models"] = toml_edit::Item::ArrayOfTables(replacement);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    if matches!(fmt, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({"status":"ok","provider":args.provider,"model":args.model,"context_window":args.context_window})
        );
    } else {
        println!("✓ model {}:{} configured", args.provider, args.model);
    }
}

fn model_remove(args: ProviderModelRemoveArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    let Some(models) = doc.get("models").and_then(|item| item.as_array_of_tables()) else {
        fail(fmt, "no provider models configured")
    };
    let retained: Vec<_> = models
        .iter()
        .filter(|model| {
            !(model.get("provider_id").and_then(|v| v.as_str()) == Some(args.provider.as_str())
                && model.get("model_id").and_then(|v| v.as_str()) == Some(args.model.as_str()))
        })
        .cloned()
        .collect();
    if retained.len() == models.len() {
        fail(
            fmt,
            format!("model {}:{} not found", args.provider, args.model),
        );
    }
    let mut replacement = toml_edit::ArrayOfTables::new();
    for model in retained {
        replacement.push(model);
    }
    doc["models"] = toml_edit::Item::ArrayOfTables(replacement);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    if matches!(fmt, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({"status":"ok","removed":format!("{}:{}", args.provider, args.model)})
        );
    } else {
        println!("✓ model {}:{} removed", args.provider, args.model);
    }
}
fn alias_list(args: ProviderAliasListArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    let values: Vec<_> = reg.aliases().map(|a| serde_json::json!({"id":a.id,"client_model":a.client_model,"context_window":a.context_window,"strict_context":a.strict_context,"candidates":a.candidates})).collect();
    if matches!(fmt, OutputFormat::Json) {
        println!("{}", serde_json::json!({"aliases":values}));
    } else {
        for a in values {
            println!(
                "{} -> {} ({} context)",
                a["id"], a["client_model"], a["context_window"]
            );
        }
    }
}
fn alias_show(args: ProviderAliasShowArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    let alias = reg
        .alias(&args.id)
        .unwrap_or_else(|| fail(fmt, format!("alias {} not found", args.id)));
    if matches!(fmt, OutputFormat::Json) {
        println!("{}", serde_json::to_string(alias).unwrap());
    } else {
        println!(
            "{} -> {} / {} tokens / strict={}",
            alias.id, alias.client_model, alias.context_window, alias.strict_context
        );
        for c in &alias.candidates {
            println!("  priority={} {}:{}", c.priority, c.provider_id, c.model_id);
        }
    }
}
fn alias_set(args: ProviderAliasSetArgs, fmt: OutputFormat) {
    let path = config_path(args.config);
    if args.candidate.is_empty() {
        fail(
            fmt,
            "at least one --candidate provider:model[:credential] is required",
        );
    }
    let candidates = args
        .candidate
        .iter()
        .enumerate()
        .map(|(priority, raw)| {
            let parts: Vec<_> = raw.split(':').collect();
            if parts.len() < 2 || parts.len() > 3 {
                fail(
                    fmt,
                    format!("invalid candidate {raw}; use provider:model[:credential]"),
                );
            }
            ModelCandidate {
                provider_id: parts[0].into(),
                model_id: parts[1].to_string(),
                credential_id: parts.get(2).map(|v| (*v).into()),
                priority: priority as i32,
            }
        })
        .collect::<Vec<_>>();
    let existing = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    for candidate in &candidates {
        if existing.provider(&candidate.provider_id).is_none() {
            fail(fmt, format!("provider {} not found", candidate.provider_id));
        }
        let model = existing
            .model(&candidate.provider_id, &candidate.model_id)
            .unwrap_or_else(|| {
                fail(
                    fmt,
                    format!(
                        "model {}:{} not found",
                        candidate.provider_id, candidate.model_id
                    ),
                )
            });
        if (args.context_window >= 1_000_000 && !model.verified_context)
            || args.context_window > model.context_window.unwrap_or_default()
        {
            fail(
                fmt,
                format!(
                    "candidate {}:{} does not satisfy {} token context",
                    candidate.provider_id, candidate.model_id, args.context_window
                ),
            );
        }
        if let Some(credential) = &candidate.credential_id {
            let credential = existing
                .credentials()
                .find(|value| &value.id == credential)
                .unwrap_or_else(|| fail(fmt, format!("credential {} not found", credential)));
            if credential.provider_id != candidate.provider_id {
                fail(
                    fmt,
                    format!("credential {} belongs to another provider", credential.id),
                );
            }
        }
    }
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    doc["schema_version"] = toml_edit::value(2);
    if doc
        .get("aliases")
        .is_none_or(|item| !item.is_array_of_tables())
    {
        doc["aliases"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let aliases = doc["aliases"].as_array_of_tables_mut().unwrap();
    let retained: Vec<_> = aliases
        .iter()
        .filter(|table| table.get("id").and_then(|value| value.as_str()) != Some(args.id.as_str()))
        .cloned()
        .collect();
    let mut replacement = toml_edit::ArrayOfTables::new();
    for table in retained {
        replacement.push(table);
    }
    *aliases = replacement;
    let mut table = toml_edit::Table::new();
    table["id"] = toml_edit::value(args.id.clone());
    table["client_model"] = toml_edit::value(args.client_model.clone());
    table["context_window"] = toml_edit::value(args.context_window as i64);
    table["strict_context"] = toml_edit::value(args.context_window >= 1_000_000);
    let mut aot = toml_edit::ArrayOfTables::new();
    for c in &candidates {
        let mut t = toml_edit::Table::new();
        t["provider_id"] = toml_edit::value(c.provider_id.to_string());
        t["model_id"] = toml_edit::value(c.model_id.clone());
        if let Some(id) = &c.credential_id {
            t["credential_id"] = toml_edit::value(id.to_string());
        }
        t["priority"] = toml_edit::value(c.priority as i64);
        aot.push(t);
    }
    table["candidates"] = toml_edit::Item::ArrayOfTables(aot);
    aliases.push(table);
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    let alias = reg
        .alias(&args.id)
        .unwrap_or_else(|| fail(fmt, "alias was not persisted"));
    if let Err(error) = reg.resolve_alias(&args.id) {
        if args.context_window >= 1_000_000 {
            fail(fmt, error.to_string());
        }
    }
    if matches!(fmt, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({"status":"ok","alias":alias.id,"client_model":alias.client_model,"context_window":alias.context_window})
        );
    } else {
        println!("✓ alias {} configured as {}", alias.id, alias.client_model);
    }
}

pub fn activate(alias: String, config: Option<String>, fmt: OutputFormat) {
    let path = config_path(config);
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    let selected = reg
        .alias(&alias)
        .unwrap_or_else(|| fail(fmt, format!("alias {} not found", alias)));
    if selected.strict_context {
        if let Err(error) = reg.resolve_alias(&alias) {
            fail(fmt, error.to_string());
        }
    }
    let mut doc = editable(&path).unwrap_or_else(|e| fail(fmt, e));
    doc["active_alias"] = toml_edit::value(alias.clone());
    write_config(&path, &doc).unwrap_or_else(|e| fail(fmt, e));
    if matches!(fmt, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({"status":"ok","active_alias":alias,"client_model":selected.client_model,"context_window":selected.context_window,"restart_required":true})
        );
    } else {
        println!("✓ active alias: {} ({})", alias, selected.client_model);
        println!("  restart required for a running daemon");
    }
}

pub async fn health(fmt: OutputFormat, explicit: Option<String>) {
    let path = config_path(explicit);
    let reg = registry(&path).unwrap_or_else(|e| fail(fmt, e));
    let client = reqwest::Client::new();
    let mut values = Vec::new();
    for provider in reg.providers() {
        let start = std::time::Instant::now();
        let result = client.get(&provider.base_url).send().await;
        values.push(serde_json::json!({"id":provider.id,"state":if result.as_ref().is_ok_and(|r| r.status().is_success() || r.status().is_client_error()) {"healthy"} else {"unavailable"},"latency_ms":start.elapsed().as_millis(),"error":result.err().map(|e|e.to_string())}));
    }
    if matches!(fmt, OutputFormat::Json) {
        println!("{}", serde_json::json!({"providers":values}));
    } else {
        for value in values {
            println!(
                "{}: {} ({}ms)",
                value["id"], value["state"], value["latency_ms"]
            );
        }
    }
}
