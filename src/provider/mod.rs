//! Provider management domain.
//!
//! This module is deliberately independent from the legacy client API-key
//! registry.  A provider credential belongs to one upstream provider and is
//! resolved only when an attempt is built.

pub mod adapters;
pub mod catalog;
pub mod config;
pub mod credentials;
pub mod health;
pub mod registry;
pub mod resilience;
pub mod routing;
pub mod store;
pub mod types;

pub use registry::{ProviderRegistry, ProviderRuntimeHandle, ProviderSnapshot, RegistryError};
pub use store::{ProviderConfigStore, ProviderMutation, ProviderStoreError, StoreMigrationReport};
pub use types::{
    AliasId, AttemptTarget, AuthScheme, Credential, CredentialId, ModelAlias, ModelCandidate,
    ModelInfo, Provider, ProviderId, ProviderKind, ProviderProtocol, ProviderRequest,
};
