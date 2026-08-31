use crate::workers::WorkerContext;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn failure_attempt_after_cycle(current: u32, success: bool) -> u32 {
    if success {
        0
    } else {
        current.saturating_add(1)
    }
}

pub(crate) fn recovery_backoff(attempt: u32, max: Duration, jitter_seed: u64) -> Duration {
    let seconds = match attempt {
        0 => 2,
        1 => 5,
        2 => 10,
        3 => 30,
        _ => 60,
    };
    let base = Duration::from_secs(seconds).min(max);
    if jitter_seed == 0 || base.is_zero() {
        return base;
    }
    let max_jitter_ms = (base.as_millis() / 5).min(u128::from(u64::MAX)) as u64;
    if max_jitter_ms == 0 {
        return base;
    }
    let jitter_ms = jitter_seed % (max_jitter_ms + 1);
    base.saturating_add(Duration::from_millis(jitter_ms))
        .min(max)
}

pub(crate) async fn sleep_with_heartbeat(context: &WorkerContext, duration: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        context.heartbeat();
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline.saturating_duration_since(now);
        let slice = remaining.min(Duration::from_secs(5));
        tokio::select! {
            _ = context.cancellation().cancelled() => return true,
            _ = tokio::time::sleep(slice) => {}
        }
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
