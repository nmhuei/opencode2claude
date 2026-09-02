//! Transient route health and deterministic fallback policy.

use super::adapters::FailureClass;
use super::types::AttemptTarget;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(30);
const DEFAULT_PROVIDER_COOLDOWN: Duration = Duration::from_secs(1);
const DEFAULT_MODEL_COOLDOWN: Duration = Duration::from_secs(300);
const DEFAULT_BILLING_COOLDOWN: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAction {
    UseTarget(usize),
    AllCoolingDown { retry_after: Duration },
    NoEligibleTargets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteStateSummary {
    pub cooling_down: usize,
    pub credential_cooldowns: usize,
    pub provider_cooldowns: usize,
    pub model_cooldowns: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CredentialKey {
    provider: String,
    id_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelKey {
    provider: String,
    model: String,
}

#[derive(Debug, Clone, Default)]
pub struct RouteState {
    credential_cooldowns: BTreeMap<CredentialKey, Instant>,
    provider_cooldowns: BTreeMap<String, Instant>,
    model_cooldowns: BTreeMap<ModelKey, Instant>,
}

impl RouteState {
    pub fn summary(&self, now: Instant) -> RouteStateSummary {
        let credential_cooldowns = self
            .credential_cooldowns
            .values()
            .filter(|until| **until > now)
            .count();
        let provider_cooldowns = self
            .provider_cooldowns
            .values()
            .filter(|until| **until > now)
            .count();
        let model_cooldowns = self
            .model_cooldowns
            .values()
            .filter(|until| **until > now)
            .count();
        RouteStateSummary {
            cooling_down: credential_cooldowns + provider_cooldowns + model_cooldowns,
            credential_cooldowns,
            provider_cooldowns,
            model_cooldowns,
        }
    }

    pub fn choose(&self, targets: &[AttemptTarget], now: Instant) -> RouteAction {
        if targets.is_empty() {
            return RouteAction::NoEligibleTargets;
        }
        let mut earliest: Option<Duration> = None;
        for (index, target) in targets.iter().enumerate() {
            if self.is_eligible(target, now) {
                return RouteAction::UseTarget(index);
            }
            if let Some(until) = self.cooldown_until(target) {
                let remaining = until.saturating_duration_since(now);
                earliest = Some(earliest.map_or(remaining, |value| value.min(remaining)));
            }
        }
        earliest.map_or(RouteAction::NoEligibleTargets, |retry_after| {
            RouteAction::AllCoolingDown { retry_after }
        })
    }

    pub fn record_failure(
        &mut self,
        target: &AttemptTarget,
        failure: FailureClass,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        let duration = retry_after.unwrap_or(match failure {
            FailureClass::RateLimit | FailureClass::CredentialRejected => {
                DEFAULT_RATE_LIMIT_COOLDOWN
            }
            FailureClass::ModelUnavailable => DEFAULT_MODEL_COOLDOWN,
            FailureClass::ProviderServer | FailureClass::Transport => DEFAULT_PROVIDER_COOLDOWN,
            FailureClass::PaymentRequired => DEFAULT_BILLING_COOLDOWN,
            FailureClass::ClientRequest | FailureClass::Unknown => Duration::ZERO,
        });
        if duration.is_zero() {
            return;
        }
        let until = now + duration;
        match failure {
            FailureClass::CredentialRejected | FailureClass::RateLimit => {
                self.credential_cooldowns
                    .insert(credential_key(target), until);
            }
            FailureClass::ModelUnavailable => {
                self.model_cooldowns.insert(model_key(target), until);
            }
            FailureClass::ProviderServer | FailureClass::Transport => {
                self.provider_cooldowns
                    .insert(target.provider_id.to_string(), until);
            }
            FailureClass::PaymentRequired => {
                self.provider_cooldowns
                    .insert(target.provider_id.to_string(), until);
            }
            FailureClass::ClientRequest | FailureClass::Unknown => {}
        }
    }

    pub fn record_success(&mut self, target: &AttemptTarget) {
        self.credential_cooldowns.remove(&credential_key(target));
        self.model_cooldowns.remove(&model_key(target));
        self.provider_cooldowns.remove(target.provider_id.as_ref());
    }

    /// Drop transient state after a validated registry replacement. Cooldowns
    /// are keyed by provider/model/credential identifiers from the old
    /// snapshot and must not survive a configuration reload as stale health.
    pub fn clear(&mut self) {
        self.credential_cooldowns.clear();
        self.provider_cooldowns.clear();
        self.model_cooldowns.clear();
    }

    pub fn is_eligible(&self, target: &AttemptTarget, now: Instant) -> bool {
        self.cooldown_until(target).is_none_or(|until| until <= now)
    }

    fn cooldown_until(&self, target: &AttemptTarget) -> Option<Instant> {
        [
            self.credential_cooldowns
                .get(&credential_key(target))
                .copied(),
            self.provider_cooldowns
                .get(target.provider_id.as_ref())
                .copied(),
            self.model_cooldowns.get(&model_key(target)).copied(),
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

fn credential_key(target: &AttemptTarget) -> CredentialKey {
    let value = target
        .credential_id
        .as_ref()
        .map(|id| id.as_ref())
        .unwrap_or("anonymous");
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    CredentialKey {
        provider: target.provider_id.to_string(),
        id_hash: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

fn model_key(target: &AttemptTarget) -> ModelKey {
    ModelKey {
        provider: target.provider_id.to_string(),
        model: target.model_id.clone(),
    }
}
