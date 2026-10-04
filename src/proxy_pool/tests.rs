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

fn identity(ip: &str) -> ExitIdentity {
    ExitIdentity {
        public_ip: ip.to_string(),
        provider: Some("cloudflare-warp".to_string()),
        colo: Some("HKG".to_string()),
        verified_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

#[test]
fn route_availability_pending_while_required_identity_is_missing() {
    let mut pool = ProxyPool::new_with_policy(&["socks5://127.0.0.1:40001".to_string()], 1, true);
    pool.proxies[0].health = HealthState::Healthy;
    pool.proxies[0].exit_identity = None;
    assert!(pool.route_availability_pending());

    pool.proxies[0].health = HealthState::Unhealthy;
    pool.proxies[0].circuit = CircuitState::Open {
        until: Instant::now() + Duration::from_secs(60),
    };
    assert!(!pool.route_availability_pending());
}

#[test]
fn proxy_pool_round_robin_distributes_across_primaries() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    assert_eq!(pool.proxies.len(), 3);
    assert_eq!(pool.active_count, 3);
    // With 3 healthy primaries, consecutive calls should cycle 0 -> 1 -> 2 -> 0 ...
    let mut seen = std::collections::HashSet::new();
    for _ in 0..6 {
        let index = pool.get_client("agent-1").expect("route").2;
        seen.insert(index);
        assert_eq!(pool.proxies[index].role, EgressRole::Primary);
    }
    // All 3 primaries must have been selected at least once.
    assert_eq!(seen.len(), 3, "round-robin should use all 3 primaries");
}

#[test]
fn concurrent_burst_reuses_active_primary_before_advancing_round_robin() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    let first = pool.get_client("claude-burst").expect("first route").2;
    assert_eq!(first, 0);
    let _lease = pool.begin_lease(first).expect("active lease");

    let second = pool
        .get_client("claude-burst")
        .expect("reuse active route")
        .2;
    assert_eq!(
        second, first,
        "concurrent Claude Code burst should share the in-flight primary"
    );

    let retry = pool
        .get_client_excluding("claude-burst", first)
        .expect("explicit retry still avoids failed proxy")
        .2;
    assert_ne!(retry, first);
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
fn round_robin_skips_rate_limited_node_and_continues_cycling() {
    let mut pool = five_node_pool();
    // Rate-limit node 0 — round-robin should skip it entirely.
    pool.mark_rate_limited(0, Duration::from_secs(300));
    let mut seen = std::collections::HashSet::new();
    for _ in 0..10 {
        let index = pool.select_proxy_for_key("any").expect("route").2;
        seen.insert(index);
        assert_ne!(index, 0, "rate-limited node 0 must never be selected");
        assert_eq!(pool.proxies[index].role, EgressRole::Primary);
    }
    // Remaining 2 healthy primaries (index 1 and 2) must both be used.
    assert_eq!(
        seen.len(),
        2,
        "round-robin should use both healthy primaries"
    );
}

#[test]
fn offline_unverified_standby_is_not_selected_after_primaries_are_unavailable() {
    let mut pool = five_node_pool();
    for index in 0..3 {
        pool.mark_rate_limited(index, Duration::from_secs(300));
    }
    assert!(pool.get_client("failover").is_none());
}

#[test]
fn verified_healthy_standby_is_used_only_after_primaries_are_unavailable() {
    let mut pool = five_node_pool();
    for index in 0..3 {
        pool.mark_rate_limited(index, Duration::from_secs(300));
    }
    pool.proxies[3].exit_identity = Some(identity("1.1.1.1"));
    pool.proxies[3].health = HealthState::Healthy;
    let selected = pool
        .get_client("failover")
        .expect("verified standby route")
        .2;
    assert_eq!(selected, 3);
    assert_eq!(pool.proxies[selected].role, EgressRole::WarmStandby);
}

#[test]
fn one_plus_one_standby_keeps_egress_ready_when_primary_is_unhealthy() {
    let urls = vec![
        "socks5h://127.0.0.1:40001".to_string(),
        "socks5h://127.0.0.1:40004".to_string(),
    ];
    let mut pool = ProxyPool::new_with_egress_policy(&urls, 1, false, Duration::from_secs(300));

    pool.proxies[0].health = HealthState::Unhealthy;
    pool.proxies[0].circuit = CircuitState::Open {
        until: Instant::now() + Duration::from_secs(300),
    };
    pool.proxies[1].health = HealthState::Healthy;
    pool.proxies[1].circuit = CircuitState::Closed;
    pool.proxies[1].exit_identity = Some(identity("1.1.1.1"));

    let selected = pool
        .select_proxy_for_key("one-plus-one-failover")
        .expect("warm standby must remain routable")
        .2;
    assert_eq!(selected, 1);
    assert_eq!(pool.proxies[selected].role, EgressRole::WarmStandby);
    assert!(
        pool.egress_ready(1, Duration::from_secs(300)),
        "readiness must remain true while a verified warm standby is routable"
    );
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
    // Mark node 0 as a duplicate exit — round-robin must skip it.
    pool.proxies[0].duplicate_of = Some("opencode-warp-owner".to_string());
    for _ in 0..6 {
        let selected = pool
            .select_proxy_for_key("duplicate-session")
            .expect("alternate primary")
            .2;
        assert_ne!(selected, 0, "duplicate node must never be selected");
    }
}

#[test]
fn rate_limit_recovery_is_never_used_as_half_open_probe() {
    let mut pool = ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]);
    pool.proxies[0].exit_identity = Some(identity("1.1.1.1"));
    pool.mark_rate_limited(0, Duration::from_secs(3_600));
    pool.drain_restart_queue();
    pool.proxies[0].health = HealthState::Recovering;
    pool.proxies[0].circuit = CircuitState::HalfOpen;
    pool.proxies[0].restart_attempts = pool.max_restart_attempts;

    assert!(pool.select_proxy_for_key("must-not-probe").is_none());
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
fn provider_rate_limit_does_not_degrade_proxy_transport_health() {
    let mut pool = ProxyPool::new(&make_test_urls(1));
    pool.proxies[0].health = HealthState::Healthy;
    pool.proxies[0].circuit = CircuitState::Closed;

    pool.mark_rate_limited(0, Duration::from_secs(60));

    assert_eq!(
        pool.proxies[0].health,
        HealthState::Healthy,
        "provider cooldown must not be recorded as a proxy transport failure"
    );
    assert_eq!(
        pool.proxies[0].circuit,
        CircuitState::Closed,
        "provider cooldown must not open the transport circuit"
    );
    assert_eq!(
        pool.proxies[0].recovery_cause,
        Some(RecoveryCause::RateLimit)
    );
    assert!(
        pool.restart_queue.is_empty(),
        "rate-limit cooldown must not rotate or restart egress identity"
    );
    assert!(
        pool.select_proxy_for_key("blocked").is_none(),
        "the rate-limited provider route itself must still be excluded"
    );
}

#[test]
fn one_rate_limited_primary_does_not_gate_other_healthy_primaries() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    for node in &mut pool.proxies {
        node.health = HealthState::Healthy;
        node.circuit = CircuitState::Closed;
    }

    pool.mark_rate_limited(0, Duration::from_secs(120));

    let selected = pool
        .select_proxy_for_key("fresh-claude-request")
        .expect("healthy non-rate-limited primary should stay routable")
        .2;
    assert_ne!(selected, 0);
    assert_eq!(pool.proxies[selected].health, HealthState::Healthy);
    assert_eq!(pool.proxies[selected].circuit, CircuitState::Closed);
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
    assert_eq!(
        stats.primary.cooldown, 0,
        "provider cooldown is tracked separately from transport circuit cooldown"
    );
    assert_eq!(stats.primary.active_requests, 2);
    assert_eq!(stats.primary.unique_verified_exits, 1);
    assert_eq!(stats.warm_standby.total, 2);
    assert!(stats.warm_standby.protected);
    assert_eq!(stats.nodes[0].health, HealthState::Healthy);
    assert_eq!(stats.nodes[0].circuit, "closed");
    assert_eq!(stats.nodes[1].health, HealthState::Healthy);
    assert_eq!(stats.nodes[1].circuit, "closed");
    assert_eq!(
        stats.nodes[1].recovery_cause,
        Some(RecoveryCause::RateLimit)
    );
    assert!(stats.nodes[1].cooldown_remaining_secs.is_none());
}

#[test]
fn restart_queue_drain_is_atomic() {
    let mut pool = ProxyPool::new(&make_test_urls(3));
    pool.restart_queue.extend([0, 1]);
    assert_eq!(pool.drain_restart_queue(), vec![0, 1]);
    assert!(pool.drain_restart_queue().is_empty());
}

#[test]
fn pool_normalizes_socks5_urls_to_remote_dns() {
    let pool = ProxyPool::new(&["socks5://127.0.0.1:40001".to_string()]);
    assert_eq!(pool.proxies.len(), 1);
    assert_eq!(pool.proxies[0].url, "socks5h://127.0.0.1:40001");
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
