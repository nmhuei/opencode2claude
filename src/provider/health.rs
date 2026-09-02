use super::types::ProviderId;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHealthState {
    Unknown,
    Healthy,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct ProviderHealth {
    pub provider_id: ProviderId,
    pub state: ProviderHealthState,
    pub latency: Option<Duration>,
    pub checked_at: SystemTime,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct HealthManager {
    states: BTreeMap<ProviderId, ProviderHealth>,
}

impl HealthManager {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn record(
        &mut self,
        provider_id: impl Into<ProviderId>,
        state: ProviderHealthState,
        latency: Option<Duration>,
        error: Option<String>,
    ) {
        let provider_id = provider_id.into();
        self.states.insert(
            provider_id.clone(),
            ProviderHealth {
                provider_id,
                state,
                latency,
                checked_at: SystemTime::now(),
                error,
            },
        );
    }
    pub fn get(&self, provider_id: impl AsRef<str>) -> Option<&ProviderHealth> {
        self.states.get(&ProviderId::from(provider_id.as_ref()))
    }
    pub fn states(&self) -> impl Iterator<Item = &ProviderHealth> {
        self.states.values()
    }
    pub async fn check(
        &mut self,
        client: &reqwest::Client,
        provider: &super::types::Provider,
    ) -> ProviderHealth {
        let started = std::time::Instant::now();
        let response = client
            .get(provider.base_url.trim_end_matches('/'))
            .send()
            .await;
        let (state, error) = match response {
            Ok(response)
                if response.status().is_success() || response.status().is_client_error() =>
            {
                (ProviderHealthState::Healthy, None)
            }
            Ok(response) => (
                ProviderHealthState::Degraded,
                Some(format!("HTTP {}", response.status())),
            ),
            Err(error) => (ProviderHealthState::Unavailable, Some(error.to_string())),
        };
        let latency = Some(started.elapsed());
        self.record(provider.id.clone(), state, latency, error);
        self.get(&provider.id)
            .cloned()
            .expect("health record was inserted")
    }
}
