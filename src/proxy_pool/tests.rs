use super::*;
use std::time::Duration;
use tracing::info;

fn make_test_urls(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect()
}

#[test]
fn test_proxy_pool_mapping() {
    let urls = make_test_urls(3);
    let mut pool = ProxyPool::new(&urls);
    // Three configured primary proxies are active by default.
    assert_eq!(pool.proxies.len(), 3);
    assert_eq!(pool.active_count, 3);

    // Same API key should always map to same proxy index
    let res1 = pool.get_client("agent-1").unwrap();
    let res2 = pool.get_client("agent-1").unwrap();
    assert_eq!(res1.2, res2.2);

    // Different API keys may map to different indexes
    let res3 = pool.get_client("agent-2").unwrap();
    info!("agent-1 mapped to preferred proxy index {}", res1.2);
    info!("agent-2 mapped to preferred proxy index {}", res3.2);
}

#[test]
fn test_sticky_mapping_stable() {
    let urls = make_test_urls(3);
    let pool = ProxyPool::new(&urls);

    let agent = "sticky-agent-42";
    let first = pool.select_proxy_for_key(agent).unwrap().2;

    for _ in 0..100 {
        let result = pool.select_proxy_for_key(agent).unwrap();
        assert_eq!(
            result.2, first,
            "sticky mapping changed for key '{}' on iteration",
            agent
        );
        assert_eq!(
            pool.proxies[result.2].role,
            ProxyRole::Primary,
            "sticky agent mapped to non-Primary proxy"
        );
    }
}

#[test]
fn test_affected_agent_only_remap() {
    let urls: Vec<String> = (0..5)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let mut pool = ProxyPool::new(&urls);

    let a_idx = pool.select_proxy_for_key("agent_a").unwrap().2;
    let b_idx = pool.select_proxy_for_key("agent_b").unwrap().2;
    let c_idx = pool.select_proxy_for_key("agent_c").unwrap().2;

    assert_eq!(pool.proxies[a_idx].role, ProxyRole::Primary);
    assert_eq!(pool.proxies[b_idx].role, ProxyRole::Primary);
    assert_eq!(pool.proxies[c_idx].role, ProxyRole::Primary);

    let a_primary = a_idx;
    let b_primary = b_idx;
    let c_primary = c_idx;

    pool.mark_rate_limited(b_primary, Duration::from_secs(300));

    let b_failover = pool.select_proxy_for_key("agent_b").unwrap().2;
    assert_eq!(
        pool.proxies[b_failover].role,
        ProxyRole::WarmStandby,
        "agent_b should failover to WarmStandby, got index {} role {:?}",
        b_failover,
        pool.proxies[b_failover].role
    );

    let a_after = pool.select_proxy_for_key("agent_a").unwrap().2;
    assert_eq!(
        a_after, a_primary,
        "agent_a remapped from primary {} to {}, expected no change",
        a_primary, a_after
    );

    let c_after = pool.select_proxy_for_key("agent_c").unwrap().2;
    assert_eq!(
        c_after, c_primary,
        "agent_c remapped from primary {} to {}, expected no change",
        c_primary, c_after
    );
}

#[test]
fn test_temporary_failover_to_warm_standby() {
    let urls: Vec<String> = (0..5)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let mut pool = ProxyPool::new(&urls);

    let primary = pool.select_proxy_for_key("failover-agent").unwrap().2;
    assert_eq!(pool.proxies[primary].role, ProxyRole::Primary);

    pool.mark_rate_limited(primary, Duration::from_secs(300));

    let result = pool.select_proxy_for_key("failover-agent").unwrap();
    assert_eq!(
        pool.proxies[result.2].role,
        ProxyRole::WarmStandby,
        "failover should route to WarmStandby, got idx {} role {:?}",
        result.2,
        pool.proxies[result.2].role
    );
}

#[test]
fn test_recovery_returns_to_primary() {
    let urls: Vec<String> = (0..3)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let mut pool = ProxyPool::new(&urls);

    let primary_idx = pool.select_proxy_for_key("recovery-agent").unwrap().2;
    assert_eq!(pool.proxies[primary_idx].role, ProxyRole::Primary);
    assert_eq!(pool.proxies[primary_idx].status, ProxyStatus::Active);

    pool.mark_rate_limited(primary_idx, Duration::from_secs(0));

    let result = pool.select_proxy_for_key("recovery-agent").unwrap();
    assert_eq!(
        result.2, primary_idx,
        "after cooldown expiry, agent should return to original primary {} not {}",
        primary_idx, result.2
    );
}

#[test]
fn test_no_standby_if_selected_primary_healthy() {
    let urls: Vec<String> = (0..5)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let pool = ProxyPool::new(&urls);

    for key in &["test-a", "test-b", "test-c", "test-d", "test-e"] {
        let result = pool.select_proxy_for_key(key).unwrap();
        assert_eq!(
            pool.proxies[result.2].role,
            ProxyRole::Primary,
            "key '{}' selected standby when primary was healthy",
            key
        );
    }
}

#[test]
fn test_rendezvous_deterministic() {
    let urls: Vec<String> = (0..3)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let _pool = ProxyPool::new(&urls);

    let score1 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40001");
    let score2 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40001");
    assert_eq!(score1, score2, "rendezvous score must be deterministic");

    let score3 = stable_rendezvous_score("agent-x", "socks5://127.0.0.1:40002");
    assert_ne!(
        score1, score3,
        "different nodes should have different scores"
    );
}

#[test]
fn test_warm_standby_excluded_from_normal_routing() {
    let urls: Vec<String> = (0..5)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let mut pool = ProxyPool::new(&urls);

    assert_eq!(pool.proxies.len(), 5);
    assert_eq!(pool.proxies[0].role, ProxyRole::Primary);
    assert_eq!(pool.proxies[1].role, ProxyRole::Primary);
    assert_eq!(pool.proxies[2].role, ProxyRole::Primary);
    assert_eq!(pool.proxies[3].role, ProxyRole::WarmStandby);
    assert_eq!(pool.proxies[4].role, ProxyRole::WarmStandby);

    for key in &["alpha", "beta", "gamma", "delta", "epsilon"] {
        let (_, _, idx) = pool.get_client(key).unwrap();
        assert!(
            pool.proxies[idx].role == ProxyRole::Primary,
            "get_client('{}') returned WarmStandby (idx {}), expected Primary",
            key,
            idx
        );
    }

    for i in 0..3 {
        pool.mark_rate_limited(i, Duration::from_secs(300));
    }

    let (_, _, idx) = pool.get_client("failover-test").unwrap();
    assert!(
        pool.proxies[idx].role == ProxyRole::WarmStandby,
        "expected WarmStandby in failover, got role={:?} at idx={}",
        pool.proxies[idx].role,
        idx
    );
}

#[test]
fn test_empty_pool_returns_none() {
    let mut pool = ProxyPool::default();
    assert!(pool.get_client("test").is_none());
}

#[test]
fn test_mark_healthy() {
    let urls = make_test_urls(1);
    let mut pool = ProxyPool::new(&urls);

    pool.mark_rate_limited(0, Duration::from_secs(60));
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));

    pool.mark_healthy(0);
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));
}

#[test]
fn test_drain_restart_queue() {
    let urls = make_test_urls(3);
    let mut pool = ProxyPool::new(&urls);
    assert!(pool.drain_restart_queue().is_empty());

    pool.restart_queue.push(0);
    pool.restart_queue.push(1);
    assert_eq!(pool.drain_restart_queue().len(), 2);
    assert!(pool.drain_restart_queue().is_empty());
}

#[test]
fn test_container_name_generation() {
    assert_eq!(
        container_name("socks5://127.0.0.1:40001"),
        "opencode-warp-1"
    );
    assert_eq!(
        container_name("socks5://127.0.0.1:40005"),
        "opencode-warp-5"
    );
    assert_eq!(
        container_name("http://127.0.0.1:9999"),
        "opencode-proxy-9999"
    );
}

#[test]
fn test_extract_port() {
    assert_eq!(extract_port("socks5://127.0.0.1:40001"), 40001);
    assert_eq!(extract_port("http://127.0.0.1:8080/"), 8080);
    assert_eq!(extract_port("invalid"), 0);
}

#[test]
fn test_record_failure_queues_managed_proxy_restart() {
    let urls = make_test_urls(1);
    let mut pool = ProxyPool::new(&urls);

    assert_eq!(pool.proxies[0].consecutive_failures, 0);
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));

    pool.record_failure(0);
    assert_eq!(pool.proxies[0].consecutive_failures, 1);
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));
    assert!(pool.restart_queue.is_empty());

    pool.record_failure(0);
    assert!(matches!(
        pool.proxies[0].status,
        ProxyStatus::Dead {
            restart_attempts: 0
        }
    ));
    assert_eq!(pool.restart_queue, vec![0]);
}

#[test]
fn test_http_400_does_not_mark_proxy_failed() {
    let urls = make_test_urls(1);
    let mut pool = ProxyPool::new(&urls);

    pool.record_failure(0);
    assert_eq!(pool.proxies[0].consecutive_failures, 1);

    pool.record_success(0);
    assert_eq!(pool.proxies[0].consecutive_failures, 0);
    assert_eq!(pool.proxies[0].consecutive_successes, 1);
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));
}

#[test]
fn test_health_json_contains_proxy_pool() {
    let urls: Vec<String> = (0..5)
        .map(|i| format!("socks5://127.0.0.1:{}", 40001 + i))
        .collect();
    let pool = ProxyPool::new(&urls);
    let stats = pool.snapshot();

    assert_eq!(stats.policy, "primary-with-warm-standby");

    assert_eq!(stats.primary.ports, vec![40001, 40002, 40003]);
    assert_eq!(stats.primary.total, 3);
    assert_eq!(stats.primary.healthy, 3);
    assert!(!stats.primary.protected);

    assert_eq!(stats.warm_standby.ports, vec![40004, 40005]);
    assert_eq!(stats.warm_standby.total, 2);
    assert_eq!(stats.warm_standby.healthy, 2);
    assert!(stats.warm_standby.protected);

    assert_eq!(stats.nodes.len(), 5);
    assert_eq!(stats.nodes[0].role, ProxyRole::Primary);
    assert_eq!(stats.nodes[3].role, ProxyRole::WarmStandby);
    assert_eq!(stats.nodes[3].lifecycle, ProxyLifecycle::Protected);
    assert!(stats.nodes[0].cooldown_remaining_secs.is_none());
}

#[test]
fn test_snapshot_shows_cooldown_count() {
    let urls = make_test_urls(5);
    let mut pool = ProxyPool::new(&urls);

    pool.mark_rate_limited(1, Duration::from_secs(60));
    pool.mark_rate_limited(2, Duration::from_secs(120));
    pool.mark_rate_limited(3, Duration::from_secs(300));

    let stats = pool.snapshot();

    assert_eq!(stats.primary.cooldown, 2);
    assert_eq!(stats.primary.healthy, 1);
    assert_eq!(stats.warm_standby.cooldown, 1);
    assert_eq!(stats.warm_standby.healthy, 1);
}

#[test]
fn test_recover_expired_cooldowns_marks_active() {
    let urls = make_test_urls(1);
    let mut pool = ProxyPool::new(&urls);

    pool.mark_rate_limited(0, Duration::from_secs(0));
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));

    let recovered = pool.recover_expired_cooldowns();
    assert_eq!(recovered, 1);
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Active));
    assert_eq!(pool.proxies[0].consecutive_failures, 0);
    assert_eq!(pool.proxies[0].consecutive_successes, 0);
}

#[test]
fn test_record_success_recovers_after_threshold() {
    let urls = make_test_urls(1);
    let mut pool = ProxyPool::new(&urls);

    pool.mark_rate_limited(0, Duration::from_secs(60));
    assert!(matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)));
    assert_eq!(pool.proxies[0].consecutive_successes, 0);

    pool.record_success(0);
    assert!(
        matches!(pool.proxies[0].status, ProxyStatus::Cooldown(_)),
        "still in cooldown after 1 success"
    );
    assert_eq!(pool.proxies[0].consecutive_successes, 1);

    pool.record_success(0);
    assert!(
        matches!(pool.proxies[0].status, ProxyStatus::Active),
        "recovered after {} successes",
        RECOVERY_SUCCESS_COUNT
    );
    assert_eq!(pool.proxies[0].consecutive_failures, 0);
    assert_eq!(pool.proxies[0].consecutive_successes, 0);
}

#[test]
fn test_retry_excludes_failed_proxy_and_prefers_other_primary() {
    let urls = make_test_urls(5);
    let mut pool = ProxyPool::new(&urls);
    let key = "retry-agent";

    let first = pool.get_client(key).expect("initial proxy");
    let retry = pool
        .get_client_excluding(key, first.2)
        .expect("alternate proxy");

    assert_ne!(first.2, retry.2, "retry selected the excluded proxy again");
    assert_eq!(
        pool.proxies[retry.2].role,
        ProxyRole::Primary,
        "another healthy primary should be tried before warm standby"
    );
}
