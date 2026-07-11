//! Bounded, secret-safe management audit trail.

use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_AUDIT_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuditEvent {
    pub timestamp_secs: u64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub outcome: AuditOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct AuditLog {
    capacity: usize,
    events: Mutex<VecDeque<AuditEvent>>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new(DEFAULT_AUDIT_CAPACITY)
    }
}

impl AuditLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            events: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
        }
    }

    pub fn record(
        &self,
        actor: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
        outcome: AuditOutcome,
        request_id: Option<String>,
        details: BTreeMap<String, String>,
    ) {
        let event = AuditEvent {
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            actor: actor.into(),
            action: action.into(),
            target: target.into(),
            outcome,
            request_id: request_id.filter(|value| !value.is_empty() && value.len() <= 128),
            details,
        };
        tracing::info!(
            audit = true,
            actor = %event.actor,
            action = %event.action,
            target = %event.target,
            outcome = ?event.outcome,
            request_id = event.request_id.as_deref().unwrap_or(""),
            "management audit event"
        );
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while events.len() >= self.capacity {
            events.pop_front();
        }
        events.push_back(event);
    }

    pub fn snapshot(&self, limit: usize) -> Vec<AuditEvent> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let take = limit.clamp(1, self.capacity).min(events.len());
        events.iter().skip(events.len() - take).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_is_bounded_and_contains_only_explicit_metadata() {
        let log = AuditLog::new(2);
        for index in 0..3 {
            log.record(
                "rest",
                "config_apply",
                "configuration",
                AuditOutcome::Success,
                Some(format!("req-{index}")),
                BTreeMap::from([("changed_key_count".to_string(), index.to_string())]),
            );
        }
        let events = log.snapshot(100);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].request_id.as_deref(), Some("req-1"));
        let encoded = serde_json::to_string(&events).unwrap();
        assert!(!encoded.contains("token"));
        assert!(!encoded.contains("content"));
    }
}
