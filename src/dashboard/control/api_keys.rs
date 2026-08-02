//! Resource-oriented API-key management endpoints.

use super::dashboard_error;
use crate::api_key::{ApiKeyError, ApiKeyPolicy, ApiKeyStatus, ApiKeyUpdate, ApiKeyView};
use crate::audit::AuditOutcome;
use crate::observability::RequestId;
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Default)]
pub struct CreateApiKeyRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    expires_in_days: Option<u64>,
    #[serde(default)]
    policy: Option<ApiKeyPolicy>,
    #[serde(default)]
    secret_bytes: Option<usize>,

    // Compatibility with the previous generate/append/replace form.
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    bytes: Option<usize>,
    #[serde(default)]
    save: Option<bool>,
    #[serde(default)]
    replace: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateApiKeyRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    clear_expiration: bool,
    #[serde(default)]
    status: Option<ApiKeyStatus>,
    #[serde(default)]
    policy: Option<ApiKeyPolicy>,
}

#[derive(Debug, Deserialize)]
pub struct RotateApiKeyRequest {
    #[serde(default = "default_secret_bytes")]
    secret_bytes: usize,
}

impl Default for RotateApiKeyRequest {
    fn default() -> Self {
        Self {
            secret_bytes: default_secret_bytes(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct VerifyApiKeyRequest {
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RevokeKeysRequest {
    indices: Vec<usize>,
}

const fn default_secret_bytes() -> usize {
    32
}

pub async fn handler_api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let registry = state.api_keys.read().await;
    let keys = registry.list();
    Ok(Json(list_response(
        &keys,
        registry.path().display().to_string(),
    )))
}

pub async fn handler_api_key_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_token(&state, &headers, None)?;
    let registry = state.api_keys.read().await;
    let key = registry
        .get(&id)
        .ok_or_else(|| dashboard_error(StatusCode::NOT_FOUND, "API key was not found"))?;
    Ok(Json(json!({
        "key": key,
        "registry_path": registry.path().display().to_string(),
        "hot_reload": true,
    })))
}

pub async fn handler_generate_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;

    // Preserve the old ephemeral generation mode for automation clients. The
    // redesigned browser UI never uses it.
    if request.name.is_none() && request.save == Some(false) {
        let count = request.count.unwrap_or(1);
        let bytes = request.bytes.or(request.secret_bytes).unwrap_or(32);
        let keys =
            crate::api_key::generate_api_keys(count, bytes, "sk-oc2-").map_err(api_key_error)?;
        return Ok(Json(json!({
            "status": "ok",
            "keys": keys,
            "saved": false,
            "restart_required": false,
        })));
    }

    let count = request.count.unwrap_or(1).clamp(1, 20);
    let secret_bytes = request
        .secret_bytes
        .or(request.bytes)
        .unwrap_or(default_secret_bytes());
    let expires_at = request.expires_at.or_else(|| {
        request
            .expires_in_days
            .filter(|days| *days > 0)
            .map(|days| {
                crate::api_key::unix_timestamp().saturating_add(days.saturating_mul(86_400))
            })
    });
    let environment = request
        .environment
        .unwrap_or_else(|| "production".to_string());
    let policy = request.policy.unwrap_or_default();

    let mut registry = state.api_keys.write().await;
    if request.replace.unwrap_or(false) {
        let ids = registry
            .list()
            .into_iter()
            .map(|key| key.id)
            .collect::<Vec<_>>();
        for id in ids {
            registry.revoke(&id).map_err(api_key_error)?;
        }
    }

    let mut created = Vec::with_capacity(count);
    let mut secrets = Vec::with_capacity(count);
    for index in 0..count {
        let name = request
            .name
            .clone()
            .unwrap_or_else(|| format!("API key {}", index + 1));
        let name = if count > 1 && request.name.is_some() {
            format!("{name} {}", index + 1)
        } else {
            name
        };
        let (view, secret) = registry
            .create(
                name,
                request.description.clone(),
                environment.clone(),
                expires_at,
                policy.clone(),
                secret_bytes,
            )
            .map_err(api_key_error)?;
        created.push(view);
        secrets.push(secret);
    }
    registry
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    let registry_path = registry.path().display().to_string();
    drop(registry);

    state.audit_log.record(
        "dashboard",
        "api_key_create",
        "client_auth",
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([
            ("count".to_string(), created.len().to_string()),
            ("hot_reload".to_string(), "true".to_string()),
        ]),
    );

    Ok(Json(json!({
        "status": "ok",
        "key": created.first(),
        "keys_metadata": created,
        "secret_once": secrets.first(),
        "keys": secrets,
        "saved": true,
        "replace": request.replace.unwrap_or(false),
        "restart_required": false,
        "hot_reload": true,
        "registry_path": registry_path,
    })))
}

pub async fn handler_update_api_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<UpdateApiKeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let expires_at = if request.clear_expiration {
        Some(None)
    } else {
        request.expires_at.map(Some)
    };
    let description = request.description.map(Some);
    let mut registry = state.api_keys.write().await;
    let key = registry
        .update(
            &id,
            ApiKeyUpdate {
                name: request.name,
                description,
                environment: request.environment,
                expires_at,
                status: request.status,
                policy: request.policy,
            },
        )
        .map_err(api_key_error)?;
    registry
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    drop(registry);
    record_action(&state, request_id, "api_key_update", &id);
    Ok(Json(json!({
        "status": "ok",
        "key": key,
        "restart_required": false,
        "hot_reload": true,
    })))
}

pub async fn handler_rotate_api_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    body: Option<Json<RotateApiKeyRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let request = body.map(|Json(value)| value).unwrap_or_default();
    let mut registry = state.api_keys.write().await;
    let (key, secret) = registry
        .rotate(&id, request.secret_bytes)
        .map_err(api_key_error)?;
    registry
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    drop(registry);
    record_action(&state, request_id, "api_key_rotate", &id);
    Ok(Json(json!({
        "status": "ok",
        "key": key,
        "secret_once": secret,
        "restart_required": false,
        "hot_reload": true,
    })))
}

pub async fn handler_delete_api_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let mut registry = state.api_keys.write().await;
    let key = registry.revoke(&id).map_err(api_key_error)?;
    registry
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    drop(registry);
    record_action(&state, request_id, "api_key_revoke", &id);
    Ok(Json(json!({
        "status": "ok",
        "key": key,
        "revoked": true,
        "restart_required": false,
        "hot_reload": true,
    })))
}

pub async fn handler_verify_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<VerifyApiKeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let registry = state.api_keys.read().await;

    if let Some(secret) = request
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let key = registry
            .verify(secret)
            .map_err(|error| dashboard_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
        return Ok(Json(json!({
            "status": "ok",
            "mode": "secret",
            "valid": key.active,
            "key": key,
            "checked_at": crate::api_key::unix_timestamp(),
        })));
    }

    let now = crate::api_key::unix_timestamp();
    let keys = registry.list();
    let mut healthy = 0usize;
    let mut warning = 0usize;
    let mut dead = 0usize;
    let mut disabled = 0usize;
    let mut expired = 0usize;
    let checks = keys
        .iter()
        .map(|key| {
            let (health, reason) = if key.expired {
                dead += 1;
                expired += 1;
                ("dead", "expired")
            } else if key.status == ApiKeyStatus::Disabled {
                dead += 1;
                disabled += 1;
                ("dead", "disabled")
            } else if key
                .expires_at
                .is_some_and(|expires| expires > now && expires - now <= 7 * 86_400)
            {
                warning += 1;
                ("warning", "expiring_soon")
            } else {
                healthy += 1;
                ("healthy", "active")
            };
            json!({
                "id": key.id,
                "name": key.name,
                "fingerprint": key.fingerprint,
                "status": key.status,
                "health": health,
                "reason": reason,
                "active": key.active,
                "expires_at": key.expires_at,
                "last_used_at": key.usage.last_used_at,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({
        "status": "ok",
        "mode": "registry",
        "checked_at": now,
        "checks": checks,
        "summary": {
            "total": keys.len(),
            "healthy": healthy,
            "warning": warning,
            "dead": dead,
            "disabled": disabled,
            "expired": expired,
        },
        "live_secret_probe": false,
        "note": "Managed raw secrets are not stored; this validates authoritative registry state and availability.",
    })))
}

/// Compatibility endpoint for the old `{indices:[...]}` revoke request.
pub async fn handler_revoke_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    Json(request): Json<RevokeKeysRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::super::auth::check_admin_mutation(&state, &headers)?;
    let mut registry = state.api_keys.write().await;
    let listed = registry.list();
    let selected_indices = request
        .indices
        .into_iter()
        .filter(|index| *index < listed.len())
        .collect::<Vec<_>>();
    let ids = selected_indices
        .iter()
        .filter_map(|index| listed.get(*index).map(|key| key.id.clone()))
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Err(dashboard_error(
            StatusCode::BAD_REQUEST,
            "at least one valid API-key index must be selected",
        ));
    }
    if ids.len() >= listed.len() {
        return Err(dashboard_error(
            StatusCode::CONFLICT,
            "refusing to revoke every client key; generate a replacement key first",
        ));
    }

    let legacy_only = listed
        .iter()
        .all(|key| key.source == crate::api_key::ApiKeySource::Legacy);
    if legacy_only {
        crate::api_key::revoke_auth_tokens(&state.config.management.config_path, &selected_indices)
            .map_err(api_key_error)?;
    }
    for id in &ids {
        registry.revoke(id).map_err(api_key_error)?;
    }
    registry
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    drop(registry);
    state.audit_log.record(
        "dashboard",
        "api_key_revoke",
        "client_auth",
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([("revoked_count".to_string(), ids.len().to_string())]),
    );
    Ok(Json(json!({
        "status": "ok",
        "revoked": ids.len(),
        "restart_required": false,
        "hot_reload": true,
    })))
}

fn list_response(keys: &[ApiKeyView], registry_path: String) -> Value {
    let now = crate::api_key::unix_timestamp();
    let active = keys.iter().filter(|key| key.active).count();
    let disabled = keys
        .iter()
        .filter(|key| key.status == ApiKeyStatus::Disabled)
        .count();
    let expired = keys.iter().filter(|key| key.expired).count();
    let expiring_soon = keys
        .iter()
        .filter(|key| {
            key.expires_at
                .is_some_and(|expires| expires > now && expires - now <= 7 * 86_400)
        })
        .count();
    json!({
        "keys": keys,
        "configured": !keys.is_empty(),
        "restart_required": false,
        "hot_reload": true,
        "registry_path": registry_path,
        "summary": {
            "total": keys.len(),
            "active": active,
            "disabled": disabled,
            "expired": expired,
            "expiring_soon": expiring_soon,
        },
        "models": crate::application::models::free_models(),
    })
}

fn record_action(
    state: &AppState,
    request_id: Option<Extension<RequestId>>,
    action: &str,
    key_id: &str,
) {
    state.audit_log.record(
        "dashboard",
        action,
        "client_auth",
        AuditOutcome::Success,
        request_id.map(|Extension(value)| value.0),
        BTreeMap::from([("key_id".to_string(), key_id.to_string())]),
    );
}

fn api_key_error(error: ApiKeyError) -> (StatusCode, Json<Value>) {
    match error {
        ApiKeyError::NotFound => dashboard_error(StatusCode::NOT_FOUND, error.to_string()),
        ApiKeyError::RevokedImmutable => dashboard_error(StatusCode::CONFLICT, error.to_string()),
        ApiKeyError::InvalidName
        | ApiKeyError::InvalidSize
        | ApiKeyError::InvalidCount
        | ApiKeyError::InvalidSelection => {
            dashboard_error(StatusCode::BAD_REQUEST, error.to_string())
        }
        other => dashboard_error(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}
