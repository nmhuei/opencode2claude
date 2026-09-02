//! Provider-scoped secret resolution.

use super::types::{Credential, SecretSource};
use crate::config::SecretString;
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential {0} is not configured")]
    Missing(String),
    #[error("credential file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("credential store is invalid: {0}")]
    Format(#[from] serde_json::Error),
    #[error("OS keychain backend is unavailable")]
    KeychainUnavailable,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ManagedSecret {
    pub provider_id: String,
    #[serde(skip)]
    pub value: String,
}

impl fmt::Debug for ManagedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedSecret")
            .field("provider_id", &self.provider_id)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self, id: impl AsRef<str>) -> PathBuf {
        self.path.with_file_name(format!("{}.json", id.as_ref()))
    }
    pub fn put_file_secret(&self, id: impl AsRef<str>, value: &str) -> Result<(), CredentialError> {
        let path = &self.path;
        let mut values = self.read_values()?;
        values.insert(id.as_ref().to_string(), value.to_string());
        let bytes = serde_json::to_vec_pretty(&values)?;
        AtomicFileStore.atomic_write(path, &bytes, true)?;
        Ok(())
    }
    pub fn put(&self, credential: &Credential, value: &str) -> Result<(), CredentialError> {
        let mut values = self.read_values()?;
        values.insert(credential.id.to_string(), value.to_string());
        let bytes = serde_json::to_vec_pretty(&values)?;
        AtomicFileStore.atomic_write(&self.path, &bytes, true)?;
        Ok(())
    }
    pub fn remove(&self, id: impl AsRef<str>) -> Result<(), CredentialError> {
        let mut values = self.read_values()?;
        values.remove(id.as_ref());
        let bytes = serde_json::to_vec_pretty(&values)?;
        AtomicFileStore.atomic_write(&self.path, &bytes, true)?;
        Ok(())
    }
    pub fn resolve(&self, credential: &Credential) -> Result<SecretString, CredentialError> {
        let value = match &credential.source {
            SecretSource::Env { variable } => std::env::var(variable).ok(),
            SecretSource::File { path } => {
                std::fs::read_to_string(resolve_file_source(&self.path, path))
                    .ok()
                    .map(|v| v.trim().to_string())
            }
            SecretSource::Managed { id } => self.read_values()?.remove(id),
        };
        value
            .and_then(SecretString::new)
            .ok_or_else(|| CredentialError::Missing(credential.id.to_string()))
    }
    fn read_values(&self) -> Result<BTreeMap<String, String>, CredentialError> {
        match AtomicFileStore.read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.into()),
        }
    }
}

fn resolve_file_source(store_path: &Path, source: &str) -> PathBuf {
    let path = Path::new(source);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        store_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

/// Pluggable OS keychain boundary. The default intentionally does not make a
/// platform keychain a prerequisite for headless servers.
pub trait KeychainBackend: Send + Sync {
    fn get(&self, _id: &str) -> Result<SecretString, CredentialError>;
    fn set(&self, _id: &str, _value: &str) -> Result<(), CredentialError>;
    fn remove(&self, _id: &str) -> Result<(), CredentialError>;
}

#[derive(Debug, Default)]
pub struct DisabledKeychain;
impl KeychainBackend for DisabledKeychain {
    fn get(&self, _: &str) -> Result<SecretString, CredentialError> {
        Err(CredentialError::KeychainUnavailable)
    }
    fn set(&self, _: &str, _: &str) -> Result<(), CredentialError> {
        Err(CredentialError::KeychainUnavailable)
    }
    fn remove(&self, _: &str) -> Result<(), CredentialError> {
        Err(CredentialError::KeychainUnavailable)
    }
}

pub fn default_store(config_path: &Path) -> CredentialStore {
    CredentialStore::new(config_path.with_file_name("provider-secrets.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{AuthScheme, ProviderId};
    #[test]
    fn managed_secret_resolves_without_leaking_debug() {
        let root = std::env::temp_dir().join(format!("provider-secrets-{}", std::process::id()));
        let store = CredentialStore::new(&root);
        let credential = Credential {
            id: "bai-main".into(),
            provider_id: ProviderId::from("bai"),
            source: SecretSource::Managed {
                id: "bai-main".into(),
            },
            auth_scheme: AuthScheme::Bearer,
        };
        store.put(&credential, "secret-value").unwrap();
        assert_eq!(store.resolve(&credential).unwrap().expose(), "secret-value");
        assert!(!format!("{credential:?}").contains("secret-value"));
        let _ = std::fs::remove_file(root);
    }

    #[test]
    fn managed_secret_debug_is_redacted() {
        let secret = ManagedSecret {
            provider_id: "bai".to_string(),
            value: "secret-value".to_string(),
        };
        let debug = format!("{secret:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-value"));
        assert!(!serde_json::to_string(&secret)
            .unwrap()
            .contains("secret-value"));
    }
    #[cfg(unix)]
    #[test]
    fn managed_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("provider-secrets-mode-{}.json", std::process::id()));
        let store = CredentialStore::new(&path);
        store.put_file_secret("id", "value").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn relative_file_source_is_resolved_next_to_config() {
        let root =
            std::env::temp_dir().join(format!("provider-file-source-{}", std::process::id()));
        let secret_path = root.join("api-key");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&secret_path, "file-secret\n").unwrap();
        let store = CredentialStore::new(root.join("provider-secrets.json"));
        let credential = Credential {
            id: "file-key".into(),
            provider_id: ProviderId::from("provider"),
            source: SecretSource::File {
                path: "api-key".to_string(),
            },
            auth_scheme: AuthScheme::Bearer,
        };
        assert_eq!(store.resolve(&credential).unwrap().expose(), "file-secret");
        let _ = std::fs::remove_dir_all(root);
    }
}
