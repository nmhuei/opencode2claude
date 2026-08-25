use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SUBSYSTEM_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxySubsystemPhase {
    Disabled,
    Starting,
    TransportVerifying,
    IdentityVerifying,
    RouteVerifying,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProxySubsystemSnapshot {
    pub phase: ProxySubsystemPhase,
    pub ready: bool,
    pub last_transition_unix_secs: u64,
    pub last_success_unix_secs: Option<u64>,
    pub last_error: Option<String>,
    pub backoff_until_unix_secs: Option<u64>,
}

#[derive(Debug)]
pub struct ProxySubsystemStatus {
    phase: ProxySubsystemPhase,
    last_transition_unix_secs: u64,
    last_success_unix_secs: Option<u64>,
    last_error: Option<String>,
    backoff_until_unix_secs: Option<u64>,
}

impl ProxySubsystemStatus {
    pub fn disabled() -> Self {
        Self::new(ProxySubsystemPhase::Disabled)
    }

    pub fn starting() -> Self {
        Self::new(ProxySubsystemPhase::Starting)
    }

    fn new(phase: ProxySubsystemPhase) -> Self {
        Self {
            phase,
            last_transition_unix_secs: now_unix_secs(),
            last_success_unix_secs: None,
            last_error: None,
            backoff_until_unix_secs: None,
        }
    }

    pub fn transition(&mut self, phase: ProxySubsystemPhase, error: Option<String>) {
        self.phase = phase;
        self.last_transition_unix_secs = now_unix_secs();
        self.last_error = error.map(|value| bounded_error(&value));
        if phase != ProxySubsystemPhase::Degraded {
            self.backoff_until_unix_secs = None;
        }
    }

    pub fn mark_ready(&mut self) {
        let now = now_unix_secs();
        self.phase = ProxySubsystemPhase::Ready;
        self.last_transition_unix_secs = now;
        self.last_success_unix_secs = Some(now);
        self.last_error = None;
        self.backoff_until_unix_secs = None;
    }

    pub fn mark_degraded(&mut self, error: impl Into<String>, backoff_until: Option<u64>) {
        self.phase = ProxySubsystemPhase::Degraded;
        self.last_transition_unix_secs = now_unix_secs();
        self.last_error = Some(bounded_error(&error.into()));
        self.backoff_until_unix_secs = backoff_until;
    }

    pub fn is_ready(&self) -> bool {
        self.phase == ProxySubsystemPhase::Ready
    }

    pub fn snapshot(&self) -> ProxySubsystemSnapshot {
        ProxySubsystemSnapshot {
            phase: self.phase,
            ready: self.is_ready(),
            last_transition_unix_secs: self.last_transition_unix_secs,
            last_success_unix_secs: self.last_success_unix_secs,
            last_error: self.last_error.clone(),
            backoff_until_unix_secs: self.backoff_until_unix_secs,
        }
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn bounded_error(value: &str) -> String {
    if value.len() <= MAX_SUBSYSTEM_ERROR_BYTES {
        return value.to_string();
    }
    let mut end = MAX_SUBSYSTEM_ERROR_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsystem_only_reports_ready_in_ready_phase() {
        let mut state = ProxySubsystemStatus::starting();
        assert!(!state.is_ready());
        state.mark_ready();
        assert!(state.is_ready());
        assert_eq!(state.snapshot().phase, ProxySubsystemPhase::Ready);
    }

    #[test]
    fn degraded_state_records_bounded_secret_safe_error() {
        let mut state = ProxySubsystemStatus::starting();
        state.mark_degraded("x".repeat(2048), Some(123));
        let snap = state.snapshot();
        assert_eq!(snap.phase, ProxySubsystemPhase::Degraded);
        assert!(snap.last_error.unwrap().len() <= 512);
        assert_eq!(snap.backoff_until_unix_secs, Some(123));
    }

    #[test]
    fn error_truncation_preserves_utf8_boundaries() {
        let mut state = ProxySubsystemStatus::starting();
        state.mark_degraded("é".repeat(600), None);
        let error = state.snapshot().last_error.unwrap();
        assert!(error.len() <= MAX_SUBSYSTEM_ERROR_BYTES);
        assert!(std::str::from_utf8(error.as_bytes()).is_ok());
    }
}
