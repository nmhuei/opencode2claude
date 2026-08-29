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

    // Transactional flow: mutate a staging copy, persist it atomically, and
    // only swap it into shared memory once persistence has succeeded. A failed
    // write therefore returns an error with the live registry untouched.
    let mut registry = state.api_keys.write().await;
    let mut staged = registry.stage();
    if request.replace.unwrap_or(false) {
        let ids = staged
            .list()
            .into_iter()
            .map(|key| key.id)
            .collect::<Vec<_>>();
        for id in ids {
            staged.revoke(&id).map_err(api_key_error)?;
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
        let (view, secret) = staged
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
    staged
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    registry.commit(staged);
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
    // Transactional flow: see `handler_generate_keys`.
    let mut registry = state.api_keys.write().await;
    let mut staged = registry.stage();
    let key = staged
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
    staged
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    registry.commit(staged);
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
    // Transactional flow: see `handler_generate_keys`. The replacement secret
    // is only returned to the caller once the registry write has succeeded.
    let mut registry = state.api_keys.write().await;
    let mut staged = registry.stage();
    // Revoked is terminal, matching `update`'s RevokedImmutable semantics.
    // The registry's own rotate() currently reactivates whatever it touches,
    // so this guard lives on the consumer side until the parity check moves
    // into api_key.rs. Judged on the staged copy while the exclusive lock is
    // held, so no concurrent mutation can slip between check and rotate.
    if staged
        .get(&id)
        .is_some_and(|key| key.status == ApiKeyStatus::Revoked)
    {
        return Err(api_key_error(ApiKeyError::RevokedImmutable));
    }
    let (key, secret) = staged
        .rotate(&id, request.secret_bytes)
        .map_err(api_key_error)?;
    staged
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    registry.commit(staged);
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
    // Transactional flow: see `handler_generate_keys`.
    let mut registry = state.api_keys.write().await;
    let mut staged = registry.stage();
    let key = staged.revoke(&id).map_err(api_key_error)?;
    staged
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    registry.commit(staged);
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

    // Transactional flow: see `handler_generate_keys`. The authoritative JSON
    // registry is revoked and durably persisted BEFORE the legacy TOML mirror
    // is touched, so a failure can never leave the two files half-updated.
    let mut staged = registry.stage();
    for id in &ids {
        staged.revoke(id).map_err(api_key_error)?;
    }
    staged
        .persist(state.file_store.as_ref())
        .map_err(api_key_error)?;
    registry.commit(staged);
    drop(registry);

    // The revocation is already durable in the authoritative store; the legacy
    // TOML mirror is best-effort from here on. Failing to update it must not
    // turn a completed revocation into an error response or a silent rollback.
    if legacy_only {
        if let Err(error) = crate::api_key::revoke_auth_tokens_with_store(
            &state.config.management.config_path,
            &selected_indices,
            state.file_store.as_ref(),
        ) {
            tracing::warn!(
                %error,
                "API-key revocation persisted to the registry but the legacy auth_tokens update failed; \
                 the authoritative registry remains the source of truth"
            );
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BridgeConfig, ManagementConfig, RuntimeConfig, SecretString};
    use crate::docker::DockerCliRuntime;
    use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
    use crate::infrastructure::warp::CliWarpController;
    use axum::http::HeaderValue;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// FileStore fault injector: registry sidecar writes and every other write
    /// (legacy TOML) can be failed independently to prove transaction ordering.
    #[derive(Debug, Default)]
    struct FaultStore {
        fail_api_keys_json: AtomicBool,
        fail_other_writes: AtomicBool,
        registry_write_attempts: AtomicUsize,
        inner: AtomicFileStore,
    }

    impl FaultStore {
        fn is_registry_sidecar(path: &Path) -> bool {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".api-keys.json"))
        }
    }

    impl FileStore for FaultStore {
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.inner.read(path)
        }

        fn atomic_write(&self, path: &Path, content: &[u8], sensitive: bool) -> io::Result<()> {
            if Self::is_registry_sidecar(path) {
                self.registry_write_attempts.fetch_add(1, Ordering::SeqCst);
                if self.fail_api_keys_json.load(Ordering::SeqCst) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected registry write failure",
                    ));
                }
            } else if self.fail_other_writes.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected legacy TOML write failure",
                ));
            }
            self.inner.atomic_write(path, content, sensitive)
        }

        fn remove_if_exists(&self, path: &Path) -> io::Result<()> {
            self.inner.remove_if_exists(path)
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "oc2api-control-keys-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn admin_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-dashboard-token", HeaderValue::from_static("dash-token"));
        headers
    }

    fn test_state(
        root: &Path,
        store: Arc<FaultStore>,
        auth_tokens: Option<Vec<SecretString>>,
    ) -> AppState {
        let config = BridgeConfig {
            auth_tokens,
            primary_proxies: None,
            warm_standby_proxies: None,
            runtime: RuntimeConfig {
                runtime_dir: Some(root.join("runtime")),
                ..BridgeConfig::default().runtime
            },
            management: ManagementConfig {
                config_path: root.join("config.toml"),
                dashboard_token: Some("dash-token".to_string().into()),
                ..BridgeConfig::default().management
            },
            ..Default::default()
        };
        AppState::new_with_infrastructure(
            config,
            Arc::new(DockerCliRuntime::from_config(&BridgeConfig::default())),
            Arc::new(CliWarpController::new("warp-cli")),
            store,
        )
    }

    #[tokio::test]
    async fn create_persist_failure_returns_error_and_keeps_memory_unchanged() {
        let root = temp_root("create-fail");
        fs::create_dir_all(&root).unwrap();
        let store = Arc::new(FaultStore::default());
        store.fail_api_keys_json.store(true, Ordering::SeqCst);
        let state = test_state(&root, store.clone(), None);

        let response = handler_generate_keys(
            State(state.clone()),
            admin_headers(),
            None,
            Json(CreateApiKeyRequest {
                name: Some("Should Not Survive".to_string()),
                ..Default::default()
            }),
        )
        .await;

        let error = response.expect_err("failed persist must surface as an error response");
        assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            store.registry_write_attempts.load(Ordering::SeqCst),
            1,
            "persist must actually be attempted before the error is returned"
        );
        // Memory must not keep the created key: the secret was shown to nobody,
        // but the record would silently vanish on restart if it stayed.
        let registry = state.api_keys.read().await;
        assert!(
            registry.list().is_empty(),
            "registry must stay unchanged when persist fails"
        );
        assert!(!registry.configured());

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rotate_handler_preserves_usage_counters() {
        let root = temp_root("rotate-usage");
        fs::create_dir_all(&root).unwrap();
        let state = test_state(&root, Arc::new(FaultStore::default()), None);

        let (view, secret) = {
            let mut registry = state.api_keys.write().await;
            registry
                .create(
                    "Rotate Target".to_string(),
                    None,
                    "production".to_string(),
                    None,
                    ApiKeyPolicy::default(),
                    16,
                )
                .unwrap()
        };
        for _ in 0..3 {
            state
                .api_keys
                .read()
                .await
                .match_secret(&secret, "/v1/messages")
                .unwrap()
                .admit()
                .await
                .unwrap();
        }
        let before = state.api_keys.read().await.get(&view.id).unwrap();
        assert_eq!(before.usage.requests, 3);
        assert_eq!(before.usage.minute_requests, 3);
        assert_eq!(before.usage.daily_requests, 3);

        let response = handler_rotate_api_key(
            State(state.clone()),
            Path(view.id.clone()),
            admin_headers(),
            None,
            Some(Json(RotateApiKeyRequest { secret_bytes: 32 })),
        )
        .await
        .unwrap();
        assert_eq!(response.0["status"], "ok");

        let after = state.api_keys.read().await.get(&view.id).unwrap();
        assert_eq!(after.usage.requests, 3, "rotation keeps lifetime requests");
        assert_eq!(
            after.usage.minute_requests, 3,
            "rotation keeps minute window"
        );
        assert_eq!(
            after.usage.daily_requests, 3,
            "rotation must not grant a fresh daily quota"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn revoke_persists_registry_first_and_legacy_failure_is_non_fatal() {
        let root = temp_root("revoke-order");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.toml"),
            "schema_version = 1\nauth_tokens = [\"legacy-token-a\", \"legacy-token-b\"]\n",
        )
        .unwrap();
        let store = Arc::new(FaultStore::default());
        store.fail_other_writes.store(true, Ordering::SeqCst);
        let state = test_state(
            &root,
            store,
            Some(vec![
                "legacy-token-a".to_string().into(),
                "legacy-token-b".to_string().into(),
            ]),
        );
        assert_eq!(state.api_keys.read().await.list().len(), 2);

        let response = handler_revoke_keys(
            State(state.clone()),
            admin_headers(),
            None,
            Json(RevokeKeysRequest { indices: vec![0] }),
        )
        .await;

        // The authoritative registry must win: revocation succeeds even though
        // the legacy TOML write fails, and the client still gets an ok response.
        let body = response.unwrap_or_else(|error| {
            panic!(
                "registry revoke+persist must succeed despite legacy failure: {:?}",
                error.0
            )
        });
        assert_eq!(body.0["status"], "ok");
        assert_eq!(body.0["revoked"], 1);

        let remaining = state.api_keys.read().await.list();
        assert_eq!(remaining.len(), 1, "exactly one legacy key remains listed");

        let registry_path = crate::api_key::registry_path(&root.join("config.toml"));
        let persisted = fs::read_to_string(&registry_path).unwrap();
        assert!(persisted.contains("\"status\": \"revoked\""));

        let tokens = crate::api_key::load_auth_tokens(&root.join("config.toml")).unwrap();
        assert_eq!(
            tokens.len(),
            2,
            "legacy TOML stays untouched when its own write fails; it must never gate the authoritative store"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rotating_a_revoked_key_is_conflict_not_resurrection() {
        let root = temp_root("rotate-revoked");
        fs::create_dir_all(&root).unwrap();
        let state = test_state(&root, Arc::new(FaultStore::default()), None);

        let view = {
            let mut registry = state.api_keys.write().await;
            let (view, _secret) = registry
                .create(
                    "Compromised Key".to_string(),
                    None,
                    "production".to_string(),
                    None,
                    ApiKeyPolicy::default(),
                    16,
                )
                .unwrap();
            registry.revoke(&view.id).unwrap();
            view
        };

        // update() refuses revoked keys with RevokedImmutable (409); rotate()
        // must enforce the identical invariant. Otherwise a stray UI retry or
        // script silently resurrects a revoked credential and hands out a
        // fresh working secret.
        let response = handler_rotate_api_key(
            State(state.clone()),
            Path(view.id.clone()),
            admin_headers(),
            None,
            Some(Json(RotateApiKeyRequest { secret_bytes: 32 })),
        )
        .await;
        let error = response.expect_err("rotating a revoked key must be rejected");
        assert_eq!(error.0, StatusCode::CONFLICT);

        let after = state.api_keys.read().await.get(&view.id).unwrap();
        assert_eq!(
            after.status,
            ApiKeyStatus::Revoked,
            "rejected rotation must leave the key revoked"
        );
        assert_eq!(
            after.fingerprint, view.fingerprint,
            "rejected rotation must not mint a replacement secret"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rotating_a_disabled_key_stays_allowed() {
        let root = temp_root("rotate-disabled");
        fs::create_dir_all(&root).unwrap();
        let state = test_state(&root, Arc::new(FaultStore::default()), None);

        let view = {
            let mut registry = state.api_keys.write().await;
            let (view, _secret) = registry
                .create(
                    "Paused Key".to_string(),
                    None,
                    "production".to_string(),
                    None,
                    ApiKeyPolicy::default(),
                    16,
                )
                .unwrap();
            registry
                .update(
                    &view.id,
                    ApiKeyUpdate {
                        status: Some(ApiKeyStatus::Disabled),
                        ..Default::default()
                    },
                )
                .unwrap();
            view
        };

        // Only Revoked is terminal. Disabling is a reversible pause, and
        // rotation remains a legitimate way to re-enable with fresh secret
        // material — the guard above must not overcorrect onto Disabled.
        let body = handler_rotate_api_key(
            State(state.clone()),
            Path(view.id.clone()),
            admin_headers(),
            None,
            Some(Json(RotateApiKeyRequest { secret_bytes: 32 })),
        )
        .await
        .unwrap();
        assert_eq!(body.0["status"], "ok");
        assert_eq!(
            state.api_keys.read().await.get(&view.id).unwrap().status,
            ApiKeyStatus::Active
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn revoke_success_still_updates_legacy_tokens_when_writable() {
        let root = temp_root("revoke-happy");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.toml"),
            "schema_version = 1\nauth_tokens = [\"legacy-token-a\", \"legacy-token-b\"]\n",
        )
        .unwrap();
        let state = test_state(
            &root,
            Arc::new(FaultStore::default()),
            Some(vec![
                "legacy-token-a".to_string().into(),
                "legacy-token-b".to_string().into(),
            ]),
        );

        let body = handler_revoke_keys(
            State(state.clone()),
            admin_headers(),
            None,
            Json(RevokeKeysRequest { indices: vec![0] }),
        )
        .await
        .unwrap();
        assert_eq!(body.0["status"], "ok");
        assert_eq!(body.0["revoked"], 1);
        assert_eq!(state.api_keys.read().await.list().len(), 1);
        let tokens = crate::api_key::load_auth_tokens(&root.join("config.toml")).unwrap();
        assert_eq!(tokens, vec!["legacy-token-b".to_string()]);

        let _ = fs::remove_dir_all(root);
    }
}
