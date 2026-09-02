use opencode2api::provider::adapters::FailureClass;
use opencode2api::provider::resilience::{RouteAction, RouteState};
use opencode2api::provider::types::{AttemptTarget, AuthScheme};
use std::time::{Duration, Instant};

fn target(provider: &str, credential: &str, index: usize) -> AttemptTarget {
    AttemptTarget {
        alias_id: "free-1m".into(),
        client_model: "sonnet[1m]".into(),
        provider_id: provider.into(),
        model_id: format!("{provider}-model"),
        wire_model_id: format!("wire-{provider}"),
        credential_id: Some(credential.into()),
        auth_scheme: AuthScheme::Bearer,
        context_window: 1_000_000,
        candidate_index: index,
    }
}

#[test]
fn rate_limit_rotates_credentials_before_advancing_provider() {
    let now = Instant::now();
    let targets = vec![
        target("bai", "bai-primary", 0),
        target("bai", "bai-secondary", 1),
        target("kilo", "kilo-free", 2),
    ];
    let mut state = RouteState::default();
    state.record_failure(
        &targets[0],
        FailureClass::RateLimit,
        Some(Duration::from_secs(60)),
        now,
    );
    assert_eq!(
        state.choose(&targets, now),
        RouteAction::UseTarget(1),
        "a second credential in the same provider is the first fallback"
    );
    state.record_failure(
        &targets[1],
        FailureClass::RateLimit,
        Some(Duration::from_secs(60)),
        now,
    );
    assert_eq!(state.choose(&targets, now), RouteAction::UseTarget(2));
}

#[test]
fn cooldown_expiry_reenables_the_original_target_without_persisting_secret_data() {
    let now = Instant::now();
    let target = target("bai", "bai-primary", 0);
    let mut state = RouteState::default();
    state.record_failure(
        &target,
        FailureClass::RateLimit,
        Some(Duration::from_secs(5)),
        now,
    );
    assert_eq!(
        state.choose(std::slice::from_ref(&target), now),
        RouteAction::AllCoolingDown {
            retry_after: Duration::from_secs(5)
        }
    );
    assert_eq!(
        state.choose(std::slice::from_ref(&target), now + Duration::from_secs(5)),
        RouteAction::UseTarget(0)
    );
    let debug = format!("{state:?}");
    assert!(!debug.contains("bai-primary"));
}

#[test]
fn billing_failure_quarantines_only_the_provider_and_reload_can_clear_it() {
    let now = Instant::now();
    let targets = vec![target("bai", "primary", 0), target("kilo", "free", 1)];
    let mut state = RouteState::default();
    state.record_failure(&targets[0], FailureClass::PaymentRequired, None, now);
    assert_eq!(state.choose(&targets, now), RouteAction::UseTarget(1));
    state.clear();
    assert_eq!(state.choose(&targets, now), RouteAction::UseTarget(0));
}
