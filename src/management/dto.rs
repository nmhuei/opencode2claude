//! Typed management API data-transfer objects and their OpenAPI schemas.

use crate::audit::AuditEvent;
use crate::management::service::{ProxyRestartResult, SafeConfigSnapshot};
use crate::observability::MetricsSnapshot;
use crate::proxy_pool::ProxyPoolStats;
use crate::workers::WorkerSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub trait ApiSchema {
    const NAME: &'static str;
    fn schema() -> Value;
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgeSummary {
    pub host: String,
    pub port: u16,
    pub client_auth_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EgressSummary {
    pub mode: String,
    pub ready: bool,
    pub unique_verified_exits: usize,
    pub proxy_pool: ProxyPoolStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    pub status: String,
    pub service: String,
    pub version: String,
    pub uptime_secs: u64,
    pub model: Option<String>,
    pub bridge: BridgeSummary,
    pub egress: EgressSummary,
    pub workers: WorkerSnapshot,
}

impl ApiSchema for StatusResponse {
    const NAME: &'static str = "StatusResponse";
    fn schema() -> Value {
        object_schema(
            &[
                "status",
                "service",
                "version",
                "uptime_secs",
                "bridge",
                "egress",
                "workers",
            ],
            json!({
                "status": {"type":"string"},
                "service": {"type":"string"},
                "version": {"type":"string"},
                "uptime_secs": {"type":"integer","minimum":0},
                "model": {"type":["string","null"]},
                "bridge": {"type":"object"},
                "egress": {"type":"object"},
                "workers": {"type":"object"}
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxiesResponse {
    pub policy: String,
    pub primary: crate::proxy_pool::ProxyTierStats,
    pub warm_standby: crate::proxy_pool::ProxyTierStats,
    pub nodes: Vec<crate::proxy_pool::ProxyNodeStats>,
}

impl From<ProxyPoolStats> for ProxiesResponse {
    fn from(snapshot: ProxyPoolStats) -> Self {
        Self {
            policy: snapshot.policy,
            primary: snapshot.primary,
            warm_standby: snapshot.warm_standby,
            nodes: snapshot.nodes,
        }
    }
}

impl ApiSchema for ProxiesResponse {
    const NAME: &'static str = "ProxiesResponse";
    fn schema() -> Value {
        object_schema(
            &["policy", "primary", "warm_standby", "nodes"],
            json!({
                "policy": {"type":"string"},
                "primary": {"type":"object"},
                "warm_standby": {"type":"object"},
                "nodes": {"type":"array","items":{"type":"object"}}
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigFeatures {
    pub client_auth_configured: bool,
    pub tavily_configured: bool,
    pub exa_configured: bool,
    pub serper_configured: bool,
    pub searxng_configured: bool,
    pub searxng_api_key_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigResponse {
    pub host: String,
    pub bridge_port: u16,
    pub opencode_port: u16,
    pub model: Option<String>,
    pub shell_policy: String,
    pub max_body_size: usize,
    pub stream_buffer_size: usize,
    pub channel_capacity: usize,
    pub max_search_loops: u32,
    pub primary_proxies: Vec<String>,
    pub warm_standby_proxies: Vec<String>,
    pub features: ConfigFeatures,
}

impl From<SafeConfigSnapshot> for ConfigResponse {
    fn from(cfg: SafeConfigSnapshot) -> Self {
        Self {
            host: cfg.host,
            bridge_port: cfg.bridge_port,
            opencode_port: cfg.opencode_port,
            model: cfg.model,
            shell_policy: cfg.shell_policy,
            max_body_size: cfg.max_body_size,
            stream_buffer_size: cfg.stream_buffer_size,
            channel_capacity: cfg.channel_capacity,
            max_search_loops: cfg.max_search_loops,
            primary_proxies: cfg.primary_proxies,
            warm_standby_proxies: cfg.warm_standby_proxies,
            features: ConfigFeatures {
                client_auth_configured: cfg.client_auth_configured,
                tavily_configured: cfg.tavily_configured,
                exa_configured: cfg.exa_configured,
                serper_configured: cfg.serper_configured,
                searxng_configured: cfg.searxng_configured,
                searxng_api_key_configured: cfg.searxng_api_key_configured,
            },
        }
    }
}

impl ApiSchema for ConfigResponse {
    const NAME: &'static str = "ConfigResponse";
    fn schema() -> Value {
        object_schema(
            &[
                "host",
                "bridge_port",
                "opencode_port",
                "shell_policy",
                "features",
            ],
            json!({
                "host": {"type":"string"},
                "bridge_port": {"type":"integer","minimum":1,"maximum":65535},
                "opencode_port": {"type":"integer","minimum":1,"maximum":65535},
                "model": {"type":["string","null"]},
                "shell_policy": {"type":"string"},
                "max_body_size": {"type":"integer","minimum":0},
                "stream_buffer_size": {"type":"integer","minimum":1},
                "channel_capacity": {"type":"integer","minimum":1},
                "max_search_loops": {"type":"integer","minimum":1},
                "primary_proxies": {"type":"array","items":{"type":"string"}},
                "warm_standby_proxies": {"type":"array","items":{"type":"string"}},
                "features": {"type":"object"}
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyActionResponse {
    pub status: String,
    pub proxy: ProxyRestartResult,
}

impl ApiSchema for ProxyActionResponse {
    const NAME: &'static str = "ProxyActionResponse";
    fn schema() -> Value {
        object_schema(
            &["status", "proxy"],
            json!({"status":{"type":"string"},"proxy":{"type":"object"}}),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsResponse {
    pub metrics: MetricsSnapshot,
    pub workers: WorkerSnapshot,
}

impl ApiSchema for MetricsResponse {
    const NAME: &'static str = "MetricsResponse";
    fn schema() -> Value {
        object_schema(
            &["metrics", "workers"],
            json!({"metrics":{"type":"object"},"workers":{"type":"object"}}),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditEventsResponse {
    pub events: Vec<AuditEvent>,
}

impl ApiSchema for AuditEventsResponse {
    const NAME: &'static str = "AuditEventsResponse";
    fn schema() -> Value {
        object_schema(
            &["events"],
            json!({
                "events": {
                    "type":"array",
                    "items": {
                        "type":"object",
                        "additionalProperties":false,
                        "required":["timestamp_secs","actor","action","target","outcome"],
                        "properties": {
                            "timestamp_secs":{"type":"integer","minimum":0},
                            "actor":{"type":"string"},
                            "action":{"type":"string"},
                            "target":{"type":"string"},
                            "outcome":{"type":"string","enum":["success","failure"]},
                            "request_id":{"type":["string","null"]},
                            "details":{"type":"object","additionalProperties":{"type":"string"}}
                        }
                    }
                }
            }),
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigDocumentRequest {
    pub content: String,
}

impl ApiSchema for ConfigDocumentRequest {
    const NAME: &'static str = "ConfigDocumentRequest";
    fn schema() -> Value {
        object_schema(
            &["content"],
            json!({"content":{"type":"string","maxLength":1048576}}),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigPreviewResponse {
    pub valid: bool,
    pub changed_keys: Vec<String>,
    pub restart_required: bool,
    pub warnings: Vec<String>,
}

impl ApiSchema for ConfigPreviewResponse {
    const NAME: &'static str = "ConfigPreviewResponse";
    fn schema() -> Value {
        object_schema(
            &["valid", "changed_keys", "restart_required", "warnings"],
            json!({
                "valid":{"type":"boolean"},
                "changed_keys":{"type":"array","items":{"type":"string"}},
                "restart_required":{"type":"boolean"},
                "warnings":{"type":"array","items":{"type":"string"}}
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigApplyResponse {
    pub status: String,
    pub path: String,
    pub changed_keys: Vec<String>,
    pub restart_required: bool,
    pub rollback_performed: bool,
}

impl ApiSchema for ConfigApplyResponse {
    const NAME: &'static str = "ConfigApplyResponse";
    fn schema() -> Value {
        object_schema(
            &[
                "status",
                "path",
                "changed_keys",
                "restart_required",
                "rollback_performed",
            ],
            json!({
                "status":{"type":"string"},
                "path":{"type":"string"},
                "changed_keys":{"type":"array","items":{"type":"string"}},
                "restart_required":{"type":"boolean"},
                "rollback_performed":{"type":"boolean"}
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorDetail {
    pub code: &'static str,
    pub message: String,
}

impl ApiSchema for ApiErrorBody {
    const NAME: &'static str = "ApiErrorBody";
    fn schema() -> Value {
        object_schema(
            &["error"],
            json!({"error":{"type":"object","required":["code","message"],"properties":{"code":{"type":"string"},"message":{"type":"string"}}}}),
        )
    }
}

pub fn schema_components() -> Value {
    let mut schemas = serde_json::Map::new();
    insert_schema::<StatusResponse>(&mut schemas);
    insert_schema::<ProxiesResponse>(&mut schemas);
    insert_schema::<ConfigResponse>(&mut schemas);
    insert_schema::<ProxyActionResponse>(&mut schemas);
    insert_schema::<MetricsResponse>(&mut schemas);
    insert_schema::<AuditEventsResponse>(&mut schemas);
    insert_schema::<ConfigDocumentRequest>(&mut schemas);
    insert_schema::<ConfigPreviewResponse>(&mut schemas);
    insert_schema::<ConfigApplyResponse>(&mut schemas);
    insert_schema::<ApiErrorBody>(&mut schemas);
    Value::Object(schemas)
}

fn insert_schema<T: ApiSchema>(schemas: &mut serde_json::Map<String, Value>) {
    schemas.insert(T::NAME.to_string(), T::schema());
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":required,
        "properties":properties
    })
}

pub fn schema_ref<T: ApiSchema>() -> Value {
    json!({"$ref": format!("#/components/schemas/{}", T::NAME)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_registry_contains_every_public_dto() {
        let components = schema_components();
        for name in [
            StatusResponse::NAME,
            ProxiesResponse::NAME,
            ConfigResponse::NAME,
            ProxyActionResponse::NAME,
            MetricsResponse::NAME,
            AuditEventsResponse::NAME,
            ConfigDocumentRequest::NAME,
            ConfigPreviewResponse::NAME,
            ConfigApplyResponse::NAME,
            ApiErrorBody::NAME,
        ] {
            assert!(components.get(name).is_some(), "missing schema {name}");
        }
    }
}
