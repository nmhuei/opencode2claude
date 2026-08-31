use crate::error::BridgeError;
use crate::observability::EgressRouteMetricClass;
use crate::proxy_pool::{EgressLease, EgressRole, RouteKind, RouteMetadata};
use crate::state::AppState;
use reqwest::{Client, RequestBuilder};
use serde::Serialize;
use std::net::IpAddr;
use std::time::Duration;
use tracing::{info, warn};

pub(crate) struct SelectedRoute {
    pub(crate) client: Client,
    pub(crate) proxy_url: Option<String>,
    pub(crate) proxy_index: Option<usize>,
    pub(crate) lease: Option<EgressLease>,
    pub(crate) upstream_real_ip: Option<String>,
    pub(crate) metadata: RouteMetadata,
}

pub(crate) fn prepare_upstream_request<T: Serialize>(
    route: &SelectedRoute,
    upstream_url: &str,
    request: &T,
    upstream_api_key: Option<&str>,
) -> RequestBuilder {
    let mut builder = route.client.post(upstream_url).json(request);
    if let Some(key) = upstream_api_key.filter(|k| !k.trim().is_empty()) {
        builder = builder.header("Authorization", format!("Bearer {key}"));
    }
    if let Some(real_ip) = route.upstream_real_ip.as_deref() {
        builder = builder.header("x-real-ip", real_ip);
    }
    builder
}

fn verified_exit_real_ip(identity: Option<&crate::proxy_pool::ExitIdentity>) -> Option<String> {
    let public_ip = identity?.public_ip.trim();
    public_ip.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

pub(crate) async fn select_route_for_attempt(
    state: &AppState,
    routing_key: &str,
    excluded_proxy: Option<usize>,
    retained_rate_limit_route: Option<SelectedRoute>,
) -> Result<SelectedRoute, BridgeError> {
    if let Some(route) = retained_rate_limit_route {
        return Ok(route);
    }
    select_route(state, routing_key, excluded_proxy).await
}

/// Commits one egress route for an upstream attempt (spec §12).
///
/// Each successful return is a fresh route decision and records exactly one
/// `record_egress_route` event. Retained rate-limit routes bypass this
/// function entirely (`select_route_for_attempt`), so in-flight retries never
/// double-count; only transport/model fallbacks that re-select a route count
/// as additional decisions.
pub(crate) async fn select_route(
    state: &AppState,
    routing_key: &str,
    excluded_proxy: Option<usize>,
) -> Result<SelectedRoute, BridgeError> {
    if state.config.egress.mode == crate::config::EgressMode::Direct {
        state
            .metrics
            .record_egress_route(EgressRouteMetricClass::Direct);
        return Ok(direct_route(state, RouteKind::Direct));
    }

    if state.config.egress.mode == crate::config::EgressMode::Hybrid {
        if !state.proxy_subsystem.read().await.is_ready() {
            state
                .metrics
                .record_egress_route(EgressRouteMetricClass::DirectHybridFallback);
            return Ok(direct_route(state, RouteKind::DirectHybridFallback));
        }

        let selection = {
            let mut pool = state.proxy_pool.write().await;
            let selected = match excluded_proxy {
                Some(index) => pool.get_client_excluding(routing_key, index),
                None => pool.get_client(routing_key),
            };
            selected.and_then(|(client, proxy_url, proxy_index)| {
                pool.begin_lease(proxy_index).map(|lease| {
                    let node = &pool.proxies[proxy_index];
                    let upstream_real_ip = verified_exit_real_ip(node.exit_identity.as_ref());
                    let metadata = proxy_route_metadata(node.role, node.id.clone());
                    (
                        client,
                        proxy_url,
                        proxy_index,
                        lease,
                        upstream_real_ip,
                        metadata,
                    )
                })
            })
        };

        if let Some((client, proxy_url, proxy_index, lease, upstream_real_ip, metadata)) = selection
        {
            state
                .metrics
                .record_egress_route(EgressRouteMetricClass::Proxy);
            return Ok(SelectedRoute {
                client,
                proxy_url: Some(proxy_url),
                proxy_index: Some(proxy_index),
                lease: Some(lease),
                upstream_real_ip,
                metadata,
            });
        }

        state
            .metrics
            .record_egress_route(EgressRouteMetricClass::DirectHybridFallback);
        return Ok(direct_route(state, RouteKind::DirectHybridFallback));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (selection, recovery_in_progress, route_availability_pending, retry_after) = {
            let mut pool = state.proxy_pool.write().await;
            if pool.proxies.is_empty() {
                return Err(BridgeError::UpstreamError(
                    "Proxy egress mode is configured but the proxy pool is empty".to_string(),
                ));
            }

            let retry_after = pool.minimum_rate_limit_remaining();
            let selection = match excluded_proxy {
                Some(index) => pool.get_client_excluding(routing_key, index),
                None => pool.get_client(routing_key),
            }
            .and_then(|(client, proxy_url, proxy_index)| {
                pool.begin_lease(proxy_index).map(|lease| {
                    let node = &pool.proxies[proxy_index];
                    let upstream_real_ip = verified_exit_real_ip(node.exit_identity.as_ref());
                    let metadata = proxy_route_metadata(node.role, node.id.clone());
                    (
                        client,
                        proxy_url,
                        proxy_index,
                        lease,
                        upstream_real_ip,
                        metadata,
                    )
                })
            });
            let recovery_in_progress = pool.recovery_in_progress();
            let route_availability_pending = pool.route_availability_pending();
            (
                selection,
                recovery_in_progress,
                route_availability_pending,
                retry_after,
            )
        };

        if let Some((client, proxy_url, proxy_index, lease, upstream_real_ip, metadata)) = selection
        {
            state
                .metrics
                .record_egress_route(EgressRouteMetricClass::Proxy);
            return Ok(SelectedRoute {
                client,
                proxy_url: Some(proxy_url),
                proxy_index: Some(proxy_index),
                lease: Some(lease),
                upstream_real_ip,
                metadata,
            });
        }

        if (recovery_in_progress || route_availability_pending)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }

        if let Some(remaining) = retry_after {
            let retry_after = super::policy::client_retry_after(remaining);
            return Err(BridgeError::EgressUnavailable(format!(
                "no unique healthy proxy exit is currently available; managed recovery is still running; retry after {} second(s)",
                retry_after.as_secs()
            )));
        }

        return Err(BridgeError::UpstreamError(
            "Proxy pool is configured but no eligible egress route is available".to_string(),
        ));
    }
}

fn direct_route(state: &AppState, kind: RouteKind) -> SelectedRoute {
    SelectedRoute {
        client: state.http_client.clone(),
        proxy_url: None,
        proxy_index: None,
        lease: None,
        upstream_real_ip: None,
        metadata: RouteMetadata {
            kind,
            proxy_node: None,
        },
    }
}

fn proxy_route_metadata(role: EgressRole, node_id: String) -> RouteMetadata {
    RouteMetadata {
        kind: match role {
            EgressRole::Primary => RouteKind::Proxy,
            EgressRole::WarmStandby => RouteKind::Standby,
        },
        proxy_node: Some(node_id),
    }
}

pub(crate) async fn record_transport_success(state: &AppState, proxy_index: Option<usize>) {
    if let Some(index) = proxy_index {
        state.proxy_pool.write().await.record_success(index);
    }
}

pub(crate) async fn apply_rate_limit_penalty(
    state: &AppState,
    route: &SelectedRoute,
    retry_count: u32,
    retry_after: Option<Duration>,
) {
    if let Some(index) = route.proxy_index {
        let mut pool = state.proxy_pool.write().await;
        if let Some(duration) = retry_after {
            pool.mark_rate_limited(index, duration);
            info!(
                proxy_index = index,
                cooldown_secs = duration.as_secs(),
                "using upstream Retry-After value"
            );
        } else {
            pool.mark_rate_limited_adaptive(index, retry_count);
        }
    } else {
        warn!(
            retry_count,
            "direct upstream rate limit received; host WARP mutation is forbidden"
        );
    }
}

pub(crate) async fn clear_rate_limit_penalty_after_success(
    state: &AppState,
    route: &SelectedRoute,
) {
    let Some(index) = route.proxy_index else {
        return;
    };
    let mut pool = state.proxy_pool.write().await;
    // A provider-quota success proves only that this same egress may serve the
    // provider again. It does not prove transport recovery: keep any open
    // circuit/cooldown/restart queue created by independent network failures.
    pool.clear_rate_limit_recovery(index);
    info!(
        proxy_index = index,
        "same-egress retry succeeded; cleared rate-limit quarantine without switching exits"
    );
}

pub(crate) fn may_change_egress_after_failure(
    class: super::policy::FailureClass,
) -> bool {
    matches!(class, super::policy::FailureClass::Transport | super::policy::FailureClass::Timeout)
}

pub(crate) fn retry_metric_class(class: super::policy::FailureClass) -> crate::observability::RetryMetricClass {
    use super::policy::FailureClass;
    use crate::observability::RetryMetricClass;
    match class {
        FailureClass::Transport => RetryMetricClass::Transport,
        FailureClass::Timeout => RetryMetricClass::Timeout,
        FailureClass::RateLimit => RetryMetricClass::RateLimit,
        FailureClass::ProviderClient => RetryMetricClass::ProviderClient,
        FailureClass::ProviderServer => RetryMetricClass::ProviderServer,
        FailureClass::MalformedResponse | FailureClass::Cancelled => {
            RetryMetricClass::MalformedResponse
        }
    }
}
