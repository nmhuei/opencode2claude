//! API-key lifecycle, policy, hot-reload registry, and legacy TOML compatibility.
//!
//! Managed keys are persisted in a sidecar JSON file next to the main TOML
//! configuration. Only SHA-256 digests are stored. A newly generated secret is
//! returned exactly once to the caller and never written to disk.
//!
//! Module layout:
//! * [`types`]   — shared types, enums, errors, and helper functions
//! * [`registry`] — the managed key registry (persistence + admission)
//! * [`legacy`]  — legacy `auth_tokens` TOML compatibility helpers

mod legacy;
mod registry;
pub(crate) mod types;

pub use legacy::{
    generate_api_keys, key_inventory, load_auth_tokens, merge_auth_tokens, remove_auth_tokens,
    revoke_auth_tokens, revoke_auth_tokens_with_store, save_auth_tokens,
    save_auth_tokens_with_store,
};
pub use registry::{registry_path, ApiKeyRegistry};
pub use types::{
    is_web_search_tool, unix_timestamp, ApiKeyAdmission, ApiKeyAuthError, ApiKeyAuthMatch,
    ApiKeyError, ApiKeyMetadata, ApiKeyPermissions, ApiKeyPolicy, ApiKeyPolicyError, ApiKeyRecord,
    ApiKeySource, ApiKeyStatus, ApiKeyUpdate, ApiKeyUsageView, ApiKeyView, AuthenticatedClient,
    LimitAction, ReasoningMode,
};

#[cfg(test)]
mod tests;
