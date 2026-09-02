//! Provider management domain.
//!
//! This module is deliberately independent from the legacy client API-key
//! registry.  A provider credential belongs to one upstream provider and is
//! resolved only when an attempt is built.

pub mod adapters;
pub mod capacity;
pub mod catalog;
pub mod config;
pub mod credentials;
pub mod health;
pub mod registry;
pub mod resilience;
pub mod routing;
pub mod store;
pub mod types;

pub use capacity::{
    parse_rate_limit_feedback, AdmissionError, AttemptIdentity, CapacityObserver,
    CapacityScheduler, CapacitySummary, DispatchLease, RateLimitFeedback, TokenUsage,
};
pub use registry::{
    ProviderRegistry, ProviderRuntimeHandle, ProviderRuntimeSnapshot, ProviderSnapshot,
    RegistryError,
};
pub use store::{ProviderConfigStore, ProviderMutation, ProviderStoreError, StoreMigrationReport};
pub use types::{
    AliasId, AttemptTarget, AuthScheme, CapacityDemand, Credential, CredentialBinding,
    CredentialId, CredentialPool, CredentialPoolId, CredentialPoolMember, ModelAlias,
    ModelCandidate, ModelInfo, PoolStrategy, Provider, ProviderId, ProviderKind, ProviderProtocol,
    ProviderRequest, RouteTarget,
};
