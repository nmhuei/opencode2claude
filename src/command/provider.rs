//! Provider-management command services.

use super::{emit, emit_error, CommandError};
use crate::cli::{
    ModelDiscoverArgs, ModelShowArgs, ModelSubcommand, ModelVerifyArgs, ProviderAddArgs,
    ProviderAliasCommand, ProviderAliasRemoveArgs, ProviderAliasSetArgs, ProviderAliasShowArgs,
    ProviderCredentialCommand, ProviderCredentialRemoveArgs, ProviderCredentialSetArgs,
    ProviderCredentialTestArgs, ProviderModelAddArgs, ProviderModelCommand,
    ProviderModelRemoveArgs, ProviderRemoveArgs, ProviderShowArgs, ProviderSubcommand,
    ProviderToggleArgs, RouteCommand, RouteExplainArgs, RouteSimulateArgs, RouteTestArgs,
};
use crate::config::{BridgeConfig, CliOverrides, SecretString};
use crate::output::OutputFormat;
use crate::provider::adapters::{AdapterRegistry, FailureClass};
use crate::provider::credentials::default_store;
use crate::provider::routing::RoutePlanner;
use crate::provider::types::{
    auto_compact_window, AliasId, AuthScheme, Credential, ModelAlias, ModelCandidate, ModelInfo,
    Provider, ProviderKind, ProviderProtocol, SecretSource,
};
use crate::provider::{ProviderConfigStore, ProviderMutation};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

pub async fn run_provider(
    command: ProviderSubcommand,
    global_config: Option<String>,
    fmt: OutputFormat,
) {
    let result = match command {
        ProviderSubcommand::List(args) => list(args.config.or(global_config)),
        ProviderSubcommand::Show(args) => show_provider(args, global_config),
        ProviderSubcommand::Add(args) => add(args, global_config),
        ProviderSubcommand::Remove(args) => remove(args, global_config),
        ProviderSubcommand::Enable(args) => toggle_provider(args, true, global_config),
        ProviderSubcommand::Disable(args) => toggle_provider(args, false, global_config),
        ProviderSubcommand::Credential(args) => {
            run_credentials_value(args.command, global_config).map(Some)
        }
        ProviderSubcommand::Alias(args) => run_alias_value(args.command, global_config).map(Some),
        ProviderSubcommand::Model(args) => run_models(args.command, global_config).map(Some),
        ProviderSubcommand::Activate(args) => activate(args.alias, args.config.or(global_config)),
        ProviderSubcommand::Use(args) => activate(args.alias, args.config.or(global_config)),
        ProviderSubcommand::Health(args) => {
            health(args.config.or(global_config), args.provider).await
        }
        ProviderSubcommand::Test(args) => {
            direct_health(args.config.or(global_config), args.provider).await
        }
        ProviderSubcommand::Opencode(_)
        | ProviderSubcommand::Api(_)
        | ProviderSubcommand::Models(_)
        | ProviderSubcommand::Status(_) => Ok(None),
    };
    match result {
        Ok(Some((title, quiet, value))) => emit(fmt, &title, &quiet, value),
        Ok(None) => {}
        Err(error) => emit_error(fmt, &error),
    }
}

pub async fn run_model(command: ModelSubcommand, global_config: Option<String>, fmt: OutputFormat) {
    let result = match command {
        ModelSubcommand::List(args) => list_models(args.config.or(global_config)),
        ModelSubcommand::Show(args) => show_model(args, global_config),
        ModelSubcommand::Discover(args) => discover_models(args, global_config).await,
        ModelSubcommand::Verify(args) => verify_model(args, global_config).await,
        ModelSubcommand::Set(_) | ModelSubcommand::Status => Ok(None),
    };
    match result {
        Ok(Some((title, quiet, value))) => emit(fmt, &title, &quiet, value),
        Ok(None) => {}
        Err(error) => emit_error(fmt, &error),
    }
}

pub async fn run_route(command: RouteCommand, global_config: Option<String>, fmt: OutputFormat) {
    let result = match command {
        RouteCommand::Explain(args) => explain(args, global_config),
        RouteCommand::Test(args) => test(args, global_config).await,
        RouteCommand::Simulate(args) => simulate(args, global_config),
    };
    match result {
        Ok((title, quiet, value)) => emit(fmt, &title, &quiet, value),
        Err(error) => emit_error(fmt, &error),
    }
}

pub async fn run_credentials(
    command: ProviderCredentialCommand,
    global_config: Option<String>,
    fmt: OutputFormat,
) {
    let result = run_credentials_value(command, global_config);
    match result {
        Ok((title, quiet, value)) => emit(fmt, &title, &quiet, value),
        Err(error) => emit_error(fmt, &error),
    }
}

pub async fn run_aliases(
    command: ProviderAliasCommand,
    global_config: Option<String>,
    fmt: OutputFormat,
) {
    let result = run_alias_value(command, global_config);
    match result {
        Ok((title, quiet, value)) => emit(fmt, &title, &quiet, value),
        Err(error) => emit_error(fmt, &error),
    }
}

pub async fn run_health(
    provider: Option<String>,
    global_config: Option<String>,
    fmt: OutputFormat,
    watch: bool,
) {
    if watch {
        loop {
            emit_health(provider.clone(), global_config.clone(), fmt).await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    emit_health(provider, global_config, fmt).await;
}

async fn emit_health(provider: Option<String>, global_config: Option<String>, fmt: OutputFormat) {
    let result = health(global_config, provider).await;
    match result {
        Ok(Some((title, quiet, value))) => emit(fmt, &title, &quiet, value),
        Ok(None) => {}
        Err(error) => emit_error(fmt, &error),
    }
}

type CommandValue = Result<Option<(String, String, Value)>, CommandError>;
type ValueResult = Result<(String, String, Value), CommandError>;

fn config_path(explicit: Option<String>) -> PathBuf {
    explicit.map(PathBuf::from).unwrap_or_else(|| {
        BridgeConfig::from_env_and_cli(CliOverrides::default())
            .management
            .config_path
    })
}

fn store(explicit: Option<String>) -> ProviderConfigStore {
    ProviderConfigStore::open(config_path(explicit))
}

fn list(explicit: Option<String>) -> CommandValue {
    let registry = store(explicit).load().map_err(store_error)?;
    let providers: Vec<Value> = registry
        .providers()
        .map(|provider| {
            json!({
                "id": provider.id,
                "name": provider.name,
                "kind": provider.kind,
                "base_url": provider.base_url,
                "protocol": provider.protocol,
                "enabled": provider.enabled,
            })
        })
        .collect();
    let quiet = providers
        .first()
        .and_then(|value| value["id"].as_str())
        .unwrap_or("")
        .to_string();
    Ok(Some((
        "Providers".to_string(),
        quiet,
        json!({"providers": providers, "active_alias": registry.active_alias()}),
    )))
}

fn show_provider(args: ProviderShowArgs, global_config: Option<String>) -> CommandValue {
    let registry = store(args.config.or(global_config))
        .load()
        .map_err(store_error)?;
    let provider = registry
        .provider(&args.id)
        .ok_or_else(|| format!("provider {} not found", args.id))?;
    let credential_count = registry
        .credentials()
        .filter(|credential| credential.provider_id == provider.id)
        .count();
    let model_count = registry
        .models()
        .filter(|model| model.provider_id == provider.id)
        .count();
    Ok(Some((
        "Provider".to_string(),
        provider.id.to_string(),
        json!({
            "id": provider.id,
            "name": provider.name,
            "kind": provider.kind,
            "base_url": provider.base_url,
            "protocol": provider.protocol,
            "enabled": provider.enabled,
            "credential_count": credential_count,
            "model_count": model_count,
        }),
    )))
}

fn list_models(explicit: Option<String>) -> CommandValue {
    let registry = store(explicit).load().map_err(store_error)?;
    let models: Vec<Value> = registry
        .models()
        .map(|model| {
            json!({
                "provider": model.provider_id,
                "model": model.model_id,
                "wire_model": model.wire_model_id,
                "context_window": model.context_window,
                "auto_compact_window": model.context_window.map(auto_compact_window),
                "max_output_tokens": model.max_output_tokens,
                "supports_thinking": model.supports_thinking,
                "verified_context": model.verified_context,
                "free": model.free,
            })
        })
        .collect();
    Ok(Some((
        "Configured models".to_string(),
        models
            .first()
            .and_then(|value| value["model"].as_str())
            .unwrap_or_default()
            .to_string(),
        json!({"models": models}),
    )))
}

fn show_model(args: ModelShowArgs, global_config: Option<String>) -> CommandValue {
    let registry = store(args.config.or(global_config))
        .load()
        .map_err(store_error)?;
    if let Some(query) = args.model {
        let (provider, model) = query
            .split_once(':')
            .map(|(provider, model)| (Some(provider), model))
            .unwrap_or((None, query.as_str()));
        let value = registry
            .models()
            .find(|entry| {
                entry.model_id == model
                    && provider.is_none_or(|provider| entry.provider_id.as_ref() == provider)
            })
            .ok_or_else(|| format!("model {} not found", query))?;
        return Ok(Some((
            "Model".to_string(),
            value.model_id.clone(),
            model_value(value),
        )));
    }
    let alias_id = registry
        .active_alias()
        .ok_or_else(|| "no active alias configured".to_string())?;
    let alias = registry
        .alias(alias_id)
        .ok_or_else(|| format!("active alias {} not found", alias_id))?;
    Ok(Some((
        "Active model alias".to_string(),
        alias.id.to_string(),
        json!({"alias": alias.id, "client_model": alias.client_model, "context_window": alias.context_window, "auto_compact_window": alias.auto_compact_window(), "candidates": alias.candidates}),
    )))
}

async fn verify_model(args: ModelVerifyArgs, global_config: Option<String>) -> CommandValue {
    let path = config_path(args.config.or(global_config));
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    let provider = registry
        .provider(&args.provider)
        .ok_or_else(|| format!("provider {} not found", args.provider))?
        .clone();
    let model = registry
        .model(&args.provider, &args.model)
        .ok_or_else(|| format!("model {}:{} not found", args.provider, args.model))?
        .clone();
    let entries = fetch_provider_catalog(&path, &registry, &provider).await?;
    let remote = entries.iter().find(|entry| {
        entry.as_str() == Some(args.model.as_str())
            || entry
                .get("id")
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                == Some(args.model.as_str())
    });
    let Some(remote) = remote else {
        return Err(format!(
            "provider {} did not advertise model {}",
            args.provider, args.model
        )
        .into());
    };
    let remote = model_metadata(remote)?;
    let context_window = remote.context_window.ok_or_else(|| {
        format!(
            "provider {} did not report context metadata for model {}",
            args.provider, args.model
        )
    })?;
    let strict_requirement = registry
        .aliases()
        .filter(|alias| alias.strict_context)
        .filter(|alias| {
            alias.candidates.iter().any(|candidate| {
                candidate.provider_id.as_ref() == args.provider && candidate.model_id == args.model
            })
        })
        .map(|alias| alias.context_window)
        .max()
        .unwrap_or(0);
    if context_window < strict_requirement {
        return Err(format!(
            "provider reports {} context tokens for {} but strict aliases require {}",
            context_window, args.model, strict_requirement
        )
        .into());
    }
    let mut verified = model;
    verified.context_window = Some(context_window);
    if let Some(max_output) = remote.max_output_tokens {
        verified.max_output_tokens = Some(max_output);
    }
    if let Some(supports_thinking) = remote.supports_thinking {
        verified.supports_thinking = supports_thinking;
    }
    verified.verified_context = true;
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::UpsertModel(verified))
        .map_err(store_error)?;
    Ok(Some((
        "Model verified".to_string(),
        args.model.clone(),
        json!({"provider": args.provider, "model": args.model, "context_window": context_window, "auto_compact_window": auto_compact_window(context_window), "verified_context": true, "source": "provider_catalog", "restart_required": true}),
    )))
}

async fn discover_models(args: ModelDiscoverArgs, global_config: Option<String>) -> CommandValue {
    let path = config_path(args.config.or(global_config));
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    let provider = registry
        .provider(&args.provider)
        .ok_or_else(|| format!("provider {} not found", args.provider))?
        .clone();
    let entries = fetch_provider_catalog(&path, &registry, &provider).await?;
    if entries.is_empty() {
        return Err(format!("provider {} returned no models", args.provider).into());
    }
    let mut discovered = 0usize;
    for entry in &entries {
        let metadata = model_metadata(entry)?;
        let model_id = metadata.id;
        let context_window = metadata.context_window;
        let max_output_tokens = metadata.max_output_tokens;
        let supports_thinking = metadata.supports_thinking;
        let existing = registry.model(&args.provider, &model_id).cloned();
        let model = ModelInfo {
            provider_id: args.provider.clone().into(),
            model_id: model_id.clone(),
            wire_model_id: existing
                .as_ref()
                .map(|model| model.wire_model_id.clone())
                .unwrap_or_else(|| model_id.clone()),
            context_window,
            max_output_tokens: max_output_tokens
                .or_else(|| existing.as_ref().and_then(|model| model.max_output_tokens)),
            supports_thinking: supports_thinking.unwrap_or_else(|| {
                existing
                    .as_ref()
                    .is_some_and(|model| model.supports_thinking)
            }),
            verified_context: false,
            free: existing.is_some_and(|model| model.free),
        };
        ProviderConfigStore::open(&path)
            .transaction(ProviderMutation::UpsertModel(model))
            .map_err(store_error)?;
        discovered += 1;
    }
    Ok(Some((
        "Models discovered".to_string(),
        args.provider.clone(),
        json!({"provider": args.provider, "discovered": discovered, "catalog_endpoint": format!("{}/models", provider.base_url.trim_end_matches('/')), "verified_context": false, "restart_required": true}),
    )))
}

async fn fetch_provider_catalog(
    path: &std::path::Path,
    registry: &crate::provider::ProviderRegistry,
    provider: &Provider,
) -> Result<Vec<Value>, CommandError> {
    let (auth_scheme, secret) = discovery_credential(registry, path, provider.id.as_ref())?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| format!("failed to create provider catalog client: {error}"))?;
    let mut request = client.get(format!(
        "{}/models",
        provider.base_url.trim_end_matches('/')
    ));
    for (name, value) in &provider.headers {
        request = request.header(name, value);
    }
    match auth_scheme {
        AuthScheme::Bearer => {
            if let Some(secret) = secret.as_ref() {
                request = request.bearer_auth(secret.expose());
            }
        }
        AuthScheme::XApiKey => {
            if let Some(secret) = secret.as_ref() {
                request = request.header("x-api-key", secret.expose());
            }
        }
        AuthScheme::None => {}
    }
    let response = request.send().await.map_err(|error| {
        format!(
            "provider catalog request failed for {}: {}",
            provider.id,
            safe_error(&error.to_string())
        )
    })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("provider catalog response failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "provider catalog for {} returned HTTP {}",
            provider.id, status
        )
        .into());
    }
    let document: Value = serde_json::from_str(&body)
        .map_err(|error| format!("provider catalog returned invalid JSON: {error}"))?;
    let entries = document
        .get("data")
        .or_else(|| document.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| "provider catalog response has no data/models array".to_string())?;
    if entries.is_empty() {
        return Err(format!("provider {} returned no models", provider.id).into());
    }
    Ok(entries.clone())
}

fn discovery_credential(
    registry: &crate::provider::ProviderRegistry,
    path: &std::path::Path,
    provider: &str,
) -> Result<(AuthScheme, Option<SecretString>), CommandError> {
    let Some(credential) = registry
        .credentials()
        .find(|credential| credential.provider_id.as_ref() == provider)
    else {
        return Ok((AuthScheme::None, None));
    };
    if credential.auth_scheme == AuthScheme::None {
        return Ok((AuthScheme::None, None));
    }
    let secret = default_store(path)
        .resolve(credential)
        .map_err(|error| format!("credential {} cannot be resolved: {error}", credential.id))?;
    Ok((credential.auth_scheme, Some(secret)))
}

#[derive(Debug, Clone)]
struct DiscoveredModelMetadata {
    id: String,
    context_window: Option<usize>,
    max_output_tokens: Option<usize>,
    supports_thinking: Option<bool>,
}

fn model_metadata(entry: &Value) -> Result<DiscoveredModelMetadata, CommandError> {
    if let Some(model_id) = entry.as_str() {
        return Ok(DiscoveredModelMetadata {
            id: model_id.to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_thinking: None,
        });
    }
    let object = entry
        .as_object()
        .ok_or_else(|| "model discovery item must be a string or object".to_string())?;
    let model_id = object
        .get("id")
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| "model discovery item has no id".to_string())?;
    let number = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| object.get(*key).and_then(Value::as_u64))
            .and_then(|value| usize::try_from(value).ok())
    };
    let supports_thinking = ["supports_thinking", "reasoning", "thinking"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_bool));
    Ok(DiscoveredModelMetadata {
        id: model_id.to_string(),
        context_window: number(&["context_window", "context_length", "max_input_tokens"]),
        max_output_tokens: number(&["max_output_tokens", "max_tokens"]),
        supports_thinking,
    })
}

fn model_value(model: &ModelInfo) -> Value {
    json!({
        "provider": model.provider_id,
        "model": model.model_id,
        "wire_model": model.wire_model_id,
        "context_window": model.context_window,
        "auto_compact_window": model.context_window.map(auto_compact_window),
        "max_output_tokens": model.max_output_tokens,
        "supports_thinking": model.supports_thinking,
        "verified_context": model.verified_context,
        "free": model.free,
    })
}

fn add(args: ProviderAddArgs, global_config: Option<String>) -> CommandValue {
    let id = non_empty(&args.id, "provider id")?;
    let base_url = validate_url(&args.url)?;
    let provider = Provider {
        id: id.clone().into(),
        name: id.clone(),
        kind: parse_kind(&args.kind)?,
        base_url,
        protocol: parse_protocol(&args.protocol)?,
        headers: BTreeMap::new(),
        enabled: true,
    };
    let path = config_path(args.config.or(global_config));
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::AddProvider(provider))
        .map_err(store_error)?;
    Ok(Some((
        "Provider added".to_string(),
        id.clone(),
        json!({"provider": id, "config_file": path.display().to_string(), "restart_required": true}),
    )))
}

fn remove(args: ProviderRemoveArgs, global_config: Option<String>) -> CommandValue {
    let id = args.id.clone();
    let path = config_path(args.config.or(global_config));
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::RemoveProvider(id.clone()))
        .map_err(store_error)?;
    Ok(Some((
        "Provider removed".to_string(),
        id.clone(),
        json!({"provider": id, "config_file": path.display().to_string(), "restart_required": true}),
    )))
}

fn toggle_provider(
    args: ProviderToggleArgs,
    enabled: bool,
    global_config: Option<String>,
) -> CommandValue {
    let id = args.id.clone();
    let path = config_path(args.config.or(global_config));
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::EnableProvider {
            id: id.clone(),
            enabled,
        })
        .map_err(store_error)?;
    Ok(Some((
        if enabled {
            "Provider enabled"
        } else {
            "Provider disabled"
        }
        .to_string(),
        id.clone(),
        json!({"provider": id, "enabled": enabled, "config_file": path.display().to_string(), "restart_required": true}),
    )))
}

fn run_credentials_value(
    command: ProviderCredentialCommand,
    global_config: Option<String>,
) -> ValueResult {
    match command {
        ProviderCredentialCommand::List(args) => {
            let path = config_path(args.config.or(global_config));
            let registry = ProviderConfigStore::open(&path)
                .load()
                .map_err(store_error)?;
            let values: Vec<Value> = registry
                .credentials()
                .map(|credential| {
                    let available = credential.auth_scheme == AuthScheme::None
                        || default_store(&path).resolve(credential).is_ok();
                    json!({"id": credential.id, "provider": credential.provider_id, "source": credential.source, "auth_scheme": credential.auth_scheme, "available": available})
                })
                .collect();
            let quiet = values
                .first()
                .and_then(|value| value["id"].as_str())
                .unwrap_or("")
                .to_string();
            Ok((
                "Credentials".to_string(),
                quiet,
                json!({"credentials": values}),
            ))
        }
        ProviderCredentialCommand::Set(args) => set_credential(args, global_config),
        ProviderCredentialCommand::Remove(args) => remove_credential(args, global_config),
        ProviderCredentialCommand::Test(args) => test_credential(args, global_config),
    }
}

fn test_credential(args: ProviderCredentialTestArgs, global_config: Option<String>) -> ValueResult {
    let path = config_path(args.config.or(global_config));
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    let credential = registry
        .credentials()
        .find(|credential| {
            credential.id.as_ref() == args.id && credential.provider_id.as_ref() == args.provider
        })
        .ok_or_else(|| {
            format!(
                "credential {} for provider {} not found",
                args.id, args.provider
            )
        })?;
    default_store(&path)
        .resolve(credential)
        .map_err(|error| format!("credential {} cannot be resolved: {error}", args.id))?;
    Ok((
        "Credential verified".to_string(),
        args.id.clone(),
        json!({"credential": args.id, "provider": args.provider, "source": credential.source, "resolved": true}),
    ))
}

fn set_credential(args: ProviderCredentialSetArgs, global_config: Option<String>) -> ValueResult {
    let path = config_path(args.config.or(global_config));
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    if registry.provider(&args.provider).is_none() {
        return Err(format!("provider {} not found", args.provider).into());
    }
    let auth_scheme = parse_auth_scheme(&args.auth_scheme)?;
    let (source, secret) = if let Some(env) = args.env {
        (SecretSource::Env { variable: env }, None)
    } else if args.api_key_stdin {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .map_err(|error| format!("failed to read credential from stdin: {error}"))?;
        let value = value.trim().to_string();
        if value.is_empty() {
            return Err("credential read from stdin is empty".into());
        }
        (
            SecretSource::Managed {
                id: args.id.clone(),
            },
            Some(value),
        )
    } else {
        return Err("provide --env NAME or --api-key-stdin".into());
    };
    let credential = Credential {
        id: args.id.clone().into(),
        provider_id: args.provider.clone().into(),
        source,
        auth_scheme,
    };
    let old_managed_id = registry
        .credentials()
        .find(|existing| existing.id.as_ref() == args.id)
        .and_then(|existing| match &existing.source {
            SecretSource::Managed { id } => Some(id.clone()),
            SecretSource::Env { .. } | SecretSource::File { .. } => None,
        });
    let secrets = default_store(&path);
    if let Some(value) = secret {
        // Stage managed bytes before publishing the TOML reference. If the
        // validated config transaction fails, restore the previous value (or
        // remove the staged entry) so a config never points at missing data.
        let previous_same_id = registry
            .credentials()
            .find(|existing| existing.id.as_ref() == args.id)
            .filter(|_| old_managed_id.as_deref() == Some(args.id.as_str()))
            .and_then(|existing| secrets.resolve(existing).ok())
            .map(|value| value.expose().to_string());
        secrets
            .put_file_secret(&args.id, &value)
            .map_err(|error| CommandError::Message(error.to_string()))?;
        if let Err(error) = ProviderConfigStore::open(&path)
            .transaction(ProviderMutation::SetCredential(credential))
            .map_err(store_error)
        {
            match previous_same_id {
                Some(previous) => {
                    let _ = secrets.put_file_secret(&args.id, &previous);
                }
                None => {
                    let _ = secrets.remove(&args.id);
                }
            }
            return Err(error);
        }
    } else {
        ProviderConfigStore::open(&path)
            .transaction(ProviderMutation::SetCredential(credential))
            .map_err(store_error)?;
    }
    let stale_secret_warning = old_managed_id
        .filter(|id| id != &args.id)
        .and_then(|id| secrets.remove(id).err().map(|error| error.to_string()));
    Ok((
        "Credential configured".to_string(),
        args.id.clone(),
        json!({"credential": args.id, "provider": args.provider, "secret": "configured", "warning": stale_secret_warning, "restart_required": true}),
    ))
}

fn remove_credential(
    args: ProviderCredentialRemoveArgs,
    global_config: Option<String>,
) -> ValueResult {
    let id = args.id.clone();
    let path = config_path(args.config.or(global_config));
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::RemoveCredential(id.clone()))
        .map_err(store_error)?;
    default_store(&path)
        .remove(&id)
        .map_err(|error| error.to_string())?;
    Ok((
        "Credential removed".to_string(),
        id.clone(),
        json!({"credential": id, "restart_required": true}),
    ))
}

fn run_alias_value(command: ProviderAliasCommand, global_config: Option<String>) -> ValueResult {
    match command {
        ProviderAliasCommand::List(args) => {
            let registry = store(args.config.or(global_config))
                .load()
                .map_err(store_error)?;
            let values: Vec<Value> = registry
                .aliases()
                .map(|alias| {
                    json!({"id": alias.id, "client_model": alias.client_model, "context_window": alias.context_window, "auto_compact_window": alias.auto_compact_window(), "strict_context": alias.strict_context, "candidates": alias.candidates})
                })
                .collect();
            Ok((
                "Aliases".to_string(),
                values
                    .first()
                    .map(|v| v["id"].as_str().unwrap_or_default())
                    .unwrap_or_default()
                    .to_string(),
                json!({"aliases": values, "active_alias": registry.active_alias()}),
            ))
        }
        ProviderAliasCommand::Show(args) => show_alias(args, global_config),
        ProviderAliasCommand::Set(args) => set_alias(args, global_config),
        ProviderAliasCommand::Remove(args) => remove_alias(args, global_config),
        ProviderAliasCommand::Use(args) => activate(args.alias, args.config.or(global_config))
            .map(|value| value.expect("activation always returns a command value")),
    }
}

fn show_alias(args: ProviderAliasShowArgs, global_config: Option<String>) -> ValueResult {
    let registry = store(args.config.or(global_config))
        .load()
        .map_err(store_error)?;
    let alias = registry
        .alias(&args.id)
        .ok_or_else(|| format!("alias {} not found", args.id))?;
    Ok((
        "Alias".to_string(),
        alias.id.to_string(),
        json!({"id": alias.id, "client_model": alias.client_model, "context_window": alias.context_window, "auto_compact_window": alias.auto_compact_window(), "strict_context": alias.strict_context, "candidates": alias.candidates}),
    ))
}

fn set_alias(args: ProviderAliasSetArgs, global_config: Option<String>) -> ValueResult {
    if args.candidate.is_empty() {
        return Err("at least one --candidate provider:model[:credential] is required".into());
    }
    let candidates = args
        .candidate
        .iter()
        .enumerate()
        .map(|(priority, value)| parse_candidate(value, priority as i32))
        .collect::<Result<Vec<_>, _>>()?;
    let path = config_path(args.config.or(global_config));
    let alias = ModelAlias {
        id: args.id.clone().into(),
        client_model: args.client_model,
        context_window: args.context_window,
        strict_context: args.context_window >= 1_000_000,
        candidates,
    };
    let expected = alias.auto_compact_window();
    let registry = ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::SetAlias(alias))
        .map_err(store_error)?;
    let alias = registry.alias(&args.id).expect("validated alias persisted");
    Ok((
        "Alias configured".to_string(),
        alias.id.to_string(),
        json!({"alias": alias.id, "client_model": alias.client_model, "context_window": alias.context_window, "auto_compact_window": expected, "strict_context": alias.strict_context, "restart_required": true}),
    ))
}

fn remove_alias(args: ProviderAliasRemoveArgs, global_config: Option<String>) -> ValueResult {
    let id = args.id.clone();
    let path = config_path(args.config.or(global_config));
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::RemoveAlias(id.clone()))
        .map_err(store_error)?;
    Ok((
        "Alias removed".to_string(),
        id.clone(),
        json!({"alias": id, "restart_required": true}),
    ))
}

fn run_models(command: ProviderModelCommand, global_config: Option<String>) -> ValueResult {
    match command {
        ProviderModelCommand::Add(args) => add_model(args, global_config),
        ProviderModelCommand::Remove(args) => remove_model(args, global_config),
    }
}

fn add_model(args: ProviderModelAddArgs, global_config: Option<String>) -> ValueResult {
    let path = config_path(args.config.or(global_config));
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    if registry.provider(&args.provider).is_none() {
        return Err(format!("provider {} not found", args.provider).into());
    }
    let model = ModelInfo {
        provider_id: args.provider.clone().into(),
        model_id: args.model.clone(),
        wire_model_id: args.wire_model.unwrap_or_else(|| args.model.clone()),
        context_window: Some(args.context_window),
        max_output_tokens: args.max_output_tokens,
        supports_thinking: false,
        verified_context: true,
        free: args.free,
    };
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::UpsertModel(model))
        .map_err(store_error)?;
    Ok((
        "Model configured".to_string(),
        args.model.clone(),
        json!({"provider": args.provider, "model": args.model, "context_window": args.context_window, "auto_compact_window": auto_compact_window(args.context_window), "restart_required": true}),
    ))
}

fn remove_model(args: ProviderModelRemoveArgs, global_config: Option<String>) -> ValueResult {
    let path = config_path(args.config.or(global_config));
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::RemoveModel {
            provider: args.provider.clone(),
            model: args.model.clone(),
        })
        .map_err(store_error)?;
    Ok((
        "Model removed".to_string(),
        args.model.clone(),
        json!({"provider": args.provider, "model": args.model, "restart_required": true}),
    ))
}

fn activate(alias: String, explicit: Option<String>) -> CommandValue {
    let path = config_path(explicit);
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    let alias_obj = registry
        .alias(&alias)
        .ok_or_else(|| format!("alias {alias} not found"))?;
    let client_model = alias_obj.client_model.clone();
    let context_window = alias_obj.context_window;
    ProviderConfigStore::open(&path)
        .transaction(ProviderMutation::SetActiveAlias(Some(AliasId::from(
            alias.clone(),
        ))))
        .map_err(store_error)?;
    Ok(Some((
        "Active alias changed".to_string(),
        alias.clone(),
        json!({"active_alias": alias, "client_model": client_model, "context_window": context_window, "auto_compact_window": auto_compact_window(context_window), "restart_required": true}),
    )))
}

async fn health(explicit: Option<String>, only: Option<String>) -> CommandValue {
    let path = config_path(explicit.clone());
    let resolved = BridgeConfig::from_env_and_cli(CliOverrides {
        config_path: Some(path.display().to_string()),
        ..Default::default()
    });
    if let Some(token) = resolved.management.rest_token() {
        let endpoint = format!(
            "http://{}:{}/api/v1/provider-runtime",
            resolved.host, resolved.bridge_port
        );
        if let Ok(response) = reqwest::Client::new()
            .get(endpoint)
            .bearer_auth(token)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(mut runtime) = response.json::<Value>().await {
                    if let Some(provider) = only.as_deref() {
                        if let Some(values) = runtime["providers"].as_array_mut() {
                            values.retain(|value| value["id"].as_str() == Some(provider));
                        }
                    }
                    return Ok(Some((
                        "Daemon provider health".to_string(),
                        only.unwrap_or_else(|| "daemon".to_string()),
                        json!({"source": "daemon", "runtime": runtime}),
                    )));
                }
            }
        }
    }
    direct_health(Some(path.display().to_string()), only).await
}

async fn direct_health(explicit: Option<String>, only: Option<String>) -> CommandValue {
    let path = config_path(explicit);
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(store_error)?;
    let client = reqwest::Client::new();
    let mut values = Vec::new();
    for provider in registry
        .providers()
        .filter(|p| only.as_deref().is_none_or(|id| id == p.id.as_ref()))
    {
        let started = std::time::Instant::now();
        let response = probe_provider(&client, &registry, &path, provider).await;
        let status = response.as_ref().ok().map(|(status, _)| *status);
        let failure = response.as_ref().ok().and_then(|(status, headers)| {
            (!status.is_success()).then(|| {
                AdapterRegistry::for_provider(provider.kind).classify_failure(
                    Some(*status),
                    headers,
                    "",
                )
            })
        });
        values.push(json!({
            "provider": provider.id,
            "endpoint": format!("{}/models", provider.base_url.trim_end_matches('/')),
            "state": status
                .map(|status| health_state(status, failure.unwrap_or(FailureClass::Unknown)))
                .unwrap_or("unavailable"),
            "status": status.map(|s| s.as_u16()),
                "failure_class": failure.map(|failure| format!("{failure:?}")),
            "retry_after_ms": response
                .as_ref()
                .ok()
                .and_then(|(_, headers)| retry_after_ms(headers)),
            "latency_ms": started.elapsed().as_millis(),
            "error": response.err().map(|error| safe_error(&error)),
        }));
    }
    Ok(Some((
        "Provider health".to_string(),
        values
            .first()
            .map(|v| v["provider"].as_str().unwrap_or_default())
            .unwrap_or_default()
            .to_string(),
        json!({"source": "direct", "providers": values}),
    )))
}

async fn probe_provider(
    client: &reqwest::Client,
    registry: &crate::provider::ProviderRegistry,
    path: &std::path::Path,
    provider: &Provider,
) -> Result<(StatusCode, reqwest::header::HeaderMap), String> {
    let (auth_scheme, secret) = discovery_credential(registry, path, provider.id.as_ref())
        .map_err(|error| error.to_string())?;
    let mut request = client.get(format!(
        "{}/models",
        provider.base_url.trim_end_matches('/')
    ));
    for (name, value) in &provider.headers {
        request = request.header(name, value);
    }
    match auth_scheme {
        AuthScheme::Bearer => {
            if let Some(secret) = secret.as_ref() {
                request = request.bearer_auth(secret.expose());
            }
        }
        AuthScheme::XApiKey => {
            if let Some(secret) = secret.as_ref() {
                request = request.header("x-api-key", secret.expose());
            }
        }
        AuthScheme::None => {}
    }
    let response = request
        .send()
        .await
        .map_err(|error| safe_error(&error.to_string()))?;
    let status = response.status();
    let headers = response.headers().clone();
    Ok((status, headers))
}

fn health_state(status: StatusCode, failure: FailureClass) -> &'static str {
    if status.is_success() {
        return "healthy";
    }
    match failure {
        FailureClass::CredentialRejected => "auth_error",
        FailureClass::RateLimit => "rate_limited",
        FailureClass::ModelUnavailable => "model_unavailable",
        FailureClass::ProviderServer | FailureClass::Transport => "unavailable",
        FailureClass::PaymentRequired => "billing_error",
        FailureClass::ClientRequest => "client_error",
        FailureClass::Unknown => "unknown",
    }
}

fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.min(300) * 1_000)
}

fn explain(args: RouteExplainArgs, global_config: Option<String>) -> ValueResult {
    let registry = store(args.config.or(global_config))
        .load()
        .map_err(store_error)?;
    let alias = registry
        .alias(&args.alias)
        .ok_or_else(|| format!("alias {} not found", args.alias))?;
    let targets = RoutePlanner::new(&registry)
        .plan_for_context(&args.alias, alias.context_window)
        .map_err(|error| error.to_string())?;
    let all = registry
        .resolve_alias(&args.alias)
        .map_err(|error| error.to_string())?;
    let excluded: Vec<Value> = all
        .iter()
        .filter(|target| !targets.iter().any(|candidate| candidate.candidate_index == target.candidate_index))
        .map(|target| json!({"candidate_index": target.candidate_index, "provider": target.provider_id, "model": target.model_id, "reason": "insufficient or unknown context"}))
        .collect();
    let selected: Vec<Value> = targets
        .iter()
        .map(|target| json!({"candidate_index": target.candidate_index, "provider": target.provider_id, "model": target.model_id, "wire_model": target.wire_model_id, "credential": target.credential_id, "context_window": target.context_window}))
        .collect();
    Ok((
        "Route explanation".to_string(),
        args.alias,
        json!({"alias": alias.id, "client_model": alias.client_model, "context_window": alias.context_window, "auto_compact_window": alias.auto_compact_window(), "selected": selected, "excluded": excluded}),
    ))
}

async fn test(args: RouteTestArgs, global_config: Option<String>) -> ValueResult {
    let explicit = args.config.or(global_config);
    let registry = store(explicit.clone()).load().map_err(store_error)?;
    let alias = registry
        .alias(&args.alias)
        .ok_or_else(|| format!("alias {} not found", args.alias))?;
    let health = direct_health(explicit, None)
        .await
        .ok()
        .flatten()
        .map(|(_, _, value)| value)
        .unwrap_or_else(|| json!({"providers": []}));
    Ok((
        "Route test".to_string(),
        args.alias,
        json!({"alias": alias.id, "context_window": alias.context_window, "auto_compact_window": alias.auto_compact_window(), "health": health}),
    ))
}

fn simulate(args: RouteSimulateArgs, global_config: Option<String>) -> ValueResult {
    let registry = store(args.config.or(global_config))
        .load()
        .map_err(store_error)?;
    let targets = registry
        .resolve_alias(&args.alias)
        .map_err(|error| error.to_string())?;
    let status =
        StatusCode::from_u16(args.status.unwrap_or(429)).map_err(|_| "invalid HTTP status")?;
    let adapter = AdapterRegistry::for_provider(ProviderKind::OpenAiCompatible);
    let failure = adapter.classify_failure(Some(status), &reqwest::header::HeaderMap::new(), "");
    let provider = args.from.unwrap_or_default();
    let next = targets
        .iter()
        .find(|target| target.provider_id.as_ref() != provider)
        .map(|target| target.provider_id.to_string());
    Ok((
        "Route simulation".to_string(),
        args.alias,
        json!({"status": status.as_u16(), "failure_class": format!("{failure:?}"), "from": provider, "next_provider": next, "target_count": targets.len()}),
    ))
}

fn parse_candidate(value: &str, priority: i32) -> Result<ModelCandidate, CommandError> {
    let parts: Vec<&str> = value.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().take(2).any(|part| part.trim().is_empty()) {
        return Err(format!("invalid candidate {value}; use provider:model[:credential]").into());
    }
    Ok(ModelCandidate {
        provider_id: parts[0].into(),
        model_id: parts[1].to_string(),
        credential_id: parts.get(2).map(|id| (*id).into()),
        priority,
    })
}

fn parse_kind(value: &str) -> Result<ProviderKind, CommandError> {
    match value {
        "opencode" => Ok(ProviderKind::OpenCode),
        "kilo" => Ok(ProviderKind::Kilo),
        "bai" | "b.ai" => Ok(ProviderKind::Bai),
        "openai-compatible" | "generic" => Ok(ProviderKind::OpenAiCompatible),
        _ => Err(format!("unknown provider kind {value}").into()),
    }
}

fn parse_protocol(value: &str) -> Result<ProviderProtocol, CommandError> {
    match value {
        "openai_chat_completions" | "chat" => Ok(ProviderProtocol::OpenAiChatCompletions),
        "openai_responses" | "responses" => Ok(ProviderProtocol::OpenAiResponses),
        "anthropic_messages" | "messages" => Ok(ProviderProtocol::AnthropicMessages),
        _ => Err(format!("unknown provider protocol {value}").into()),
    }
}

fn parse_auth_scheme(value: &str) -> Result<AuthScheme, CommandError> {
    match value {
        "bearer" => Ok(AuthScheme::Bearer),
        "x-api-key" | "x_api_key" => Ok(AuthScheme::XApiKey),
        "none" => Ok(AuthScheme::None),
        _ => Err(format!("unknown auth scheme {value}").into()),
    }
}

fn validate_url(value: &str) -> Result<String, CommandError> {
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
                .into(),
        );
    }
    Ok(value.to_string())
}

fn non_empty(value: &str, name: &str) -> Result<String, CommandError> {
    (!value.trim().is_empty())
        .then(|| value.trim().to_string())
        .ok_or_else(|| format!("{name} must not be empty").into())
}

fn store_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::Message(error.to_string())
}

fn safe_error(value: &str) -> String {
    value
        .replace("Authorization", "authorization")
        .split(" for url")
        .next()
        .unwrap_or(value)
        .to_string()
}
