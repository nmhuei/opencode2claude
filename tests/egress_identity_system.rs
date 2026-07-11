//! Opt-in system tests for real local WARP SOCKS proxies.
//!
//! Run explicitly:
//! `cargo test --test egress_identity_system -- --ignored --nocapture`

use opencode2api::proxy_pool::{refresh_exit_identities, ProxyPool};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::test]
#[ignore = "requires local WARP SOCKS proxies on ports 40001-40003 and Internet access"]
async fn real_warp_identity_consensus_and_duplicate_suppression() {
    let urls = [40001_u16, 40002, 40003]
        .into_iter()
        .map(|port| format!("socks5h://127.0.0.1:{port}"))
        .collect::<Vec<_>>();
    let pool = Arc::new(RwLock::new(ProxyPool::new_with_policy(&urls, 3, true)));
    let endpoints = vec![
        "https://cloudflare.com/cdn-cgi/trace".to_string(),
        "https://api.ipify.org?format=json".to_string(),
    ];

    refresh_exit_identities(pool.clone(), &endpoints).await;
    let pool = pool.read().await;
    let snapshot = pool.snapshot();

    for node in &snapshot.nodes {
        println!(
            "node={} port={} ip={:?} duplicate_of={:?}",
            node.id,
            node.port,
            node.exit_identity
                .as_ref()
                .map(|identity| identity.public_ip.as_str()),
            node.duplicate_of
        );
    }

    assert!(
        snapshot
            .nodes
            .iter()
            .all(|node| node.exit_identity.is_some()),
        "all three active WARP nodes must reach identity consensus"
    );
    assert!(pool.verified_unique_exit_count() >= 1);
    assert!(pool.egress_ready(1, std::time::Duration::from_secs(300)));

    // When Cloudflare assigns the same public exit to multiple containers, all
    // but one must be marked as duplicate and excluded from routing capacity.
    let unique = pool.verified_unique_exit_count();
    let duplicate_count = snapshot
        .nodes
        .iter()
        .filter(|node| node.duplicate_of.is_some())
        .count();
    assert_eq!(unique + duplicate_count, snapshot.nodes.len());
}
