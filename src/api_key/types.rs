//! Core types, enums, error definitions, and shared helpers for API-key management.

use crate::opencode::mapper::map_model_name;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const REGISTRY_SCHEMA_VERSION: u32 = 1;
pub(crate) const DEFAULT_KEY_PREFIX: &str = "sk-oc2-";

#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    #[error("API-key size must be between 16 and 64 bytes")]
    InvalidSize,
    #[error("API-key count must be between 1 and 20")]
    InvalidCount,
    #[error("at least one valid API-key index must be selected")]
    InvalidSelection,
    #[error("API-key name must contain between 1 and 80 characters")]
    InvalidName,
    #[error("API-key record was not found")]
    NotFound,
    #[error("revoked API keys are immutable")]
    RevokedImmutable,
    #[error("failed to generate secure random bytes: {0}")]
    Random(String),
    #[error("failed to read API-key data: {0}")]
    Read(#[source] io::Error),
    #[error("invalid API-key registry: {0}")]
    RegistryParse(#[source] serde_json::Error),
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml_edit::TomlError),
    #[error("updated configuration is invalid: {0}")]
    Validation(#[source] toml::de::Error),
    #[error("failed to write API-key data atomically: {0}")]
    Write(#[source] io::Error),
    #[error("failed to serialize API-key registry: {0}")]
    RegistrySerialize(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyStatus {
    #[default]
    Active,
    Disabled,
    Revoked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeySource {
    #[default]
    Managed,
    Legacy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    #[default]
    Inherit,
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LimitAction {
    #[default]
    Reject,
    Clamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyPermissions {
    #[serde(default = "default_true")]
    pub anthropic_messages: bool,
    #[serde(default = "default_true")]
    pub openai_chat: bool,
    #[serde(default = "default_true")]
    pub list_models: bool,
    #[serde(default = "default_true")]
    pub count_tokens: bool,
    #[serde(default = "default_true")]
    pub streaming: bool,
    #[serde(default = "default_true")]
    pub tools: bool,
    #[serde(default = "default_true")]
    pub web_search: bool,
    #[serde(default)]
    pub shell: bool,
}

impl Default for ApiKeyPermissions {
    fn default() -> Self {
        Self {
            anthropic_messages: true,
            openai_chat: true,
            list_models: true,
            count_tokens: true,
            streaming: true,
            tools: true,
            web_search: true,
            shell: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyPolicy {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_model_override: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub max_reasoning_tokens: Option<u32>,
    #[serde(default)]
    pub reasoning_mode: ReasoningMode,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub limit_action: LimitAction,
    #[serde(default)]
    pub max_concurrent_requests: Option<usize>,
    #[serde(default)]
    pub requests_per_minute: Option<u32>,
    #[serde(default)]
    pub daily_request_quota: Option<u64>,
    #[serde(default)]
    pub permissions: ApiKeyPermissions,
}

impl Default for ApiKeyPolicy {
    fn default() -> Self {
        Self {
            default_model: None,
            allowed_models: Vec::new(),
            allow_model_override: true,
            max_output_tokens: None,
            max_reasoning_tokens: None,
            reasoning_mode: ReasoningMode::Inherit,
            reasoning_effort: None,
            limit_action: LimitAction::Reject,
            max_concurrent_requests: None,
            requests_per_minute: None,
            daily_request_quota: None,
            permissions: ApiKeyPermissions::default(),
        }
    }
}

impl ApiKeyPolicy {
    pub fn normalize(&mut self) {
        self.default_model = clean_optional(self.default_model.take());
        self.reasoning_effort =
            clean_optional(self.reasoning_effort.take()).map(|value| value.to_ascii_lowercase());
        self.allowed_models = self
            .allowed_models
            .drain(..)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        self.allowed_models.sort();
        self.allowed_models.dedup();
        self.max_concurrent_requests = nonzero(self.max_concurrent_requests);
        self.requests_per_minute = nonzero(self.requests_per_minute);
        self.daily_request_quota = nonzero(self.daily_request_quota);
        self.max_output_tokens = nonzero(self.max_output_tokens);
        self.max_reasoning_tokens = nonzero(self.max_reasoning_tokens);
    }

    pub fn endpoint_allowed(&self, path: &str) -> bool {
        match path {
            "/v1/messages" => self.permissions.anthropic_messages,
            "/v1/messages/count_tokens" => self.permissions.count_tokens,
            "/v1/chat/completions" => self.permissions.openai_chat,
            "/v1/models" => self.permissions.list_models,
            _ => true,
        }
    }

    /// Resolve the effective model for this key's policy.
    ///
    /// Selection precedence is unchanged: when overrides are allowed the
    /// client-requested name wins, otherwise the key's `default_model`, then
    /// the bridge-global configured model, then `fallback`.
    ///
    /// # Allowlist namespace (resolved ids)
    ///
    /// `allowed_models` is matched in the RESOLVED model-id namespace: both
    /// the selected name and every allowlist entry are run through
    /// [`crate::opencode::mapper::map_model_name`] — the exact mapping later
    /// applied at the forwarding seam (`opencode/…` prefix stripping,
    /// `-free` aliasing, `claude-*` family collapse) — before comparison.
    /// Consequences, pinned by tests:
    ///
    /// * an entry written as a resolved id (`deepseek-v4-flash-free`) admits
    ///   every wire spelling that normalizes onto it (`deepseek-v4-flash`,
    ///   `opencode/deepseek-v4-flash`);
    /// * legacy entries written in wire form keep working and additionally
    ///   admit aliases within the same resolved class. This can only widen:
    ///   identical strings always normalize identically, so no previously
    ///   admitted pair can become rejected;
    /// * an empty/unset allowlist still means "every model allowed";
    /// * entries are whole-token matches after normalization — there is no
    ///   wildcard syntax;
    /// * the returned `String` deliberately stays the client-selected name:
    ///   the wire→resolved rewrite itself still happens downstream at the
    ///   forwarding seam (`map_model_name`), exactly as before. The rewrite
    ///   is idempotent on already-resolved ids.
    pub fn resolve_model(
        &self,
        requested: Option<&str>,
        global: Option<&str>,
        fallback: &str,
    ) -> Result<String, ApiKeyPolicyError> {
        let requested = requested.map(str::trim).filter(|value| !value.is_empty());
        let selected = if self.allow_model_override {
            requested
                .or(self.default_model.as_deref())
                .or(global)
                .unwrap_or(fallback)
        } else {
            self.default_model.as_deref().or(global).unwrap_or(fallback)
        };

        // Judge the verdict in the resolved namespace so an admin writing an
        // intuitive allowlist never silently breaks a key over spelling: the
        // forwarder maps names with `map_model_name`, so the gate must too.
        if !self.allowed_models.is_empty()
            && !self
                .allowed_models
                .iter()
                .any(|allowed| map_model_name(allowed) == map_model_name(selected))
        {
            return Err(ApiKeyPolicyError::ModelNotAllowed(selected.to_string()));
        }
        Ok(selected.to_string())
    }

    pub fn enforce_output_tokens(
        &self,
        requested: Option<u32>,
    ) -> Result<Option<u32>, ApiKeyPolicyError> {
        let Some(limit) = self.max_output_tokens else {
            return Ok(requested);
        };
        match requested {
            Some(value) if value > limit && self.limit_action == LimitAction::Reject => {
                Err(ApiKeyPolicyError::OutputLimitExceeded {
                    requested: value,
                    limit,
                })
            }
            Some(value) => Ok(Some(value.min(limit))),
            None => Ok(Some(limit)),
        }
    }

    pub fn enforce_reasoning_tokens(
        &self,
        requested: Option<u32>,
    ) -> Result<Option<u32>, ApiKeyPolicyError> {
        let Some(limit) = self.max_reasoning_tokens else {
            return Ok(requested);
        };
        match requested {
            Some(value) if value > limit && self.limit_action == LimitAction::Reject => {
                Err(ApiKeyPolicyError::ReasoningLimitExceeded {
                    requested: value,
                    limit,
                })
            }
            Some(value) => Ok(Some(value.min(limit))),
            None => Ok(Some(limit)),
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ApiKeyPolicyError {
    #[error("model '{0}' is not allowed for this API key")]
    ModelNotAllowed(String),
    #[error("requested max_tokens {requested} exceeds this API key limit of {limit}")]
    OutputLimitExceeded { requested: u32, limit: u32 },
    #[error("requested reasoning budget {requested} exceeds this API key limit of {limit}")]
    ReasoningLimitExceeded { requested: u32, limit: u32 },
    #[error("streaming is disabled for this API key")]
    StreamingDisabled,
    #[error("tool use is disabled for this API key")]
    ToolsDisabled,
    #[error("web search is disabled for this API key")]
    WebSearchDisabled,
    #[error("shell execution is disabled for this API key")]
    ShellDisabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub fingerprint: String,
    pub secret_hash: String,
    #[serde(default)]
    pub status: ApiKeyStatus,
    #[serde(default = "default_environment")]
    pub environment: String,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub source: ApiKeySource,
    #[serde(default)]
    pub policy: ApiKeyPolicy,
}

impl ApiKeyRecord {
    pub(crate) fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.description = clean_optional(self.description.take());
        self.environment = normalize_environment(&self.environment);
        self.policy.normalize();
    }

    pub fn expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|expires| expires <= now)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApiKeyUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub environment: Option<String>,
    pub expires_at: Option<Option<u64>>,
    pub status: Option<ApiKeyStatus>,
    pub policy: Option<ApiKeyPolicy>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApiKeyUsageView {
    pub requests: u64,
    pub rejected: u64,
    pub last_used_at: Option<u64>,
    pub in_flight: usize,
    pub minute_requests: u32,
    pub daily_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub fingerprint: String,
    pub status: ApiKeyStatus,
    pub environment: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub expired: bool,
    pub source: ApiKeySource,
    pub policy: ApiKeyPolicy,
    pub usage: ApiKeyUsageView,
    // Compatibility fields for older dashboard clients.
    pub index: usize,
    pub length: usize,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedClient {
    pub key_id: String,
    pub name: String,
    pub environment: String,
    pub policy: ApiKeyPolicy,
}

#[derive(Debug)]
pub struct ApiKeyAdmission {
    pub client: AuthenticatedClient,
    #[allow(dead_code)]
    pub(crate) permit: Option<OwnedSemaphorePermit>,
}

impl ApiKeyAdmission {
    /// Create an admission for the bridge-owned Claude Code integration key.
    ///
    /// This credential is intentionally outside the dashboard-managed API-key
    /// registry, so creating, rotating, disabling, or revoking application keys
    /// cannot interrupt the local Claude Code connection.
    pub(crate) fn claude_code(policy: ApiKeyPolicy) -> Self {
        Self {
            client: AuthenticatedClient {
                key_id: "system_claude_code".to_string(),
                name: "Claude Code".to_string(),
                environment: "local".to_string(),
                policy,
            },
            permit: None,
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ApiKeyAuthError {
    #[error("invalid API key")]
    Invalid,
    #[error("API key is disabled")]
    Disabled,
    #[error("API key has been revoked")]
    Revoked,
    #[error("API key has expired")]
    Expired,
    #[error("this API key is not allowed to access {0}")]
    EndpointDenied(String),
    #[error("API key concurrent request limit reached")]
    ConcurrentLimit,
    #[error("API key requests-per-minute limit reached")]
    RequestsPerMinute,
    #[error("API key daily request quota reached")]
    DailyQuota,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ApiKeyRegistryFile {
    #[serde(default = "registry_schema_version")]
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) keys: Vec<ApiKeyRecord>,
    #[serde(default)]
    pub(crate) suppressed_legacy_hashes: Vec<String>,
}

impl Default for ApiKeyRegistryFile {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            keys: Vec::new(),
            suppressed_legacy_hashes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiKeyAuthMatch {
    pub(crate) record: ApiKeyRecord,
    pub(crate) runtime: Arc<ApiKeyRuntime>,
}

impl ApiKeyAuthMatch {
    pub async fn admit(self) -> Result<ApiKeyAdmission, ApiKeyAuthError> {
        let permit = self.runtime.admit(&self.record.policy).await?;
        Ok(ApiKeyAdmission {
            client: AuthenticatedClient {
                key_id: self.record.id,
                name: self.record.name,
                environment: self.record.environment,
                policy: self.record.policy,
            },
            permit,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiKeyMetadata {
    pub index: usize,
    pub fingerprint: String,
    pub length: usize,
    pub active: bool,
}

// ---------------------------------------------------------------------------
// Runtime (concurrency + rate-limit enforcement)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RuntimeSnapshot {
    pub(crate) requests: u64,
    pub(crate) rejected: u64,
    pub(crate) last_used_at: u64,
    pub(crate) minute_started: u64,
    pub(crate) minute_requests: u32,
    pub(crate) day_number: u64,
    pub(crate) daily_requests: u64,
}

#[derive(Debug, Default)]
pub(crate) struct RateWindow {
    pub(crate) minute_started: u64,
    pub(crate) minute_requests: u32,
    pub(crate) day_number: u64,
    pub(crate) daily_requests: u64,
}

#[derive(Debug)]
pub(crate) struct ApiKeyRuntime {
    semaphore: Option<Arc<Semaphore>>,
    max_concurrent: Option<usize>,
    requests: AtomicU64,
    rejected: AtomicU64,
    last_used_at: AtomicU64,
    window: Mutex<RateWindow>,
}

impl ApiKeyRuntime {
    pub(crate) fn new(policy: &ApiKeyPolicy, snapshot: Option<RuntimeSnapshot>) -> Self {
        let snapshot = snapshot.unwrap_or_default();
        Self {
            semaphore: policy
                .max_concurrent_requests
                .filter(|value| *value > 0)
                .map(|value| Arc::new(Semaphore::new(value))),
            max_concurrent: policy.max_concurrent_requests,
            requests: AtomicU64::new(snapshot.requests),
            rejected: AtomicU64::new(snapshot.rejected),
            last_used_at: AtomicU64::new(snapshot.last_used_at),
            window: Mutex::new(RateWindow {
                minute_started: snapshot.minute_started,
                minute_requests: snapshot.minute_requests,
                day_number: snapshot.day_number,
                daily_requests: snapshot.daily_requests,
            }),
        }
    }

    pub(crate) async fn admit(
        &self,
        policy: &ApiKeyPolicy,
    ) -> Result<Option<OwnedSemaphorePermit>, ApiKeyAuthError> {
        let permit = match &self.semaphore {
            Some(semaphore) => Some(semaphore.clone().try_acquire_owned().map_err(|_| {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                ApiKeyAuthError::ConcurrentLimit
            })?),
            None => None,
        };

        let now = unix_timestamp();
        let mut window = self
            .window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if window.minute_started == 0 || now.saturating_sub(window.minute_started) >= 60 {
            window.minute_started = now;
            window.minute_requests = 0;
        }
        let day_number = now / 86_400;
        if window.day_number != day_number {
            window.day_number = day_number;
            window.daily_requests = 0;
        }
        if policy
            .requests_per_minute
            .is_some_and(|limit| window.minute_requests >= limit)
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(ApiKeyAuthError::RequestsPerMinute);
        }
        if policy
            .daily_request_quota
            .is_some_and(|limit| window.daily_requests >= limit)
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(ApiKeyAuthError::DailyQuota);
        }
        window.minute_requests = window.minute_requests.saturating_add(1);
        window.daily_requests = window.daily_requests.saturating_add(1);
        drop(window);
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.last_used_at.store(now, Ordering::Relaxed);
        Ok(permit)
    }

    pub(crate) fn snapshot(&self) -> RuntimeSnapshot {
        let window = self
            .window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        RuntimeSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            last_used_at: self.last_used_at.load(Ordering::Relaxed),
            minute_started: window.minute_started,
            minute_requests: window.minute_requests,
            day_number: window.day_number,
            daily_requests: window.daily_requests,
        }
    }

    pub(crate) fn view(&self) -> ApiKeyUsageView {
        let snapshot = self.snapshot();
        let in_flight = self
            .semaphore
            .as_ref()
            .zip(self.max_concurrent)
            .map(|(semaphore, max)| max.saturating_sub(semaphore.available_permits()))
            .unwrap_or(0);
        ApiKeyUsageView {
            requests: snapshot.requests,
            rejected: snapshot.rejected,
            last_used_at: (snapshot.last_used_at > 0).then_some(snapshot.last_used_at),
            in_flight,
            minute_requests: snapshot.minute_requests,
            daily_requests: snapshot.daily_requests,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helper functions
// ---------------------------------------------------------------------------

pub(crate) fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn registry_schema_version() -> u32 {
    REGISTRY_SCHEMA_VERSION
}

pub(crate) fn default_environment() -> String {
    "production".to_string()
}

pub(crate) fn normalize_environment(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "development" | "staging" | "production" => value,
        "dev" => "development".to_string(),
        "stage" => "staging".to_string(),
        _ if value.is_empty() => "production".to_string(),
        _ => value.chars().take(32).collect(),
    }
}

pub(crate) fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn nonzero<T>(value: Option<T>) -> Option<T>
where
    T: PartialEq + From<u8>,
{
    value.filter(|value| *value != T::from(0))
}

pub(crate) fn fingerprint(token: &str) -> String {
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() <= 18 {
        return "••••••••".to_string();
    }
    let prefix = chars.iter().take(14).collect::<String>();
    let suffix = chars
        .iter()
        .skip(chars.len().saturating_sub(6))
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

pub fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn is_web_search_tool(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "web_search" | "websearch" | "web_fetch" | "webfetch"
    )
}
