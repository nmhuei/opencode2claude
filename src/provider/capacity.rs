//! Process-local provider credential admission.
//!
//! The scheduler deliberately owns both capacity and transient route health.
//! Admission and reservation are one short mutex transaction; the lock is
//! never held while resolving secrets or performing network I/O.

use super::adapters::FailureClass;
use super::registry::{ProviderRegistry, ProviderSnapshot};
use super::resilience::RouteState;
use super::types::{
    AttemptTarget, AuthScheme, CapacityDemand, CredentialBinding, CredentialId, CredentialPool,
    CredentialPoolId, CredentialPoolMember, PoolStrategy, ProviderId, RouteTarget,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    NoEligibleRoutes,
    Exhausted { retry_after: Option<Duration> },
    DemandTooLarge { requested: u64, limit: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttemptIdentity {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub credential_id: Option<CredentialId>,
    pub candidate_index: usize,
}

impl AttemptIdentity {
    pub fn from_target(target: &AttemptTarget) -> Self {
        Self {
            provider_id: target.provider_id.clone(),
            model_id: target.model_id.clone(),
            credential_id: target.credential_id.clone(),
            candidate_index: target.candidate_index,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CapacityPoolSummary {
    pub id: CredentialPoolId,
    pub provider_id: ProviderId,
    pub strategy: PoolStrategy,
    pub member_count: usize,
    pub active_members: usize,
    pub available_members: usize,
    pub cooling_members: usize,
    pub saturated_members: usize,
    pub in_flight: u32,
    pub quota_scopes: Vec<String>,
    pub quota_enabled: bool,
    pub next_retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapacitySummary {
    pub credential_pools: usize,
    pub active_members: usize,
    pub cooling_members: usize,
    pub saturated_members: usize,
    pub in_flight: u32,
    pub quota_scopes: usize,
    pub queue_depth: usize,
    pub admissions: u64,
    pub rejected: u64,
    pub fallback_due_to_capacity: u64,
    pub estimated_tokens: u64,
    pub observed_input_tokens: u64,
    pub observed_output_tokens: u64,
    pub cooldown_events: u64,
    pub route_cooling_down: usize,
    pub credential_cooldowns: usize,
    pub provider_cooldowns: usize,
    pub model_cooldowns: usize,
    pub pools: Vec<CapacityPoolSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimitFeedback {
    pub remaining_requests: Option<u64>,
    pub remaining_tokens: Option<u64>,
    pub reset_after: Option<Duration>,
}

/// Hooks used by response owners to report terminal upstream observations
/// without coupling the scheduler to a particular response protocol.
pub trait CapacityObserver {
    fn observe_headers(&mut self, headers: &reqwest::header::HeaderMap, now: Instant);
    fn observe_success(&mut self, usage: Option<TokenUsage>);
    fn fail(&mut self, failure: FailureClass, retry_after: Option<Duration>, now: Instant);
}

/// Parse common OpenAI-compatible quota headers. Providers disagree on the
/// exact header set, so malformed or unknown values are intentionally ignored.
pub fn parse_rate_limit_feedback(headers: &reqwest::header::HeaderMap) -> RateLimitFeedback {
    let remaining_requests = header_u64(
        headers,
        &[
            "x-ratelimit-remaining-requests",
            "x-ratelimit-remaining-request",
        ],
    );
    let remaining_tokens = header_u64(
        headers,
        &[
            "x-ratelimit-remaining-tokens",
            "x-ratelimit-remaining-token",
        ],
    );
    let reset_after = [
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
        "retry-after",
    ]
    .iter()
    .filter_map(|name| headers.get(*name))
    .filter_map(|value| value.to_str().ok())
    .filter_map(parse_reset_duration)
    .min();
    RateLimitFeedback {
        remaining_requests,
        remaining_tokens,
        reset_after,
    }
}

#[derive(Debug)]
pub struct CapacityScheduler {
    state: Mutex<SchedulerState>,
}

#[derive(Debug)]
struct SchedulerState {
    pools: BTreeMap<CredentialPoolId, PoolRuntime>,
    scopes: BTreeMap<String, QuotaScopeRuntime>,
    scope_cooldowns: BTreeMap<String, Instant>,
    direct_in_flight: BTreeMap<CredentialId, u32>,
    reservations: BTreeMap<u64, Reservation>,
    route_state: RouteState,
    credentials: Arc<BTreeMap<CredentialId, super::types::Credential>>,
    next_reservation_id: u64,
    admissions: u64,
    rejected: u64,
    fallback_due_to_capacity: u64,
    estimated_tokens: u64,
    observed_input_tokens: u64,
    observed_output_tokens: u64,
    cooldown_events: u64,
}

#[derive(Debug)]
struct PoolRuntime {
    config: CredentialPool,
    members: Vec<MemberRuntime>,
    cursor: usize,
    weighted_cursor: usize,
}

#[derive(Debug)]
struct MemberRuntime {
    config: CredentialPoolMember,
    in_flight: u32,
}

#[derive(Debug)]
struct QuotaScopeRuntime {
    requests_per_minute: Option<u32>,
    tokens_per_minute: Option<u64>,
    max_in_flight: u32,
    in_flight: u32,
    request_tokens: f64,
    token_tokens: f64,
    last_refill: Instant,
}

#[derive(Debug)]
struct Reservation {
    pool_id: Option<CredentialPoolId>,
    member_index: Option<usize>,
    scope: Option<String>,
    direct_credential: Option<CredentialId>,
}

enum ReserveResult {
    Reserved(Reservation),
    Unavailable(Option<Duration>),
    TooLarge(u64),
}

impl CapacityScheduler {
    pub fn from_registry(registry: &ProviderRegistry) -> Arc<Self> {
        let snapshot = registry
            .compile_snapshot()
            .expect("provider registry must be valid before scheduling");
        Self::from_snapshot(&snapshot)
    }

    pub fn from_snapshot(snapshot: &ProviderSnapshot) -> Arc<Self> {
        let mut pools = BTreeMap::new();
        let mut scopes = BTreeMap::new();
        let scope_start = Instant::now();
        for pool in snapshot.pools.values() {
            let members = pool
                .members
                .iter()
                .cloned()
                .map(|config| MemberRuntime {
                    config,
                    in_flight: 0,
                })
                .collect::<Vec<_>>();
            for member in &members {
                insert_scope(&mut scopes, &member.config, scope_start);
            }
            pools.insert(
                pool.id.clone(),
                PoolRuntime {
                    config: pool.clone(),
                    members,
                    cursor: 0,
                    weighted_cursor: 0,
                },
            );
        }
        Arc::new(Self {
            state: Mutex::new(SchedulerState {
                pools,
                scopes,
                scope_cooldowns: BTreeMap::new(),
                direct_in_flight: BTreeMap::new(),
                reservations: BTreeMap::new(),
                route_state: RouteState::default(),
                credentials: Arc::clone(&snapshot.credentials),
                next_reservation_id: 1,
                admissions: 0,
                rejected: 0,
                fallback_due_to_capacity: 0,
                estimated_tokens: 0,
                observed_input_tokens: 0,
                observed_output_tokens: 0,
                cooldown_events: 0,
            }),
        })
    }

    pub fn admit(
        self: &Arc<Self>,
        routes: &[RouteTarget],
        demand: CapacityDemand,
        exclusions: &BTreeSet<AttemptIdentity>,
        now: Instant,
    ) -> Result<DispatchLease, AdmissionError> {
        if routes.is_empty() {
            return Err(AdmissionError::NoEligibleRoutes);
        }
        let mut state = self.state.lock().expect("capacity scheduler lock poisoned");
        let mut earliest_retry: Option<Duration> = None;
        let mut largest_too_small: Option<u64> = None;
        for (route_index, route) in routes.iter().enumerate() {
            match route.binding.clone() {
                CredentialBinding::Anonymous => {
                    let target = make_target(route, None, &state.credentials);
                    if exclusions.contains(&AttemptIdentity::from_target(&target))
                        || !state.route_state.is_eligible(&target, now)
                    {
                        continue;
                    }
                    state.admissions += 1;
                    state.estimated_tokens = state
                        .estimated_tokens
                        .saturating_add(demand.estimated_tokens);
                    if route_index > 0 {
                        state.fallback_due_to_capacity += 1;
                    }
                    return Ok(DispatchLease::new(0, target, Arc::clone(self)));
                }
                CredentialBinding::Direct(credential_id) => {
                    let target =
                        make_target(route, Some(credential_id.clone()), &state.credentials);
                    if exclusions.contains(&AttemptIdentity::from_target(&target))
                        || !state.route_state.is_eligible(&target, now)
                    {
                        continue;
                    }
                    match reserve_direct(&mut state, &credential_id, demand, now) {
                        ReserveResult::Reserved(reservation) => {
                            return Ok(self.finish_admission(
                                &mut state,
                                route_index,
                                target,
                                demand,
                                reservation,
                            ));
                        }
                        ReserveResult::Unavailable(retry) => {
                            update_earliest(&mut earliest_retry, retry)
                        }
                        ReserveResult::TooLarge(limit) => {
                            largest_too_small =
                                Some(largest_too_small.map_or(limit, |old| old.min(limit)))
                        }
                    }
                }
                CredentialBinding::Pool(pool_id) => {
                    let Some(order) = member_order(&state, &pool_id) else {
                        continue;
                    };
                    for member_index in order {
                        let Some(member_config) = state
                            .pools
                            .get(&pool_id)
                            .and_then(|pool| pool.members.get(member_index))
                            .map(|member| member.config.clone())
                        else {
                            continue;
                        };
                        let target = make_target(
                            route,
                            Some(member_config.credential_id.clone()),
                            &state.credentials,
                        );
                        if exclusions.contains(&AttemptIdentity::from_target(&target))
                            || !state.route_state.is_eligible(&target, now)
                        {
                            continue;
                        }
                        match reserve_pool_member(
                            &mut state,
                            &pool_id,
                            member_index,
                            &member_config,
                            demand,
                            now,
                        ) {
                            ReserveResult::Reserved(reservation) => {
                                if let Some(pool) = state.pools.get_mut(&pool_id) {
                                    advance_cursor(pool, member_index);
                                }
                                return Ok(self.finish_admission(
                                    &mut state,
                                    route_index,
                                    target,
                                    demand,
                                    reservation,
                                ));
                            }
                            ReserveResult::Unavailable(retry) => {
                                update_earliest(&mut earliest_retry, retry)
                            }
                            ReserveResult::TooLarge(limit) => {
                                largest_too_small =
                                    Some(largest_too_small.map_or(limit, |old| old.min(limit)))
                            }
                        }
                    }
                }
            }
        }
        state.rejected += 1;
        if let Some(limit) = largest_too_small {
            if demand.estimated_tokens > limit {
                return Err(AdmissionError::DemandTooLarge {
                    requested: demand.estimated_tokens,
                    limit,
                });
            }
        }
        Err(AdmissionError::Exhausted {
            retry_after: earliest_retry,
        })
    }

    fn finish_admission(
        self: &Arc<Self>,
        state: &mut SchedulerState,
        route_index: usize,
        target: AttemptTarget,
        demand: CapacityDemand,
        reservation: Reservation,
    ) -> DispatchLease {
        let reservation_id = state.next_reservation_id;
        state.next_reservation_id = state.next_reservation_id.saturating_add(1);
        state.reservations.insert(reservation_id, reservation);
        state.admissions += 1;
        state.estimated_tokens = state
            .estimated_tokens
            .saturating_add(demand.estimated_tokens);
        if route_index > 0 {
            state.fallback_due_to_capacity += 1;
        }
        DispatchLease::new(reservation_id, target, Arc::clone(self))
    }

    pub fn summary(&self, now: Instant) -> CapacitySummary {
        let state = self.state.lock().expect("capacity scheduler lock poisoned");
        let route_summary = state.route_state.summary(now);
        let mut active_members = 0;
        let mut cooling_members = 0;
        let mut saturated_members = 0;
        let mut in_flight = 0;
        let mut pools = Vec::with_capacity(state.pools.len());
        for pool in state.pools.values() {
            let mut pool_active = 0;
            let mut pool_available = 0;
            let mut pool_cooling = 0;
            let mut pool_saturated = 0;
            let mut pool_in_flight = 0;
            for member in &pool.members {
                pool_in_flight += member.in_flight;
                if member.in_flight > 0 {
                    pool_active += 1;
                }
                if state.route_state.credential_cooling(
                    &pool.config.provider_id,
                    &member.config.credential_id,
                    now,
                ) || scope_retry_after(&state, &member.config.quota_scope, now).is_some()
                {
                    pool_cooling += 1;
                } else if member.in_flight < member.config.max_in_flight.get() {
                    pool_available += 1;
                }
                if member.in_flight >= member.config.max_in_flight.get() {
                    pool_saturated += 1;
                }
            }
            active_members += pool_active;
            cooling_members += pool_cooling;
            let quota_scopes = pool
                .members
                .iter()
                .map(|member| member.config.quota_scope.clone())
                .collect::<BTreeSet<_>>();
            let next_retry_after_ms = pool
                .members
                .iter()
                .filter_map(|member| {
                    state.route_state.credential_retry_after(
                        &pool.config.provider_id,
                        &member.config.credential_id,
                        now,
                    )
                })
                .map(|value| value.as_millis().min(u64::MAX as u128) as u64)
                .min();
            let quota_enabled = quota_scopes.iter().any(|scope| {
                state.scopes.get(scope).is_some_and(|scope| {
                    scope.requests_per_minute.is_some() || scope.tokens_per_minute.is_some()
                })
            });
            saturated_members += pool_saturated;
            in_flight += pool_in_flight;
            pools.push(CapacityPoolSummary {
                id: pool.config.id.clone(),
                provider_id: pool.config.provider_id.clone(),
                strategy: pool.config.strategy,
                member_count: pool.members.len(),
                active_members: pool_active,
                available_members: pool_available,
                cooling_members: pool_cooling,
                saturated_members: pool_saturated,
                in_flight: pool_in_flight,
                quota_scopes: quota_scopes.into_iter().collect(),
                quota_enabled,
                next_retry_after_ms,
            });
        }
        in_flight += state.direct_in_flight.values().copied().sum::<u32>();
        CapacitySummary {
            credential_pools: state.pools.len(),
            active_members,
            cooling_members,
            saturated_members,
            in_flight,
            quota_scopes: state.scopes.len(),
            queue_depth: 0,
            admissions: state.admissions,
            rejected: state.rejected,
            fallback_due_to_capacity: state.fallback_due_to_capacity,
            estimated_tokens: state.estimated_tokens,
            observed_input_tokens: state.observed_input_tokens,
            observed_output_tokens: state.observed_output_tokens,
            cooldown_events: state.cooldown_events,
            route_cooling_down: route_summary.cooling_down,
            credential_cooldowns: route_summary.credential_cooldowns,
            provider_cooldowns: route_summary.provider_cooldowns,
            model_cooldowns: route_summary.model_cooldowns,
            pools,
        }
    }

    fn release(&self, reservation_id: u64) {
        if reservation_id == 0 {
            return;
        }
        let mut state = self.state.lock().expect("capacity scheduler lock poisoned");
        release_locked(&mut state, reservation_id);
    }
}

pub struct DispatchLease {
    reservation_id: u64,
    target: AttemptTarget,
    scheduler: Arc<CapacityScheduler>,
    completed: bool,
    header_cooldown: bool,
}

impl std::fmt::Debug for DispatchLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchLease")
            .field("reservation_id", &self.reservation_id)
            .field("target", &self.target)
            .field("completed", &self.completed)
            .finish()
    }
}

impl DispatchLease {
    fn new(reservation_id: u64, target: AttemptTarget, scheduler: Arc<CapacityScheduler>) -> Self {
        Self {
            reservation_id,
            target,
            scheduler,
            completed: false,
            header_cooldown: false,
        }
    }

    pub fn target(&self) -> &AttemptTarget {
        &self.target
    }

    pub fn fail(&mut self, failure: FailureClass, retry_after: Option<Duration>, now: Instant) {
        if self.completed {
            return;
        }
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("capacity scheduler lock poisoned");
        let cooldown = retry_after.or_else(|| match failure {
            FailureClass::RateLimit | FailureClass::CredentialRejected => {
                Some(Duration::from_secs(30))
            }
            FailureClass::ModelUnavailable => Some(Duration::from_secs(300)),
            FailureClass::ProviderServer | FailureClass::Transport => Some(Duration::from_secs(1)),
            FailureClass::PaymentRequired => Some(Duration::from_secs(300)),
            FailureClass::ClientRequest | FailureClass::Unknown => None,
        });
        let scope_name = state
            .reservations
            .get(&self.reservation_id)
            .and_then(|reservation| reservation.scope.clone());
        state
            .route_state
            .record_failure(&self.target, failure, retry_after, now);
        if failure == FailureClass::RateLimit {
            if let Some(scope_name) = scope_name.as_deref() {
                if let Some(cooldown) = cooldown {
                    record_scope_cooldown(&mut state, scope_name, cooldown, now);
                }
            }
        }
        if cooldown.is_some_and(|value| !value.is_zero()) {
            state.cooldown_events = state.cooldown_events.saturating_add(1);
        }
        release_locked(&mut state, self.reservation_id);
        self.completed = true;
    }

    pub fn observe_headers(&mut self, headers: &reqwest::header::HeaderMap, now: Instant) {
        if self.completed {
            return;
        }
        let feedback = parse_rate_limit_feedback(headers);
        let exhausted =
            feedback.remaining_requests == Some(0) || feedback.remaining_tokens == Some(0);
        if !exhausted {
            return;
        }
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("capacity scheduler lock poisoned");
        let scope_name = state
            .reservations
            .get(&self.reservation_id)
            .and_then(|reservation| reservation.scope.clone());
        if let Some(scope_name) = scope_name.as_deref() {
            record_scope_cooldown(
                &mut state,
                scope_name,
                feedback.reset_after.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN),
                now,
            );
        }
        state.route_state.record_failure(
            &self.target,
            FailureClass::RateLimit,
            feedback.reset_after,
            now,
        );
        state.cooldown_events = state.cooldown_events.saturating_add(1);
        self.header_cooldown = true;
    }

    pub fn observe_success(&mut self, usage: Option<TokenUsage>) {
        if self.completed {
            return;
        }
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("capacity scheduler lock poisoned");
        if !self.header_cooldown {
            state.route_state.record_success(&self.target);
        }
        if let Some(usage) = usage {
            state.observed_input_tokens = state
                .observed_input_tokens
                .saturating_add(usage.input_tokens);
            state.observed_output_tokens = state
                .observed_output_tokens
                .saturating_add(usage.output_tokens);
        }
        release_locked(&mut state, self.reservation_id);
        self.completed = true;
    }
}

impl CapacityObserver for DispatchLease {
    fn observe_headers(&mut self, headers: &reqwest::header::HeaderMap, now: Instant) {
        DispatchLease::observe_headers(self, headers, now);
    }

    fn observe_success(&mut self, usage: Option<TokenUsage>) {
        DispatchLease::observe_success(self, usage);
    }

    fn fail(&mut self, failure: FailureClass, retry_after: Option<Duration>, now: Instant) {
        DispatchLease::fail(self, failure, retry_after, now);
    }
}

impl Drop for DispatchLease {
    fn drop(&mut self) {
        if !self.completed {
            self.scheduler.release(self.reservation_id);
            self.completed = true;
        }
    }
}

impl QuotaScopeRuntime {
    fn new(member: &CredentialPoolMember, now: Instant) -> Self {
        let requests_per_minute = member.requests_per_minute.map(|value| value.get());
        let tokens_per_minute = member.tokens_per_minute.map(|value| value.get());
        Self {
            requests_per_minute,
            tokens_per_minute,
            max_in_flight: member.max_in_flight.get(),
            in_flight: 0,
            request_tokens: requests_per_minute.map(f64::from).unwrap_or(0.0),
            token_tokens: tokens_per_minute.map(|value| value as f64).unwrap_or(0.0),
            last_refill: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        if elapsed == 0.0 {
            return;
        }
        if let Some(limit) = self.requests_per_minute {
            self.request_tokens =
                (self.request_tokens + elapsed * f64::from(limit) / 60.0).min(f64::from(limit));
        }
        if let Some(limit) = self.tokens_per_minute {
            self.token_tokens =
                (self.token_tokens + elapsed * limit as f64 / 60.0).min(limit as f64);
        }
        self.last_refill = now;
    }

    fn retry_after(&self, demand: CapacityDemand) -> Option<Duration> {
        let mut retry = None;
        if let Some(limit) = self.requests_per_minute {
            if self.request_tokens < 1.0 {
                retry = Some(refill_delay(1.0 - self.request_tokens, f64::from(limit)));
            }
        }
        if let Some(limit) = self.tokens_per_minute {
            if self.token_tokens < demand.estimated_tokens as f64 {
                let delay = refill_delay(
                    demand.estimated_tokens as f64 - self.token_tokens,
                    limit as f64,
                );
                retry = Some(retry.map_or(delay, |old| old.max(delay)));
            }
        }
        retry
    }

    fn can_consume(&self, demand: CapacityDemand) -> Result<(), ReserveResult> {
        if let Some(limit) = self.tokens_per_minute {
            if demand.estimated_tokens > limit {
                return Err(ReserveResult::TooLarge(limit));
            }
        }
        if self
            .requests_per_minute
            .is_some_and(|_| self.request_tokens < 1.0)
            || self
                .tokens_per_minute
                .is_some_and(|_| self.token_tokens < demand.estimated_tokens as f64)
        {
            return Err(ReserveResult::Unavailable(self.retry_after(demand)));
        }
        Ok(())
    }

    fn consume(&mut self, demand: CapacityDemand) {
        if self.requests_per_minute.is_some() {
            self.request_tokens -= 1.0;
        }
        if self.tokens_per_minute.is_some() {
            self.token_tokens -= demand.estimated_tokens as f64;
        }
    }
}

fn insert_scope(
    scopes: &mut BTreeMap<String, QuotaScopeRuntime>,
    member: &CredentialPoolMember,
    now: Instant,
) {
    if let Some(scope) = scopes.get_mut(&member.quota_scope) {
        scope.max_in_flight = scope.max_in_flight.min(member.max_in_flight.get());
        return;
    }
    scopes.insert(
        member.quota_scope.clone(),
        QuotaScopeRuntime::new(member, now),
    );
}

fn member_order(state: &SchedulerState, pool_id: &CredentialPoolId) -> Option<Vec<usize>> {
    let pool = state.pools.get(pool_id)?;
    if pool.members.is_empty() {
        return None;
    }
    match pool.config.strategy {
        PoolStrategy::RoundRobin => Some(
            (0..pool.members.len())
                .map(|offset| (pool.cursor + offset) % pool.members.len())
                .collect(),
        ),
        PoolStrategy::LeastLoaded => {
            let mut indices = (0..pool.members.len()).collect::<Vec<_>>();
            indices.sort_by(|left, right| {
                let left_member = &pool.members[*left];
                let right_member = &pool.members[*right];
                let left_load = u64::from(left_member.in_flight)
                    * u64::from(right_member.config.max_in_flight.get());
                let right_load = u64::from(right_member.in_flight)
                    * u64::from(left_member.config.max_in_flight.get());
                let left_distance = (*left + pool.members.len() - pool.cursor) % pool.members.len();
                let right_distance =
                    (*right + pool.members.len() - pool.cursor) % pool.members.len();
                left_load
                    .cmp(&right_load)
                    .then_with(|| left_distance.cmp(&right_distance))
                    .then_with(|| left.cmp(right))
            });
            Some(indices)
        }
        PoolStrategy::WeightedRoundRobin => {
            let total: usize = pool
                .members
                .iter()
                .map(|member| member.config.weight.get() as usize)
                .sum();
            if total == 0 {
                return None;
            }
            let start = pool.weighted_cursor % total;
            let mut order = Vec::with_capacity(pool.members.len());
            let mut seen = BTreeSet::new();
            for step in 0..total {
                let position = (start + step) % total;
                let mut offset = position;
                let mut selected = 0;
                for (index, member) in pool.members.iter().enumerate() {
                    let weight = member.config.weight.get() as usize;
                    if offset < weight {
                        selected = index;
                        break;
                    }
                    offset -= weight;
                }
                if seen.insert(selected) {
                    order.push(selected);
                }
            }
            Some(order)
        }
    }
}

fn advance_cursor(pool: &mut PoolRuntime, selected: usize) {
    match pool.config.strategy {
        PoolStrategy::RoundRobin | PoolStrategy::LeastLoaded => {
            pool.cursor = (selected + 1) % pool.members.len();
        }
        PoolStrategy::WeightedRoundRobin => {
            let total: usize = pool
                .members
                .iter()
                .map(|member| member.config.weight.get() as usize)
                .sum();
            if total == 0 {
                return;
            }
            let start = pool.weighted_cursor % total;
            let mut position = start;
            for _ in 0..total {
                let mut offset = position;
                let mut found = 0;
                for (index, member) in pool.members.iter().enumerate() {
                    let weight = member.config.weight.get() as usize;
                    if offset < weight {
                        found = index;
                        break;
                    }
                    offset -= weight;
                }
                if found == selected {
                    pool.weighted_cursor = (position + 1) % total;
                    return;
                }
                position = (position + 1) % total;
            }
        }
    }
}

fn reserve_direct(
    state: &mut SchedulerState,
    credential_id: &CredentialId,
    demand: CapacityDemand,
    now: Instant,
) -> ReserveResult {
    let scope_name = format!("credential:{credential_id}");
    if let Some(retry_after) = scope_retry_after(state, &scope_name, now) {
        return ReserveResult::Unavailable(Some(retry_after));
    }
    let scope = state.scopes.entry(scope_name.clone()).or_insert_with(|| {
        CredentialPoolMember::new(credential_id.clone(), scope_name.clone(), 1, None, None, 1)
            .map(|member| QuotaScopeRuntime::new(&member, now))
            .expect("implicit direct pool member must be valid")
    });
    scope.refill(now);
    if scope.in_flight >= scope.max_in_flight {
        return ReserveResult::Unavailable(None);
    }
    if let Err(error) = scope.can_consume(demand) {
        return error;
    }
    scope.consume(demand);
    *state
        .direct_in_flight
        .entry(credential_id.clone())
        .or_default() += 1;
    scope.in_flight += 1;
    ReserveResult::Reserved(Reservation {
        pool_id: None,
        member_index: None,
        scope: Some(scope_name),
        direct_credential: Some(credential_id.clone()),
    })
}

fn reserve_pool_member(
    state: &mut SchedulerState,
    pool_id: &CredentialPoolId,
    member_index: usize,
    member: &CredentialPoolMember,
    demand: CapacityDemand,
    now: Instant,
) -> ReserveResult {
    let member_in_flight = state
        .pools
        .get(pool_id)
        .and_then(|pool| pool.members.get(member_index))
        .map(|member| member.in_flight)
        .unwrap_or(member.max_in_flight.get());
    if member_in_flight >= member.max_in_flight.get() {
        return ReserveResult::Unavailable(None);
    }
    if let Some(retry_after) = scope_retry_after(state, &member.quota_scope, now) {
        return ReserveResult::Unavailable(Some(retry_after));
    }
    let scope = state
        .scopes
        .entry(member.quota_scope.clone())
        .or_insert_with(|| QuotaScopeRuntime::new(member, now));
    scope.refill(now);
    if scope.in_flight >= scope.max_in_flight {
        return ReserveResult::Unavailable(None);
    }
    if let Err(error) = scope.can_consume(demand) {
        return error;
    }
    scope.consume(demand);
    scope.in_flight += 1;
    if let Some(pool) = state.pools.get_mut(pool_id) {
        pool.members[member_index].in_flight += 1;
    }
    ReserveResult::Reserved(Reservation {
        pool_id: Some(pool_id.clone()),
        member_index: Some(member_index),
        scope: Some(member.quota_scope.clone()),
        direct_credential: None,
    })
}

fn release_locked(state: &mut SchedulerState, reservation_id: u64) {
    let Some(reservation) = state.reservations.remove(&reservation_id) else {
        return;
    };
    if let (Some(pool_id), Some(member_index)) = (reservation.pool_id, reservation.member_index) {
        if let Some(pool) = state.pools.get_mut(&pool_id) {
            if let Some(member) = pool.members.get_mut(member_index) {
                member.in_flight = member.in_flight.saturating_sub(1);
            }
        }
    }
    if let Some(credential) = reservation.direct_credential {
        if let Some(in_flight) = state.direct_in_flight.get_mut(&credential) {
            *in_flight = in_flight.saturating_sub(1);
            if *in_flight == 0 {
                state.direct_in_flight.remove(&credential);
            }
        }
    }
    if let Some(scope_name) = reservation.scope {
        if let Some(scope) = state.scopes.get_mut(&scope_name) {
            scope.in_flight = scope.in_flight.saturating_sub(1);
        }
    }
}

fn record_scope_cooldown(
    state: &mut SchedulerState,
    scope_name: &str,
    cooldown: Duration,
    now: Instant,
) {
    let Some(until) = now.checked_add(cooldown) else {
        return;
    };
    if until <= now {
        return;
    }
    state
        .scope_cooldowns
        .entry(scope_name.to_string())
        .and_modify(|current| *current = (*current).max(until))
        .or_insert(until);
}

fn scope_retry_after(state: &SchedulerState, scope_name: &str, now: Instant) -> Option<Duration> {
    state
        .scope_cooldowns
        .get(scope_name)
        .map(|until| until.saturating_duration_since(now))
        .filter(|remaining| !remaining.is_zero())
}

fn make_target(
    route: &RouteTarget,
    credential_id: Option<CredentialId>,
    credentials: &BTreeMap<CredentialId, super::types::Credential>,
) -> AttemptTarget {
    let auth_scheme = credential_id
        .as_ref()
        .and_then(|id| credentials.get(id))
        .map(|credential| credential.auth_scheme)
        .unwrap_or(AuthScheme::None);
    AttemptTarget {
        alias_id: route.alias_id.clone(),
        client_model: route.client_model.clone(),
        provider_id: route.provider_id.clone(),
        model_id: route.model_id.clone(),
        wire_model_id: route.wire_model_id.clone(),
        credential_id,
        auth_scheme,
        context_window: route.context_window,
        candidate_index: route.candidate_index,
    }
}

fn update_earliest(current: &mut Option<Duration>, next: Option<Duration>) {
    if let Some(next) = next {
        *current = Some(current.map_or(next, |old| old.min(next)));
    }
}

fn refill_delay(missing: f64, per_minute: f64) -> Duration {
    let seconds = (missing * 60.0 / per_minute).ceil().max(1.0) as u64;
    Duration::from_secs(seconds)
}

fn header_u64(headers: &reqwest::header::HeaderMap, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .filter_map(|name| headers.get(*name))
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| value.trim().parse::<u64>().ok())
}

fn parse_reset_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Some(milliseconds) = value.strip_suffix("ms") {
        return milliseconds
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_millis);
    }
    if let Some(seconds) = value.strip_suffix('s') {
        return seconds.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    value.parse::<u64>().ok().map(Duration::from_secs)
}
