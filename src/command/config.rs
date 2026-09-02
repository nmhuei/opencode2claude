//! Configuration inspection and migration command services.

use super::{emit, emit_error, CommandError};
use crate::cli::{ConfigCommand, ConfigMigrateArgs, ConfigShowArgs};
use crate::config::{BridgeConfig, CliOverrides};
use crate::output::OutputFormat;
use crate::provider::ProviderConfigStore;
use serde_json::{json, Value};
use std::path::PathBuf;

pub async fn run(command: ConfigCommand, global_config: Option<String>, fmt: OutputFormat) {
    let result = match command {
        ConfigCommand::Path => path(global_config),
        ConfigCommand::Show(args) => show(args, global_config),
        ConfigCommand::Validate => validate(global_config),
        ConfigCommand::Migrate(args) => migrate(args, global_config),
    };
    match result {
        Ok((title, quiet, value)) => emit(fmt, &title, &quiet, value),
        Err(error) => emit_error(fmt, &error),
    }
}

fn resolve(explicit: Option<String>) -> PathBuf {
    explicit.map(PathBuf::from).unwrap_or_else(|| {
        BridgeConfig::from_env_and_cli(CliOverrides::default())
            .management
            .config_path
    })
}

fn path(global_config: Option<String>) -> Result<(String, String, Value), CommandError> {
    let path = resolve(global_config);
    Ok((
        "Configuration path".to_string(),
        path.display().to_string(),
        json!({"path": path.display().to_string()}),
    ))
}

fn show(
    args: ConfigShowArgs,
    global_config: Option<String>,
) -> Result<(String, String, Value), CommandError> {
    let path = resolve(args.config.or(global_config));
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if args.effective {
        let config = BridgeConfig::from_env_and_cli(CliOverrides {
            config_path: Some(path.display().to_string()),
            ..Default::default()
        });
        return Ok((
            "Effective configuration".to_string(),
            path.display().to_string(),
            json!({
                "config_file": path.display().to_string(),
                "host": config.host.to_string(),
                "bridge_port": config.bridge_port,
                "model": config.model,
                "active_alias": config.active_alias,
                "provider_registry": config.provider_registry.is_some(),
                "source": "cli > environment > file > defaults"
            }),
        ));
    }
    let parsed: toml::Value = raw
        .parse()
        .map_err(|error| format!("invalid TOML: {error}"))?;
    let mut redacted = parsed;
    redact_secrets(&mut redacted);
    Ok((
        "Configuration file".to_string(),
        path.display().to_string(),
        json!({"path": path.display().to_string(), "document": redacted}),
    ))
}

fn redact_secrets(value: &mut toml::Value) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table.iter_mut() {
                if is_secret_key(key) {
                    *value = toml::Value::String("[REDACTED]".to_string());
                } else {
                    redact_secrets(value);
                }
            }
        }
        toml::Value::Array(values) => values.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let compact = key.replace(['-', '_'], "");
    compact.contains("apikey")
        || key == "token"
        || key.ends_with("_token")
        || key == "auth_tokens"
        || compact == "authorization"
        || compact == "cookie"
        || key.contains("secret")
        || key.contains("password")
}

fn validate(global_config: Option<String>) -> Result<(String, String, Value), CommandError> {
    let path = resolve(global_config);
    let registry = ProviderConfigStore::open(&path)
        .load()
        .map_err(|error| error.to_string())?;
    Ok((
        "Configuration valid".to_string(),
        path.display().to_string(),
        json!({"path": path.display().to_string(), "providers": registry.providers().count(), "models": registry.models().count(), "aliases": registry.aliases().count()}),
    ))
}

fn migrate(
    args: ConfigMigrateArgs,
    global_config: Option<String>,
) -> Result<(String, String, Value), CommandError> {
    let path = resolve(args.config.or(global_config));
    if !args.write {
        let registry = ProviderConfigStore::open(&path)
            .load()
            .map_err(|error| error.to_string())?;
        return Ok((
            "Migration preview".to_string(),
            path.display().to_string(),
            json!({"path": path.display().to_string(), "to_version": 3, "providers": registry.providers().count(), "write_required": true}),
        ));
    }
    let report = ProviderConfigStore::open(&path)
        .migrate_to_v3()
        .map_err(|error| error.to_string())?;
    Ok((
        "Configuration migrated".to_string(),
        path.display().to_string(),
        json!({"path": path.display().to_string(), "from_version": report.from_version, "to_version": report.to_version, "backup": report.backup_path.display().to_string(), "restart_required": true}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ConfigShowArgs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn file_view_redacts_secret_values() {
        let path = std::env::temp_dir().join(format!(
            "opencode2api-config-view-{}.toml",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "schema_version = 1\nupstream_api_key = \"top-secret\"\n[search]\napi_token = \"another-secret\"\nmax_output_tokens = 128000\n[provider.headers]\nAuthorization = \"header-secret\"\n",
        )
        .unwrap();
        let (_, _, value) = show(
            ConfigShowArgs {
                config: Some(path.display().to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let output = value.to_string();
        assert!(!output.contains("top-secret"));
        assert!(!output.contains("another-secret"));
        assert!(!output.contains("header-secret"));
        assert_eq!(value["document"]["upstream_api_key"], "[REDACTED]");
        assert_eq!(value["document"]["search"]["max_output_tokens"], 128000);
        let _ = std::fs::remove_file(path);
    }
}
