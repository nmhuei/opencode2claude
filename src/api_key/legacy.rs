//! Legacy `auth_tokens` helpers kept for CLI/config backward compatibility.

use super::types::*;
use crate::config::{BridgeConfig, TomlConfig};
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use std::collections::HashSet;
use std::io;
use std::path::Path;
use toml_edit::{value, Array, DocumentMut, Item, Value};

pub fn generate_api_keys(
    count: usize,
    random_bytes: usize,
    prefix: &str,
) -> Result<Vec<String>, ApiKeyError> {
    if !(1..=20).contains(&count) {
        return Err(ApiKeyError::InvalidCount);
    }
    if !(16..=64).contains(&random_bytes) {
        return Err(ApiKeyError::InvalidSize);
    }

    (0..count)
        .map(|_| {
            crate::infrastructure::random::secure_random_hex(random_bytes)
                .map(|random| format!("{prefix}{random}"))
                .map_err(|error| ApiKeyError::Random(error.to_string()))
        })
        .collect()
}

pub fn merge_auth_tokens(
    existing: &str,
    generated: &[String],
    replace: bool,
) -> Result<String, ApiKeyError> {
    let document = parse_document(existing)?;
    let mut tokens = if replace {
        Vec::new()
    } else {
        existing_tokens(document.get("auth_tokens"))
    };
    tokens.extend(generated.iter().cloned());
    normalize_tokens(&mut tokens);
    render_auth_tokens(document, &tokens)
}

pub fn remove_auth_tokens(existing: &str, indices: &[usize]) -> Result<String, ApiKeyError> {
    if indices.is_empty() {
        return Err(ApiKeyError::InvalidSelection);
    }
    let document = parse_document(existing)?;
    let tokens = existing_tokens(document.get("auth_tokens"));
    let selected: HashSet<usize> = indices.iter().copied().collect();
    if !selected.iter().any(|index| *index < tokens.len()) {
        return Err(ApiKeyError::InvalidSelection);
    }
    let remaining = tokens
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| (!selected.contains(&index)).then_some(token))
        .collect::<Vec<_>>();
    render_auth_tokens(document, &remaining)
}

pub fn save_auth_tokens(
    path: &Path,
    generated: &[String],
    replace: bool,
) -> Result<(), ApiKeyError> {
    save_auth_tokens_with_store(path, generated, replace, &AtomicFileStore)
}

/// Like [`save_auth_tokens`] but routed through an injected file store so the
/// legacy TOML write shares the caller's transactional fault domain.
pub fn save_auth_tokens_with_store(
    path: &Path,
    generated: &[String],
    replace: bool,
    store: &dyn FileStore,
) -> Result<(), ApiKeyError> {
    update_file_with_store(path, store, |existing| {
        merge_auth_tokens(existing, generated, replace)
    })
}

pub fn revoke_auth_tokens(path: &Path, indices: &[usize]) -> Result<(), ApiKeyError> {
    revoke_auth_tokens_with_store(path, indices, &AtomicFileStore)
}

/// Like [`revoke_auth_tokens`] but routed through an injected file store.
pub fn revoke_auth_tokens_with_store(
    path: &Path,
    indices: &[usize],
    store: &dyn FileStore,
) -> Result<(), ApiKeyError> {
    update_file_with_store(path, store, |existing| {
        remove_auth_tokens(existing, indices)
    })
}

pub fn load_auth_tokens(path: &Path) -> Result<Vec<String>, ApiKeyError> {
    let store = AtomicFileStore;
    let existing = match store.read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(ApiKeyError::Read(error)),
    };
    let document = parse_document(&existing)?;
    Ok(existing_tokens(document.get("auth_tokens")))
}

pub fn key_inventory(tokens: &[String], active_key: Option<&str>) -> Vec<ApiKeyMetadata> {
    tokens
        .iter()
        .enumerate()
        .map(|(index, token)| ApiKeyMetadata {
            index,
            fingerprint: fingerprint(token),
            length: token.chars().count(),
            active: active_key.is_some_and(|active| active == token),
        })
        .collect()
}

pub(super) fn import_legacy_records(config: &BridgeConfig, file: &mut ApiKeyRegistryFile) {
    let suppressed: HashSet<&str> = file
        .suppressed_legacy_hashes
        .iter()
        .map(String::as_str)
        .collect();
    let mut existing_hashes: HashSet<String> = file
        .keys
        .iter()
        .map(|record| record.secret_hash.clone())
        .collect();
    let Some(tokens) = &config.auth_tokens else {
        return;
    };
    for (index, token) in tokens.iter().enumerate() {
        // Empty/whitespace-only configured tokens must never become records:
        // SHA256("") would otherwise authenticate the empty credential on
        // every match_secret/verify caller (middleware filters it upstream,
        // but dashboard verify and future callers would still be exposed).
        if token.expose().trim().is_empty() {
            continue;
        }
        let digest = hash_token(token.expose());
        if suppressed.contains(digest.as_str()) || existing_hashes.contains(&digest) {
            continue;
        }
        let id = format!("legacy_{}", &digest[..12]);
        let mut policy = ApiKeyPolicy::default();
        policy.permissions.shell = config.shell_policy.kind() != "disabled";
        existing_hashes.insert(digest.clone());
        file.keys.push(ApiKeyRecord {
            id,
            name: format!("Legacy key {}", index + 1),
            description: Some(
                "Imported from auth_tokens in the main TOML configuration.".to_string(),
            ),
            fingerprint: fingerprint(token.expose()),
            secret_hash: digest,
            status: ApiKeyStatus::Active,
            environment: "production".to_string(),
            created_at: unix_timestamp(),
            expires_at: None,
            source: ApiKeySource::Legacy,
            policy,
        });
    }
}

fn update_file_with_store<F>(
    path: &Path,
    store: &dyn FileStore,
    operation: F,
) -> Result<(), ApiKeyError>
where
    F: FnOnce(&str) -> Result<String, ApiKeyError>,
{
    let existing = match store.read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(ApiKeyError::Read(error)),
    };
    let updated = operation(&existing)?;
    store
        .atomic_write(path, updated.as_bytes(), true)
        .map_err(ApiKeyError::Write)
}

fn parse_document(existing: &str) -> Result<DocumentMut, ApiKeyError> {
    if existing.trim().is_empty() {
        let mut document = DocumentMut::new();
        document["schema_version"] = value(1);
        Ok(document)
    } else {
        Ok(existing.parse::<DocumentMut>()?)
    }
}

fn render_auth_tokens(mut document: DocumentMut, tokens: &[String]) -> Result<String, ApiKeyError> {
    let mut array = Array::new();
    for token in tokens {
        array.push(token.as_str());
    }
    document["auth_tokens"] = Item::Value(Value::Array(array));
    let rendered = document.to_string();
    toml::from_str::<TomlConfig>(&rendered).map_err(ApiKeyError::Validation)?;
    Ok(rendered)
}

fn normalize_tokens(tokens: &mut Vec<String>) {
    let mut seen = HashSet::new();
    tokens.retain(|token| !token.trim().is_empty() && seen.insert(token.clone()));
}

fn existing_tokens(item: Option<&Item>) -> Vec<String> {
    let Some(value) = item.and_then(Item::as_value) else {
        return Vec::new();
    };
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
    }
    value
        .as_str()
        .map(|csv| {
            csv.split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
