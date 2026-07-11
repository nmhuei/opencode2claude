use super::*;
use std::time::{Duration, Instant};

fn make_test_urls(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("socks5://127.0.0.1:{}", 40001 + index))
        .collect()
}

fn five_node_pool() -> ProxyPool {
    ProxyPool::new(&make_test_urls(5))
}

#[test]
fn proxy_pool_mapping_is_sticky() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    assert_eq!(pool.proxies.len(), 3);
    assert_eq!(pool.active_count, 3);
    let first = pool.get_client("agent-1").expect("route").2;
    for _ in 0..100 {
        assert_eq!(pool.get_client("agent-1").expect("route").2, first);
    }
    assert_eq!(pool.proxies[first].role, EgressRole::Primary);
}

#[test]
fn stable_hash_has_cross_build_golden_value() {
    assert_eq!(
        stable_rendezvous_score("agent-x", "opencode-warp-1"),
        15_041_632_887_015_498_948
    );
    assert_ne!(
        stable_rendezvous_score("agent-x", "opencode-warp-1"),
        stable_rendezvous_score("agent-x", "opencode-warp-2")
    );
}

#[test]
fn configured_active_count_only_changes_primary_serving_set() {
    let pool = ProxyPool::new_with_active_count(&make_test_urls(5), 2);
    assert_eq!(pool.active_count, 2);
    assert!(pool.proxies[0].routing_enabled);
    assert!(pool.proxies[1].routing_enabled);
    assert!(!pool.proxies[2].routing_enabled);
    assert_eq!(pool.proxies[3].role, EgressRole::WarmStandby);
    assert_eq!(pool.proxies[3].lifecycle, LifecyclePolicy::Protected);
}

#[test]
fn retry_excludes_failed_proxy_and_prefers_other_primary() {
    let mut pool = five_node_pool();
    let first = pool.get_client("retry-agent").expect("initial route");
    let retry = pool
        .get_client_excluding("retry-agent", first.2)
        .expect("alternate route");
    assert_ne!(first.2, retry.2);
    assert_eq!(pool.proxies[retry.2].role, EgressRole::Primary);
}

#[test]
fn unavailable_sticky_primary_does_not_remap_other_healthy_sessions() {
    let mut pool = five_node_pool();
    let mut selected = None;
    for index in 0..200 {
        let key = format!("session-{index}");
        let route = pool.select_proxy_for_key(&key).expect("route").2;
        let other = format!("other-{index}");
        let other_route = pool.select_proxy_for_key(&other).expect("route").2;
        if route != other_route {
            selected = Some((key, route, other, other_route));
            break;
        }
    }
    let (affected_key, affected_primary, healthy_key, healthy_primary) =
        selected.expect("two distinct primary assignments");

    pool.mark_rate_limited(affected_primary, Duration::from_secs(300));
    let affected_failover = pool
        .select_proxy_for_key(&affected_key)
        .expect("affected failover")
        .2;
    assert_ne!(affected_failover, affected_primary);
    assert_eq!(pool.proxies[affected_failover].role, EgressRole::Primary);
    assert_eq!(
        pool.select_proxy_for_key(&healthy_key)
            .expect("healthy route")
            .2,
        healthy_primary
    );
}

#[test]
fn standby_is_used_only_after_enabled_primaries_are_unavailable() {
    let mut pool = five_node_pool();
    for index in 0..3 {
        pool.mark_rate_limited(index, Duration::from_secs(300));
    }
    let selected = pool.get_client("failover").expect("standby route").2;
    assert_eq!(pool.proxies[selected].role, EgressRole::WarmStandby);
}

#[test]
fn protected_standby_is_never_normal_route() {
    let mut pool = five_node_pool();
    for key in ["a", "b", "c", "d", "e"] {
        let selected = pool.get_client(key).expect("primary route").2;
        assert_eq!(pool.proxies[selected].role, EgressRole::Primary);
    }
}

#[test]
fn duplicate_exit_is_excluded_from_routing() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    let assigned = pool
        .rendezvous_assigned_primary("duplicate-session")
        .expect("assigned primary");
    pool.proxies[assigned].duplicate_of = Some("opencode-warp-owner".to_string());
    let selected = pool
        .select_proxy_for_key("duplicate-session")
        .expect("alternate primary")
        .2;
    assert_ne!(assigned, selected);
}

#[test]
fn half_open_node_accepts_only_one_probe_lease() {
    let mut pool = ProxyPool::new(&make_test_urls(1));
    pool.proxies[0].health = HealthState::Recovering;
    pool.proxies[0].circuit = CircuitState::HalfOpen;
    assert!(pool.select_proxy_for_key("probe").is_some());
    let lease = pool.begin_lease(0).expect("probe lease");
    assert!(pool.select_proxy_for_key("second-probe").is_none());
    drop(lease);
    assert!(pool.select_proxy_for_key("probe-again").is_some());
}

#[test]
fn leases_block_destructive_lifecycle_operations() {
    let pool = ProxyPool::new(&make_test_urls(1));
    let lease = pool.begin_lease(0).expect("request lease");
    assert!(pool
        .can_modify_node(0)
        .unwrap_err()
        .contains("active request"));
    drop(lease);
    assert!(pool.can_modify_node(0).is_ok());
}

#[test]
fn protected_node_cannot_be_modified_even_without_lease() {
    let pool = five_node_pool();
    let standby = pool
        .proxies
        .iter()
        .position(|node| node.role == EgressRole::WarmStandby)
        .expect("standby");
    assert!(pool
        .can_modify_node(standby)
        .unwrap_err()
        .contains("protected"));
}

#[test]
fn mark_rate_limited_opens_circuit() {
    let mut pool = ProxyPool::new(&make_test_urls(1));
    pool.mark_rate_limited(0, Duration::from_secs(60));
    assert_eq!(pool.proxies[0].health, HealthState::Degraded);
    assert!(matches!(pool.proxies[0].circuit, CircuitState::Open { .. }));
    assert!(pool.select_proxy_for_key("blocked").is_none());
}

#[test]
fn expired_circuit_becomes_half_open_and_successes_close_it() {
    let mut pool = ProxyPool::new(&make_test_urls(1));
    pool.proxies[0].health = HealthState::Degraded;
    pool.proxies[0].circuit = CircuitState::Open {
        until: Instant::now() - Duration::from_secs(1),
    };
    pool.proxies[0].cooldown_until = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(pool.recover_expired_cooldowns(), 1);
    assert_eq!(pool.proxies[0].health, HealthState::Recovering);
    assert_eq!(pool.proxies[0].circuit, CircuitState::HalfOpen);

    pool.record_success(0);
    assert_eq!(pool.proxies[0].circuit, CircuitState::HalfOpen);
    pool.record_success(0);
    assert_eq!(pool.proxies[0].health, HealthState::Healthy);
    assert_eq!(pool.proxies[0].circuit, CircuitState::Closed);
}

#[test]
fn transport_failure_degrades_then_opens_managed_node() {
    let mut pool = ProxyPool::new(&make_test_urls(1));
    pool.record_failure(0);
    assert_eq!(pool.proxies[0].health, HealthState::Degraded);
    assert!(pool.restart_queue.is_empty());

    pool.record_failure(0);
    assert_eq!(pool.proxies[0].health, HealthState::Unhealthy);
    assert!(matches!(pool.proxies[0].circuit, CircuitState::Open { .. }));
    assert_eq!(pool.restart_queue, vec![0]);
}

#[test]
fn success_clears_single_transport_failure_without_false_recovery() {
    let mut pool = ProxyPool::new(&make_test_urls(1));
    pool.record_failure(0);
    pool.record_success(0);
    assert_eq!(pool.proxies[0].consecutive_failures, 0);
    assert_eq!(pool.proxies[0].health, HealthState::Degraded);
}

#[test]
fn snapshot_exposes_independent_state_dimensions() {
    let mut pool = five_node_pool();
    pool.mark_rate_limited(1, Duration::from_secs(60));
    let _lease_one = pool.begin_lease(0).expect("lease one");
    let _lease_two = pool.begin_lease(0).expect("lease two");
    pool.proxies[0].exit_identity = Some(ExitIdentity {
        public_ip: "203.0.113.10".to_string(),
        provider: Some("warp".to_string()),
        colo: Some("SIN".to_string()),
        verified_at_unix_secs: 1,
    });

    let stats = pool.snapshot();
    assert_eq!(stats.policy, "primary-with-protected-warm-standby");
    assert_eq!(stats.primary.total, 3);
    assert_eq!(stats.primary.cooldown, 1);
    assert_eq!(stats.primary.active_requests, 2);
    assert_eq!(stats.primary.unique_verified_exits, 1);
    assert_eq!(stats.warm_standby.total, 2);
    assert!(stats.warm_standby.protected);
    assert_eq!(stats.nodes[0].health, HealthState::Healthy);
    assert_eq!(stats.nodes[0].circuit, "closed");
}

#[test]
fn restart_queue_drain_is_atomic() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    pool.restart_queue.extend([0, 1]);
    assert_eq!(pool.drain_restart_queue(), vec![0, 1]);
    assert!(pool.drain_restart_queue().is_empty());
}

#[test]
fn helper_contracts_are_stable() {
    assert_eq!(extract_port("socks5://127.0.0.1:40001"), 40001);
    assert_eq!(extract_port("http://127.0.0.1:8080/"), 8080);
    assert_eq!(extract_port("invalid"), 0);
    assert_eq!(
        container_name("socks5://127.0.0.1:40001"),
        "opencode-warp-1"
    );
    assert!(is_managed_proxy_port(40001));
    assert!(is_protected_proxy_port(40004));
    assert!(ensure_not_protected(40004).is_err());
}

#[test]
fn empty_pool_returns_none() {
    assert!(ProxyPool::default()
        .select_proxy_for_key("session")
        .is_none());
}
