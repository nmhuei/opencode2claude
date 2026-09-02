use opencode2api::provider::adapters::FailureClass;
use opencode2api::provider::types::{
    AuthScheme, CapacityDemand, Credential, CredentialPool, CredentialPoolMember, ModelAlias,
    ModelInfo, PoolStrategy, Provider, ProviderKind, ProviderProtocol, SecretSource,
};
use opencode2api::provider::{CapacityScheduler, ProviderRegistry};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

fn registry_with_pool() -> ProviderRegistry {
    registry_with_pool_strategy(PoolStrategy::RoundRobin)
}

fn registry_with_pool_strategy(strategy: PoolStrategy) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry
        .register_provider(Provider {
            id: "bai".into(),
            name: "B.AI".into(),
            kind: ProviderKind::Bai,
            base_url: "https://api.b.ai/v1".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();
    for id in ["key-a", "key-b", "key-c"] {
        registry
            .register_credential(Credential {
                id: id.into(),
                provider_id: "bai".into(),
                source: SecretSource::Env {
                    variable: id.to_ascii_uppercase().to_string(),
                },
                auth_scheme: AuthScheme::Bearer,
            })
            .unwrap();
    }
    registry.insert_model(ModelInfo {
        provider_id: "bai".into(),
        model_id: "deepseek-1m".into(),
        wire_model_id: "deepseek-1m".into(),
        context_window: Some(1_000_000),
        max_output_tokens: Some(128_000),
        supports_thinking: false,
        verified_context: true,
        free: true,
    });
    registry
        .upsert_pool(
            CredentialPool::new(
                "free-1m",
                "bai",
                strategy,
                ["key-a", "key-b", "key-c"]
                    .into_iter()
                    .map(|id| {
                        let weight =
                            if strategy == PoolStrategy::WeightedRoundRobin && id == "key-a" {
                                3
                            } else {
                                1
                            };
                        CredentialPoolMember::new(
                            id,
                            id.replace("key", "account"),
                            1,
                            None,
                            None,
                            weight,
                        )
                        .unwrap()
                    })
                    .collect(),
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_alias(ModelAlias::one_million(
            "free-1m",
            vec![opencode2api::provider::types::ModelCandidate {
                provider_id: "bai".into(),
                model_id: "deepseek-1m".into(),
                credential_id: None,
                credential_pool_id: Some("free-1m".into()),
                priority: 0,
            }],
        ))
        .unwrap();
    registry
}

#[test]
fn three_concurrent_admissions_use_three_independent_credentials() {
    let registry = registry_with_pool();
    let routes = registry.resolve_routes("free-1m").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let demand = CapacityDemand::new(100, 100);
    let now = Instant::now();
    let mut leases = Vec::new();
    for _ in 0..3 {
        leases.push(
            scheduler
                .admit(&routes, demand, &BTreeSet::new(), now)
                .unwrap(),
        );
    }
    let selected = leases
        .iter()
        .map(|lease| lease.target().credential_id.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(selected, ["key-a".into(), "key-b".into(), "key-c".into()]);
    assert!(scheduler
        .admit(&routes, demand, &BTreeSet::new(), now)
        .is_err());
}

#[test]
fn releasing_a_lease_allows_the_next_admission_without_double_release() {
    let registry = registry_with_pool();
    let routes = registry.resolve_routes("free-1m").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let lease = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    drop(lease);
    let next = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    drop(next);
    assert_eq!(scheduler.summary(now).in_flight, 0);
}

#[test]
fn shared_quota_scope_limits_sibling_credentials() {
    let mut registry = registry_with_pool();
    registry.remove_alias("free-1m").unwrap();
    registry.remove_pool("free-1m").unwrap();
    registry
        .upsert_pool(
            CredentialPool::new(
                "shared",
                "bai",
                PoolStrategy::RoundRobin,
                ["key-a", "key-b"]
                    .into_iter()
                    .map(|id| {
                        CredentialPoolMember::new(id, "same-account", 2, Some(1), None, 1).unwrap()
                    })
                    .collect(),
            )
            .unwrap(),
        )
        .unwrap();
    let routes = vec![opencode2api::provider::types::RouteTarget {
        alias_id: "free-1m".into(),
        client_model: "sonnet[1m]".into(),
        provider_id: "bai".into(),
        model_id: "deepseek-1m".into(),
        wire_model_id: "deepseek-1m".into(),
        binding: opencode2api::provider::types::CredentialBinding::Pool("shared".into()),
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        candidate_index: 0,
    }];
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let first = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    let second = scheduler.admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now);
    assert!(second.is_err());
    drop(first);
}

#[test]
fn weighted_round_robin_observes_member_weights() {
    let registry = registry_with_pool_strategy(PoolStrategy::WeightedRoundRobin);
    let routes = registry.resolve_routes("free-1m").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let mut selected = Vec::new();
    for _ in 0..4 {
        let lease = scheduler
            .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
            .unwrap();
        selected.push(lease.target().credential_id.clone().unwrap());
        drop(lease);
    }
    assert_eq!(
        selected,
        [
            "key-a".into(),
            "key-a".into(),
            "key-a".into(),
            "key-b".into()
        ]
    );
}

#[test]
fn rate_limit_failure_cools_only_the_selected_credential() {
    let registry = registry_with_pool();
    let routes = registry.resolve_routes("free-1m").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let mut first = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    assert_eq!(
        first.target().credential_id.as_ref().unwrap().as_ref(),
        "key-a"
    );
    first.fail(
        FailureClass::RateLimit,
        Some(std::time::Duration::from_secs(30)),
        now,
    );
    let second = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    assert_eq!(
        second.target().credential_id.as_ref().unwrap().as_ref(),
        "key-b"
    );
}

#[test]
fn rate_limit_failure_cools_shared_quota_scope_for_all_members() {
    let mut registry = registry_with_pool();
    registry.remove_alias("free-1m").unwrap();
    registry.remove_pool("free-1m").unwrap();
    registry
        .upsert_pool(
            CredentialPool::new(
                "shared-cooldown",
                "bai",
                PoolStrategy::RoundRobin,
                vec![
                    CredentialPoolMember::new("key-a", "same-account", 2, None, None, 1).unwrap(),
                    CredentialPoolMember::new("key-b", "same-account", 2, None, None, 1).unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_alias(ModelAlias::one_million(
            "shared-cooldown",
            vec![opencode2api::provider::types::ModelCandidate {
                provider_id: "bai".into(),
                model_id: "deepseek-1m".into(),
                credential_id: None,
                credential_pool_id: Some("shared-cooldown".into()),
                priority: 0,
            }],
        ))
        .unwrap();
    let routes = registry.resolve_routes("shared-cooldown").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let mut first = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    first.fail(
        FailureClass::RateLimit,
        Some(std::time::Duration::from_secs(30)),
        now,
    );
    assert!(matches!(
        scheduler.admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now,),
        Err(opencode2api::provider::AdmissionError::Exhausted { .. })
    ));
}

#[test]
fn least_loaded_compares_fractional_load_without_integer_rounding() {
    let mut registry = registry_with_pool();
    registry.remove_alias("free-1m").unwrap();
    registry.remove_pool("free-1m").unwrap();
    registry
        .upsert_pool(
            CredentialPool::new(
                "least-loaded",
                "bai",
                PoolStrategy::LeastLoaded,
                vec![
                    CredentialPoolMember::new("key-a", "account-a", 3, None, None, 1).unwrap(),
                    CredentialPoolMember::new("key-b", "account-b", 2, None, None, 1).unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_alias(ModelAlias::one_million(
            "least-loaded",
            vec![opencode2api::provider::types::ModelCandidate {
                provider_id: "bai".into(),
                model_id: "deepseek-1m".into(),
                credential_id: None,
                credential_pool_id: Some("least-loaded".into()),
                priority: 0,
            }],
        ))
        .unwrap();
    let routes = registry.resolve_routes("least-loaded").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let first = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    assert_eq!(
        first.target().credential_id.as_ref().unwrap().as_ref(),
        "key-a"
    );
    let second = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    assert_eq!(
        second.target().credential_id.as_ref().unwrap().as_ref(),
        "key-b"
    );
}

#[test]
fn quota_bucket_refills_from_injected_monotonic_time() {
    let mut registry = registry_with_pool();
    registry.remove_alias("free-1m").unwrap();
    registry.remove_pool("free-1m").unwrap();
    registry
        .upsert_pool(
            CredentialPool::new(
                "rpm-one",
                "bai",
                PoolStrategy::RoundRobin,
                vec![CredentialPoolMember::new("key-a", "account-a", 1, Some(1), None, 1).unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_alias(ModelAlias::one_million(
            "rpm-one",
            vec![opencode2api::provider::types::ModelCandidate {
                provider_id: "bai".into(),
                model_id: "deepseek-1m".into(),
                credential_id: None,
                credential_pool_id: Some("rpm-one".into()),
                priority: 0,
            }],
        ))
        .unwrap();
    let routes = registry.resolve_routes("rpm-one").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let first = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    drop(first);
    assert!(matches!(
        scheduler.admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now),
        Err(opencode2api::provider::AdmissionError::Exhausted {
            retry_after: Some(_)
        })
    ));
    let refilled = scheduler
        .admit(
            &routes,
            CapacityDemand::new(1, 1),
            &BTreeSet::new(),
            now + std::time::Duration::from_secs(60),
        )
        .unwrap();
    drop(refilled);
}

#[test]
fn demand_larger_than_scope_bucket_is_rejected_without_retry_loop() {
    let mut registry = registry_with_pool();
    registry.remove_alias("free-1m").unwrap();
    registry.remove_pool("free-1m").unwrap();
    registry
        .upsert_pool(
            CredentialPool::new(
                "tiny-tpm",
                "bai",
                PoolStrategy::RoundRobin,
                vec![CredentialPoolMember::new("key-a", "account-a", 1, None, Some(2), 1).unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_alias(ModelAlias::one_million(
            "tiny-tpm",
            vec![opencode2api::provider::types::ModelCandidate {
                provider_id: "bai".into(),
                model_id: "deepseek-1m".into(),
                credential_id: None,
                credential_pool_id: Some("tiny-tpm".into()),
                priority: 0,
            }],
        ))
        .unwrap();
    let routes = registry.resolve_routes("tiny-tpm").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    assert!(matches!(
        scheduler.admit(
            &routes,
            CapacityDemand::new(1, 1),
            &BTreeSet::new(),
            Instant::now(),
        ),
        Err(opencode2api::provider::AdmissionError::DemandTooLarge { .. })
    ));
}

#[test]
fn successful_header_feedback_cools_a_member_without_exposing_secrets() {
    let registry = registry_with_pool();
    let routes = registry.resolve_routes("free-1m").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let now = Instant::now();
    let mut lease = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-ratelimit-remaining-requests", "0".parse().unwrap());
    headers.insert("x-ratelimit-reset-requests", "30".parse().unwrap());
    lease.observe_headers(&headers, now);
    lease.observe_success(None);
    let next = scheduler
        .admit(&routes, CapacityDemand::new(1, 1), &BTreeSet::new(), now)
        .unwrap();
    assert_eq!(
        next.target().credential_id.as_ref().unwrap().as_ref(),
        "key-b"
    );
    let serialized = serde_json::to_string(&scheduler.summary(now)).unwrap();
    assert!(!serialized.contains("DEMO"));
}

#[test]
fn malformed_quota_headers_are_ignored() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-ratelimit-remaining-requests",
        "not-a-number".parse().unwrap(),
    );
    headers.insert(
        "x-ratelimit-reset-requests",
        "also-invalid".parse().unwrap(),
    );
    let feedback = opencode2api::provider::capacity::parse_rate_limit_feedback(&headers);
    assert_eq!(feedback, Default::default());
}

#[test]
fn successful_usage_is_aggregated_without_key_labels() {
    let registry = registry_with_pool();
    let routes = registry.resolve_routes("free-1m").unwrap();
    let scheduler = CapacityScheduler::from_registry(&registry);
    let mut lease = scheduler
        .admit(
            &routes,
            CapacityDemand::new(1, 1),
            &BTreeSet::new(),
            Instant::now(),
        )
        .unwrap();
    lease.observe_success(Some(opencode2api::provider::capacity::TokenUsage {
        input_tokens: 123,
        output_tokens: 45,
    }));
    let summary = scheduler.summary(Instant::now());
    assert_eq!(summary.observed_input_tokens, 123);
    assert_eq!(summary.observed_output_tokens, 45);
    let serialized = serde_json::to_string(&summary).unwrap();
    assert!(!serialized.contains("key-a"));
}
