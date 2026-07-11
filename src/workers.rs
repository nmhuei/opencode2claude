//! Owned background-task registry with cancellation, health, and bounded shutdown.

use futures_util::FutureExt;
use serde::Serialize;
use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Starting,
    Running,
    Stopped,
    Failed,
    Panicked,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerHealth {
    pub name: String,
    pub critical: bool,
    pub state: WorkerState,
    pub last_heartbeat_unix_secs: u64,
    pub last_failure: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerSnapshot {
    pub accepting_tasks: bool,
    pub active_ephemeral_tasks: usize,
    pub workers: Vec<WorkerHealth>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerShutdownError {
    #[error("worker shutdown exceeded {0:?}")]
    Timeout(Duration),
}

#[derive(Debug)]
struct WorkerRecord {
    critical: bool,
    state: WorkerState,
    last_heartbeat: Instant,
    last_heartbeat_unix_secs: u64,
    last_failure: Option<String>,
}

#[derive(Debug)]
pub struct WorkerRegistry {
    cancellation: CancellationToken,
    tracker: TaskTracker,
    records: Arc<RwLock<BTreeMap<String, WorkerRecord>>>,
    sequence: AtomicU64,
    active_ephemeral: Arc<AtomicUsize>,
}

impl Default for WorkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerRegistry {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            tracker: TaskTracker::new(),
            records: Arc::new(RwLock::new(BTreeMap::new())),
            sequence: AtomicU64::new(1),
            active_ephemeral: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.child_token()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
        self.tracker.close();
    }

    pub fn spawn_critical<F, Fut>(&self, name: impl Into<String>, factory: F)
    where
        F: FnOnce(WorkerContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let name = name.into();
        {
            let mut records = self.records.write().expect("worker records poisoned");
            records.insert(
                name.clone(),
                WorkerRecord {
                    critical: true,
                    state: WorkerState::Starting,
                    last_heartbeat: Instant::now(),
                    last_heartbeat_unix_secs: unix_now(),
                    last_failure: None,
                },
            );
        }
        let context = WorkerContext {
            name: name.clone(),
            cancellation: self.cancellation.child_token(),
            records: self.records.clone(),
        };
        let records = self.records.clone();
        self.tracker.spawn(async move {
            context.set_state(WorkerState::Running, None);
            let outcome = AssertUnwindSafe(factory(context.clone()))
                .catch_unwind()
                .await;
            match outcome {
                Ok(Ok(())) => context.set_state(WorkerState::Stopped, None),
                Ok(Err(error)) => context.set_state(WorkerState::Failed, Some(error)),
                Err(payload) => {
                    let message = panic_message(payload);
                    context.set_state(WorkerState::Panicked, Some(message));
                }
            }
            drop(records);
        });
    }

    pub fn spawn_ephemeral<Fut>(&self, label: &str, future: Fut) -> String
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let id = self.sequence.fetch_add(1, Ordering::Relaxed);
        let name = format!("{label}-{id}");
        let active = self.active_ephemeral.clone();
        active.fetch_add(1, Ordering::AcqRel);
        self.tracker.spawn(async move {
            let _guard = EphemeralGuard(active);
            let _ = AssertUnwindSafe(future).catch_unwind().await;
        });
        name
    }

    pub fn snapshot(&self) -> WorkerSnapshot {
        let records = self.records.read().expect("worker records poisoned");
        WorkerSnapshot {
            accepting_tasks: !self.tracker.is_closed(),
            active_ephemeral_tasks: self.active_ephemeral.load(Ordering::Acquire),
            workers: records
                .iter()
                .map(|(name, record)| WorkerHealth {
                    name: name.clone(),
                    critical: record.critical,
                    state: record.state,
                    last_heartbeat_unix_secs: record.last_heartbeat_unix_secs,
                    last_failure: record.last_failure.clone(),
                })
                .collect(),
        }
    }

    pub fn critical_ready(&self, max_heartbeat_age: Duration) -> bool {
        let now = Instant::now();
        self.records
            .read()
            .expect("worker records poisoned")
            .values()
            .filter(|record| record.critical)
            .all(|record| {
                record.state == WorkerState::Running
                    && now.saturating_duration_since(record.last_heartbeat) <= max_heartbeat_age
            })
    }

    pub async fn shutdown(&self, timeout: Duration) -> Result<(), WorkerShutdownError> {
        self.cancel();
        tokio::time::timeout(timeout, self.tracker.wait())
            .await
            .map_err(|_| WorkerShutdownError::Timeout(timeout))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WorkerContext {
    name: String,
    cancellation: CancellationToken,
    records: Arc<RwLock<BTreeMap<String, WorkerRecord>>>,
}

impl WorkerContext {
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn heartbeat(&self) {
        let mut records = self.records.write().expect("worker records poisoned");
        if let Some(record) = records.get_mut(&self.name) {
            record.last_heartbeat = Instant::now();
            record.last_heartbeat_unix_secs = unix_now();
            if record.state == WorkerState::Starting {
                record.state = WorkerState::Running;
            }
        }
    }

    fn set_state(&self, state: WorkerState, failure: Option<String>) {
        let mut records = self.records.write().expect("worker records poisoned");
        if let Some(record) = records.get_mut(&self.name) {
            record.state = state;
            record.last_heartbeat = Instant::now();
            record.last_heartbeat_unix_secs = unix_now();
            record.last_failure = failure;
        }
    }
}

struct EphemeralGuard(Arc<AtomicUsize>);

impl Drop for EphemeralGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "worker panicked with non-string payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cancellation_stops_all_tracked_tasks_within_deadline() {
        let registry = WorkerRegistry::new();
        registry.spawn_critical("loop", |context| async move {
            loop {
                tokio::select! {
                    _ = context.cancellation().cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(1)) => context.heartbeat(),
                }
            }
        });
        registry.spawn_ephemeral("request", {
            let token = registry.cancellation_token();
            async move { token.cancelled().await }
        });
        tokio::task::yield_now().await;
        assert_eq!(registry.snapshot().active_ephemeral_tasks, 1);
        registry
            .shutdown(Duration::from_secs(2))
            .await
            .expect("shutdown");
        assert_eq!(registry.snapshot().active_ephemeral_tasks, 0);
    }

    #[tokio::test]
    async fn worker_failure_changes_readiness_and_reports_reason() {
        let registry = WorkerRegistry::new();
        registry.spawn_critical("failure", |_context| async move {
            Err("intentional failure".to_string())
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert!(!registry.critical_ready(Duration::from_secs(30)));
        let snapshot = registry.snapshot();
        let worker = snapshot
            .workers
            .iter()
            .find(|worker| worker.name == "failure")
            .expect("worker");
        assert_eq!(worker.state, WorkerState::Failed);
        assert_eq!(worker.last_failure.as_deref(), Some("intentional failure"));
    }

    #[tokio::test]
    async fn worker_panic_is_captured_not_propagated() {
        let registry = WorkerRegistry::new();
        registry.spawn_critical("panic", |_context| async move {
            panic!("boom");
            #[allow(unreachable_code)]
            Ok(())
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.workers[0].state, WorkerState::Panicked);
        assert!(snapshot.workers[0]
            .last_failure
            .as_deref()
            .is_some_and(|message| message.contains("boom")));
    }
}
