use crate::config::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(ProviderId);
id_type!(CredentialId);
id_type!(AliasId);

/// Stable identifier for a named set of credentials sharing one routing
/// policy. Pool ids are kept separate from credential ids so CLI/config
/// bindings cannot silently resolve to the wrong namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialPoolId(pub String);

impl CredentialPoolId {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("credential pool id must not be empty".to_string());
        }
        Ok(Self(value))
    }
}

impl From<&str> for CredentialPoolId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for CredentialPoolId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl AsRef<str> for CredentialPoolId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialPoolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[serde(rename = "opencode", alias = "open-code")]
    OpenCode,
    #[serde(rename = "kilo")]
    Kilo,
    #[serde(rename = "bai", alias = "b.ai")]
    Bai,
    #[serde(rename = "openai-compatible", alias = "open-ai-compatible")]
    OpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[serde(rename = "openai_chat_completions", alias = "open_ai_chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai_responses", alias = "open_ai_responses")]
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AuthScheme {
    #[serde(rename = "bearer")]
    #[default]
    Bearer,
    #[serde(rename = "x-api-key", alias = "x_api_key")]
    XApiKey,
    None,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: ProviderId,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers = self
            .headers
            .iter()
            .map(|(key, value)| {
                (
                    key,
                    if is_secret_header(key) {
                        "[REDACTED]".to_string()
                    } else {
                        value.clone()
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        f.debug_struct("Provider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("protocol", &self.protocol)
            .field("headers", &headers)
            .field("enabled", &self.enabled)
            .finish()
    }
}

fn is_secret_header(key: &str) -> bool {
    let compact = key.to_ascii_lowercase().replace(['-', '_'], "");
    matches!(
        compact.as_str(),
        "authorization" | "apikey" | "xapikey" | "cookie" | "setcookie"
    ) || compact.contains("token")
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: CredentialId,
    pub provider_id: ProviderId,
    pub source: SecretSource,
    pub auth_scheme: AuthScheme,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("source", &self.source)
            .field("auth_scheme", &self.auth_scheme)
            .finish()
    }
}

impl Credential {
    pub fn belongs_to(&self, provider: impl AsRef<str>) -> Result<(), String> {
        (self.provider_id.as_ref() == provider.as_ref())
            .then_some(())
            .ok_or_else(|| {
                format!(
                    "credential {} belongs to provider {}, not {}",
                    self.id,
                    self.provider_id,
                    provider.as_ref()
                )
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SecretSource {
    Env { variable: String },
    File { path: String },
    Managed { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub wire_model_id: String,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub verified_context: bool,
    #[serde(default)]
    pub free: bool,
}

impl ModelInfo {
    pub fn context_class(&self) -> ContextClass {
        match self.context_window {
            Some(value) if value >= 1_000_000 => ContextClass::OneMillion,
            Some(value) => ContextClass::Known(value),
            None => ContextClass::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextClass {
    OneMillion,
    Known(usize),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCandidate {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub credential_id: Option<CredentialId>,
    /// V3 uses this field instead of `credential_id`; keeping both optional
    /// fields preserves the schema-v2 Rust representation while validation
    /// guarantees that at most one is populated.
    #[serde(default)]
    pub credential_pool_id: Option<CredentialPoolId>,
    #[serde(default)]
    pub priority: i32,
}

impl ModelCandidate {
    pub fn binding(&self) -> Result<CredentialBinding, String> {
        match (&self.credential_id, &self.credential_pool_id) {
            (None, None) => Ok(CredentialBinding::Anonymous),
            (Some(credential), None) => Ok(CredentialBinding::Direct(credential.clone())),
            (None, Some(pool)) => Ok(CredentialBinding::Pool(pool.clone())),
            (Some(_), Some(_)) => {
                Err("candidate cannot define both credential and credential_pool".to_string())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PoolStrategy {
    #[default]
    RoundRobin,
    LeastLoaded,
    WeightedRoundRobin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialPoolMember {
    pub credential_id: CredentialId,
    pub quota_scope: String,
    pub max_in_flight: NonZeroU32,
    pub requests_per_minute: Option<NonZeroU32>,
    pub tokens_per_minute: Option<NonZeroU64>,
    pub weight: NonZeroU32,
}

impl CredentialPoolMember {
    pub fn new(
        credential_id: impl Into<CredentialId>,
        quota_scope: impl Into<String>,
        max_in_flight: u32,
        requests_per_minute: Option<u32>,
        tokens_per_minute: Option<u64>,
        weight: u32,
    ) -> Result<Self, String> {
        let credential_id: CredentialId = credential_id.into();
        if credential_id.as_ref().trim().is_empty() {
            return Err("pool member credential id must not be empty".to_string());
        }
        let quota_scope = quota_scope.into();
        if quota_scope.trim().is_empty() {
            return Err("pool member quota scope must not be empty".to_string());
        }
        let max_in_flight = NonZeroU32::new(max_in_flight)
            .filter(|value| value.get() <= 1024)
            .ok_or_else(|| "max_in_flight must be in 1..=1024".to_string())?;
        let requests_per_minute = match requests_per_minute {
            None => None,
            Some(value) => Some(
                NonZeroU32::new(value)
                    .filter(|value| value.get() <= 10_000_000)
                    .ok_or_else(|| "requests_per_minute must be in 1..=10000000".to_string())?,
            ),
        };
        let tokens_per_minute = match tokens_per_minute {
            None => None,
            Some(value) => Some(
                NonZeroU64::new(value)
                    .filter(|value| value.get() <= 10_000_000_000)
                    .ok_or_else(|| "tokens_per_minute must be in 1..=10000000000".to_string())?,
            ),
        };
        let weight = NonZeroU32::new(weight)
            .filter(|value| value.get() <= 1000)
            .ok_or_else(|| "weight must be in 1..=1000".to_string())?;
        Ok(Self {
            credential_id,
            quota_scope,
            max_in_flight,
            requests_per_minute,
            tokens_per_minute,
            weight,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialPool {
    pub id: CredentialPoolId,
    pub provider_id: ProviderId,
    pub strategy: PoolStrategy,
    pub members: Vec<CredentialPoolMember>,
}

impl CredentialPool {
    pub fn new(
        id: impl Into<String>,
        provider_id: impl Into<ProviderId>,
        strategy: PoolStrategy,
        members: Vec<CredentialPoolMember>,
    ) -> Result<Self, String> {
        if members.is_empty() {
            return Err("credential pool must contain at least one member".to_string());
        }
        let id = CredentialPoolId::new(id.into())?;
        let provider_id = provider_id.into();
        if provider_id.as_ref().trim().is_empty() {
            return Err("credential pool provider must not be empty".to_string());
        }
        Ok(Self {
            id,
            provider_id,
            strategy,
            members,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialBinding {
    Anonymous,
    Direct(CredentialId),
    Pool(CredentialPoolId),
}

impl CredentialBinding {
    pub fn credential_id(&self) -> Option<&CredentialId> {
        match self {
            Self::Direct(id) => Some(id),
            Self::Anonymous | Self::Pool(_) => None,
        }
    }

    pub fn pool_id(&self) -> Option<&CredentialPoolId> {
        match self {
            Self::Pool(id) => Some(id),
            Self::Anonymous | Self::Direct(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteTarget {
    pub alias_id: AliasId,
    pub client_model: String,
    pub provider_id: ProviderId,
    pub model_id: String,
    pub wire_model_id: String,
    pub binding: CredentialBinding,
    pub context_window: usize,
    pub max_output_tokens: Option<usize>,
    pub candidate_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityDemand {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_tokens: u64,
}

impl CapacityDemand {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        let total = input_tokens.saturating_add(output_tokens);
        let estimated_tokens = total
            .checked_mul(125)
            .and_then(|value| value.checked_add(99))
            .map(|value| value / 100)
            .unwrap_or(u64::MAX)
            .max(1);
        Self {
            input_tokens,
            output_tokens,
            estimated_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    pub id: AliasId,
    pub client_model: String,
    pub context_window: usize,
    #[serde(default)]
    pub strict_context: bool,
    pub candidates: Vec<ModelCandidate>,
}

impl ModelAlias {
    /// The compaction boundary is a product invariant shared by every route.
    /// Keeping it derived prevents a fallback candidate from changing the
    /// client-visible context policy.
    pub fn auto_compact_window(&self) -> usize {
        auto_compact_window(self.context_window)
    }

    pub fn one_million(id: impl Into<AliasId>, candidates: Vec<ModelCandidate>) -> Self {
        Self {
            id: id.into(),
            client_model: "sonnet[1m]".to_string(),
            context_window: 1_000_000,
            strict_context: true,
            candidates,
        }
    }
}

/// Return the 80% compaction threshold without rounding errors for small or
/// non-round context windows.
pub fn auto_compact_window(context_window: usize) -> usize {
    let whole = context_window / 100 * 80;
    let remainder = context_window % 100 * 80 / 100;
    whole + remainder
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptTarget {
    pub alias_id: AliasId,
    pub client_model: String,
    pub provider_id: ProviderId,
    pub model_id: String,
    pub wire_model_id: String,
    pub credential_id: Option<CredentialId>,
    pub auth_scheme: AuthScheme,
    pub context_window: usize,
    pub candidate_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub client_model: String,
    pub messages: serde_json::Value,
    pub max_output_tokens: Option<usize>,
    pub stream: bool,
    /// Original OpenAI-shaped request when available. Adapters use this to
    /// preserve tools, tool choice, sampling, response format, and vendor
    /// extensions while replacing only the selected wire model.
    pub body: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ProviderHttpRequest {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct NormalizedResponse {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub body: serde_json::Value,
}

#[allow(dead_code)]
pub(crate) fn redacted_secret(secret: &SecretString) -> String {
    let _ = secret;
    "[REDACTED]".to_string()
}

#[cfg(test)]
mod capacity_types_tests {
    use super::*;

    #[test]
    fn pool_member_and_route_binding_are_explicit() {
        let member = CredentialPoolMember::new("key-a", "account-a", 2, None, None, 3).unwrap();
        let pool = CredentialPool::new(
            "free-1m",
            "bai",
            PoolStrategy::WeightedRoundRobin,
            vec![member.clone()],
        )
        .unwrap();

        assert_eq!(pool.id.as_ref(), "free-1m");
        assert_eq!(pool.provider_id.as_ref(), "bai");
        assert_eq!(pool.members[0].credential_id.as_ref(), "key-a");
        assert_eq!(pool.members[0].weight.get(), 3);
        assert_eq!(
            CredentialBinding::Pool(pool.id.clone()).pool_id(),
            Some(&pool.id)
        );
        assert!(CredentialBinding::Anonymous.pool_id().is_none());
        let key = CredentialId::from("key-a");
        assert_eq!(
            CredentialBinding::Direct(key.clone()).credential_id(),
            Some(&key)
        );
    }

    #[test]
    fn capacity_demand_uses_saturating_ceil_with_minimum_one() {
        assert_eq!(CapacityDemand::new(0, 0).estimated_tokens, 1);
        assert_eq!(CapacityDemand::new(1, 1).estimated_tokens, 3);
        assert_eq!(
            CapacityDemand::new(u64::MAX, u64::MAX).estimated_tokens,
            u64::MAX
        );
    }

    #[test]
    fn invalid_pool_values_are_rejected() {
        assert!(CredentialPoolId::new("").is_err());
        assert!(CredentialPoolMember::new("key", "scope", 0, None, None, 1).is_err());
        assert!(CredentialPoolMember::new("key", "scope", 1, Some(0), None, 1).is_err());
        assert!(CredentialPoolMember::new("key", "scope", 1, None, None, 0).is_err());
    }
}
