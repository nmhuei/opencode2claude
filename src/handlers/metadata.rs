//! Metadata, health, readiness, and token-count endpoints.

use super::MessagesRequest;
use crate::config::{EgressMode, DEFAULT_MODEL};
use crate::error::BridgeError;
use crate::opencode;
use crate::state::AppState;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;
use std::time::Duration;

/// Probe surfaces are polled by load balancers and monitors; a cached
/// "ready" masks outages and a cached "not_ready" hides recovery.
const NO_STORE_HEADERS: [(header::HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];

pub async fn handle_count_tokens(
    payload: Result<Json<MessagesRequest>, JsonRejection>,
) -> Result<axum::response::Response, BridgeError> {
    let Json(payload) = payload
        .map_err(|error| BridgeError::InvalidRequest(format!("Invalid request body: {error}")))?;
    Ok(Json(json!({
        "input_tokens": opencode::estimate_input_tokens(&payload)
    }))
    .into_response())
}

pub async fn handle_models(State(state): State<AppState>) -> impl IntoResponse {
    let model_id = state
        .config
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    Json(json!({
        "object": "list",
        "data": [{
            "id": model_id,
            "object": "model",
            "created": 0
        }]
    }))
}

/// Backward-compatible minimal health response.
pub async fn handle_health(State(_state): State<AppState>) -> impl IntoResponse {
    (
        NO_STORE_HEADERS,
        Json(json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// Process/event-loop liveness. This endpoint deliberately does not disclose topology.
pub async fn handle_liveness() -> impl IntoResponse {
    (
        NO_STORE_HEADERS,
        Json(json!({
            "status": "live",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// Operational readiness: critical workers must be healthy and configured egress must be usable.
///
/// Readiness contract (docs/superpowers/specs/2026-08-24-hybrid-egress-design.md §11):
/// gateway readiness is separated from proxy readiness. In `direct` and
/// `hybrid` modes the direct fallback keeps the gateway usable, so proxy
/// degradation never turns into a 503 here; live proxy subsystem state is
/// reported additively under `checks.proxy` / `egress.proxy`.
pub async fn handle_readiness(State(state): State<AppState>) -> impl IntoResponse {
    let heartbeat_budget = state
        .config
        .egress
        .health_interval
        .saturating_mul(3)
        .max(Duration::from_secs(90));
    let workers_ready = state.workers.critical_ready(heartbeat_budget);

    // Snapshot the subsystem before locking the pool and never nest these
    // locks; hybrid route selection follows the same ordering.
    let proxy_snapshot = state.proxy_subsystem.read().await.snapshot();
    let identity_ttl = state.config.egress.identity_ttl;

    struct EgressReadiness {
        /// Whether the configured egress keeps the gateway usable at all.
        gateway_usable: bool,
        checks_proxy: bool,
        verified_unique_exit_ips: usize,
        active_route: &'static str,
        direct: serde_json::Value,
        proxy: serde_json::Value,
    }

    fn subsystem_json(
        snapshot: &crate::proxy_pool::ProxySubsystemSnapshot,
        exits: usize,
    ) -> serde_json::Value {
        json!({
            "state": serde_json::to_value(snapshot.phase)
                .unwrap_or_else(|_| json!("unknown")),
            "ready": snapshot.ready,
            "verified_unique_exit_ips": exits,
            "last_error": snapshot.last_error,
        })
    }

    let readiness = match state.config.egress.mode {
        // Direct mode has no proxy path; local configuration guarantees the
        // direct route is usable, so readiness reduces to the worker gate.
        EgressMode::Direct => EgressReadiness {
            gateway_usable: true,
            checks_proxy: false,
            verified_unique_exit_ips: 0,
            active_route: "direct",
            direct: json!({ "ready": true }),
            proxy: subsystem_json(&proxy_snapshot, 0),
        },
        // Hybrid stays gateway-ready through the direct fallback by design;
        // exit evidence comes from the live pool, not a constant.
        EgressMode::Hybrid => {
            let (exits, proxy_routable) = {
                let pool = state.proxy_pool.read().await;
                (
                    pool.verified_unique_exit_count_fresh(identity_ttl),
                    pool.egress_ready(state.config.egress.minimum_unique_exit_ips, identity_ttl),
                )
            };
            let proxy_ready = proxy_snapshot.ready && proxy_routable;
            EgressReadiness {
                gateway_usable: true,
                checks_proxy: proxy_ready,
                // Mirrors fresh-request preference in select_route: a proxy
                // route is preferred only while reconciliation is Ready AND
                // at least one eligible (including non-draining) route exists.
                active_route: if proxy_ready { "proxy" } else { "direct" },
                verified_unique_exit_ips: exits,
                direct: json!({ "ready": true }),
                proxy: {
                    let mut value = subsystem_json(&proxy_snapshot, exits);
                    value["ready"] = json!(proxy_ready);
                    value
                },
            }
        }
        // Pure proxy mode has no fallback: the pool gate remains the single
        // source of gateway readiness (unchanged legacy semantics). The
        // subsystem lifecycle itself IS reconciled in pure-proxy mode too
        // (AppState registers the reconciler for Proxy|Hybrid), so §11
        // evidence comes from the live snapshot once it has spoken; until the
        // first reconcile cycle lands (phase still Starting) the pool gate
        // keeps deciding, preserving the legacy ready/degraded rendering.
        EgressMode::Proxy => {
            let (gateway_usable, exits) = {
                let pool = state.proxy_pool.read().await;
                (
                    pool.egress_ready(state.config.egress.minimum_unique_exit_ips, identity_ttl),
                    pool.verified_unique_exit_count_fresh(identity_ttl),
                )
            };
            let proxy = if proxy_snapshot.phase == crate::proxy_pool::ProxySubsystemPhase::Starting
            {
                json!({
                    "state": if gateway_usable { "ready" } else { "degraded" },
                    "ready": gateway_usable,
                    "verified_unique_exit_ips": exits,
                    "last_error": proxy_snapshot.last_error,
                })
            } else {
                subsystem_json(&proxy_snapshot, exits)
            };
            EgressReadiness {
                gateway_usable,
                checks_proxy: gateway_usable,
                active_route: "proxy",
                verified_unique_exit_ips: exits,
                direct: json!({ "ready": false }),
                proxy,
            }
        }
    };

    let ready = workers_ready && readiness.gateway_usable;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        NO_STORE_HEADERS,
        Json(json!({
            "status": if ready { "ready" } else { "not_ready" },
            "checks": {
                "critical_workers": workers_ready,
                "egress": readiness.gateway_usable,
                "proxy": readiness.checks_proxy,
            },
            "egress": {
                "mode": match state.config.egress.mode {
                    EgressMode::Direct => "direct",
                    EgressMode::Proxy => "proxy",
                    EgressMode::Hybrid => "hybrid",
                },
                "verified_unique_exit_ips": readiness.verified_unique_exit_ips,
                "minimum_unique_exit_ips": state.config.egress.minimum_unique_exit_ips,
                "active_route": readiness.active_route,
                "direct": readiness.direct,
                "proxy": readiness.proxy,
            },
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use crate::proxy_pool::ExitIdentity;
    use crate::shell::ShellPolicy;
    use axum::body::to_bytes;
    use serde_json::Value;

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn exit_identity(public_ip: &str, verified_at_unix_secs: u64) -> ExitIdentity {
        ExitIdentity {
            public_ip: public_ip.to_string(),
            provider: Some("cloudflare-warp".to_string()),
            colo: Some("SIN".to_string()),
            verified_at_unix_secs,
        }
    }

    /// State for readiness probes. Identity endpoints are cleared so no
    /// background monitor probes external services from tests.
    fn readiness_state(mode: EgressMode, primary_proxies: Option<Vec<String>>) -> AppState {
        AppState::new(BridgeConfig {
            host: "127.0.0.1".parse().unwrap(),
            bridge_port: 0,
            opencode_port: 4096,
            model: Some("test-model".to_string()),
            shell_policy: ShellPolicy::Disabled,
            max_body_size: 1024,
            stream_buffer_size: 4096,
            channel_capacity: 64,
            primary_proxies,
            max_search_loops: 3,
            egress: crate::config::EgressConfig {
                mode,
                identity_endpoints: Vec::new(),
                ..BridgeConfig::default().egress
            },
            ..Default::default()
        })
    }

    async fn readiness_json(state: AppState) -> (StatusCode, Value) {
        let response = handle_readiness(State(state)).await.into_response();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn hybrid_readiness_derives_verified_exits_from_pool() {
        let state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].exit_identity = Some(exit_identity("203.0.113.10", unix_now()));
        }
        let (_, body) = readiness_json(state).await;
        assert_eq!(body["egress"]["mode"], "hybrid");
        assert_eq!(
            body["egress"]["verified_unique_exit_ips"], 1,
            "readiness must report live verified exit evidence, not a constant"
        );
    }

    #[tokio::test]
    async fn hybrid_readiness_ignores_stale_exit_identities() {
        let ttl_secs = BridgeConfig::default().egress.identity_ttl.as_secs();
        let state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].exit_identity = Some(exit_identity(
                "203.0.113.10",
                unix_now().saturating_sub(ttl_secs + 60),
            ));
        }
        let (_, body) = readiness_json(state).await;
        assert_eq!(
            body["egress"]["verified_unique_exit_ips"], 0,
            "stale identities must not count as verified exits"
        );
    }

    #[tokio::test]
    async fn hybrid_readiness_keeps_egress_check_true_without_verified_exits() {
        let state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        let (_, body) = readiness_json(state).await;
        assert_eq!(
            body["checks"]["egress"], true,
            "hybrid stays gateway-usable via the direct fallback even without proxy evidence"
        );
        assert_eq!(body["egress"]["verified_unique_exit_ips"], 0);
    }

    #[tokio::test]
    async fn hybrid_readiness_reports_direct_route_while_subsystem_starting() {
        let state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        // A freshly constructed hybrid state starts in the Starting phase.
        let (_, body) = readiness_json(state).await;
        assert_eq!(body["checks"]["proxy"], false);
        assert_eq!(
            body["egress"]["active_route"], "direct",
            "active_route must mirror fresh-request route preference"
        );
        assert_eq!(body["egress"]["direct"]["ready"], true);
        assert_eq!(body["egress"]["proxy"]["ready"], false);
        assert_eq!(
            body["egress"]["proxy"]["last_error"],
            serde_json::Value::Null
        );
    }

    /// Reconciler interference guard: background workers hold their own clone
    /// of the original subsystem Arc. Swapping the AppState field isolates the
    /// manually driven verdicts below from concurrently running reconcile
    /// cycles (docker CLI latency makes those races timing-dependent flakes).
    fn detach_subsystem_from_workers(state: &mut AppState) {
        state.proxy_subsystem = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::proxy_pool::ProxySubsystemStatus::starting(),
        ));
    }

    #[tokio::test]
    async fn health_family_responses_are_not_cacheable() {
        // Probes and load balancers poll these endpoints; a cached "ready"
        // masks outages and a cached "not_ready" hides recovery.
        let state = readiness_state(EgressMode::Direct, None);
        for (name, response) in [
            (
                "/health",
                handle_health(State(state.clone())).await.into_response(),
            ),
            ("/health/live", handle_liveness().await.into_response()),
            (
                "/health/ready",
                handle_readiness(State(state.clone())).await.into_response(),
            ),
        ] {
            assert_eq!(
                response.headers()["cache-control"],
                "no-store",
                "{name} must send Cache-Control: no-store"
            );
        }
    }

    #[tokio::test]
    async fn hybrid_readiness_reports_proxy_route_when_subsystem_ready() {
        let mut state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        detach_subsystem_from_workers(&mut state);
        state.proxy_subsystem.write().await.mark_ready();
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].health = crate::proxy_pool::HealthState::Healthy;
            pool.proxies[0].circuit = crate::proxy_pool::CircuitState::Closed;
            pool.proxies[0].exit_identity = Some(exit_identity("203.0.113.10", unix_now()));
        }
        let (_, body) = readiness_json(state).await;
        assert_eq!(body["checks"]["proxy"], true);
        assert_eq!(body["egress"]["active_route"], "proxy");
        assert_eq!(body["egress"]["proxy"]["state"], "ready");
        assert_eq!(body["egress"]["proxy"]["ready"], true);
        assert_eq!(body["egress"]["proxy"]["verified_unique_exit_ips"], 1);
    }

    #[tokio::test]
    async fn hybrid_readiness_surfaces_subsystem_error_without_gateway_503_signal() {
        let mut state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        detach_subsystem_from_workers(&mut state);
        state
            .proxy_subsystem
            .write()
            .await
            .mark_degraded("verification failed", None);
        let (http_status, body) = readiness_json(state).await;
        assert_eq!(body["egress"]["proxy"]["state"], "degraded");
        assert_eq!(
            body["egress"]["proxy"]["last_error"], "verification failed",
            "the bounded subsystem error must surface under egress.proxy.last_error"
        );
        assert_eq!(body["egress"]["active_route"], "direct");
        assert_eq!(
            body["checks"]["egress"], true,
            "degraded proxy must never flip hybrid gateway egress readiness"
        );
        // Proxy degradation alone must not produce a 503; whatever status the
        // workers gate yields, a degraded proxy cannot turn ready into 503.
        if body["checks"]["critical_workers"] == true {
            assert_eq!(http_status, StatusCode::OK);
            assert_eq!(body["status"], "ready");
        }
    }

    #[tokio::test]
    async fn hybrid_readiness_falls_back_direct_when_ready_proxy_is_drained() {
        let mut state = readiness_state(
            EgressMode::Hybrid,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        detach_subsystem_from_workers(&mut state);
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].health = crate::proxy_pool::HealthState::Healthy;
            pool.proxies[0].circuit = crate::proxy_pool::CircuitState::Closed;
            pool.proxies[0].exit_identity = Some(exit_identity("203.0.113.10", unix_now()));
            pool.begin_drain(0).expect("drain managed primary");
        }
        state.proxy_subsystem.write().await.mark_ready();
        let (http_status, body) = readiness_json(state).await;
        assert_eq!(
            body["checks"]["egress"], true,
            "hybrid direct fallback stays usable even when proxy routing is intentionally drained"
        );
        assert_eq!(body["checks"]["proxy"], false);
        assert_eq!(body["egress"]["active_route"], "direct");
        assert_eq!(body["egress"]["proxy"]["ready"], false);
        if body["checks"]["critical_workers"] == true {
            assert_eq!(http_status, StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn strict_proxy_readiness_fails_when_only_route_is_drained() {
        let mut state = readiness_state(
            EgressMode::Proxy,
            Some(vec!["socks5h://127.0.0.1:40001".to_string()]),
        );
        detach_subsystem_from_workers(&mut state);
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].health = crate::proxy_pool::HealthState::Healthy;
            pool.proxies[0].circuit = crate::proxy_pool::CircuitState::Closed;
            pool.proxies[0].exit_identity = Some(exit_identity("203.0.113.10", unix_now()));
            pool.begin_drain(0).expect("drain managed primary");
        }
        state.proxy_subsystem.write().await.mark_ready();
        let (http_status, body) = readiness_json(state).await;
        assert_eq!(http_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["checks"]["egress"], false);
        assert_eq!(body["checks"]["proxy"], false);
    }

    #[tokio::test]
    async fn direct_readiness_preserves_legacy_shape_additively() {
        // No proxies configured -> no critical workers registered -> the
        // worker gate is vacuously satisfied and the response is deterministic.
        let state = readiness_state(EgressMode::Direct, None);
        let (http_status, body) = readiness_json(state).await;
        assert_eq!(http_status, StatusCode::OK);
        // Legacy keys (unchanged semantics).
        assert_eq!(body["status"], "ready");
        assert_eq!(body["checks"]["critical_workers"], true);
        assert_eq!(body["checks"]["egress"], true);
        assert_eq!(body["egress"]["mode"], "direct");
        assert_eq!(body["egress"]["verified_unique_exit_ips"], 0);
        assert_eq!(
            body["egress"]["minimum_unique_exit_ips"],
            BridgeConfig::default().egress.minimum_unique_exit_ips
        );
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        // Additive §11 fields.
        assert_eq!(body["checks"]["proxy"], false);
        assert_eq!(body["egress"]["active_route"], "direct");
        assert_eq!(body["egress"]["direct"]["ready"], true);
        assert_eq!(body["egress"]["proxy"]["state"], "disabled");
        assert_eq!(body["egress"]["proxy"]["ready"], false);
    }

    #[tokio::test]
    async fn proxy_mode_readiness_stays_pool_derived_and_additive() {
        // Empty default pool: no routable proxy -> proxy-mode gateway is not
        // ready; no proxies configured means no critical workers, so the
        // outcome is deterministic. With no usable pool the subsystem starts
        // Disabled (state.rs), so the snapshot renders verbatim instead of
        // taking the Starting-phase gate fallback.
        let state = readiness_state(EgressMode::Proxy, None);
        let (http_status, body) = readiness_json(state).await;
        assert_eq!(http_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "not_ready");
        assert_eq!(body["checks"]["egress"], false);
        assert_eq!(body["checks"]["proxy"], false);
        assert_eq!(body["egress"]["active_route"], "proxy");
        assert_eq!(body["egress"]["direct"]["ready"], false);
        assert_eq!(body["egress"]["proxy"]["ready"], false);
        assert_eq!(body["egress"]["proxy"]["state"], "disabled");
    }

    #[tokio::test]
    async fn proxy_mode_readiness_reads_live_subsystem_snapshot_once_reconciled() {
        // The reconciler drives ProxySubsystemStatus in pure-proxy mode too,
        // so once it has spoken, §11 evidence must come from the live
        // snapshot — including its error text — instead of gate guesses.
        let state = readiness_state(EgressMode::Proxy, None);

        // Reconciler verdict: ready. The pool gate stays unusable (empty
        // pool), so gateway checks keep failing, but egress.proxy must now
        // report the snapshot's own phase instead of deriving from the gate.
        state.proxy_subsystem.write().await.mark_ready();
        let (_, body) = readiness_json(state.clone()).await;
        assert_eq!(
            body["egress"]["proxy"]["state"], "ready",
            "a reconciled Ready snapshot must not be re-derived from the pool gate"
        );
        assert_eq!(body["egress"]["proxy"]["ready"], true);
        assert_eq!(
            body["checks"]["egress"], false,
            "snapshot readiness must not flip pure-proxy gateway readiness"
        );

        // Reconciler verdict: degraded with a bounded error that must surface.
        state
            .proxy_subsystem
            .write()
            .await
            .mark_degraded("verification failed", None);
        let (_, body) = readiness_json(state).await;
        assert_eq!(body["egress"]["proxy"]["state"], "degraded");
        assert_eq!(body["egress"]["proxy"]["ready"], false);
        assert_eq!(
            body["egress"]["proxy"]["last_error"], "verification failed",
            "the snapshot's last_error must reach egress.proxy.last_error"
        );
    }
}
