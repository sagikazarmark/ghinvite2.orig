//! Attempt-level ingress timing, outside Restate replay and authoritative state.
//! Only bounded classifications and clocks are emitted; never input or IDs.
use crate::link_authority::{AuthorityError, Result};
use ghinvite_core::admission::{AdmissionReceipt, AdmissionResult};

const MAX_ELAPSED_MS: u64 = 60_000;

pub(crate) struct Observation {
    #[cfg(not(target_arch = "wasm32"))]
    started: std::time::Instant,
    #[cfg(target_arch = "wasm32")]
    started: f64,
    completed: bool,
}
impl Observation {
    pub(crate) fn start() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            started: std::time::Instant::now(),
            #[cfg(target_arch = "wasm32")]
            started: js_sys::Date::now(),
            completed: false,
        }
    }
    pub(crate) fn complete(mut self, result: &Result<AdmissionReceipt>) {
        let outcome = match result {
            Ok(receipt) => match receipt.result {
                AdmissionResult::Accepted { .. } => "accepted",
                AdmissionResult::Rejected { .. } => "refused",
            },
            Err(AuthorityError::Invalid) => "invalid",
            Err(AuthorityError::Missing) => "missing",
            Err(AuthorityError::Conflict) => "conflict",
            Err(AuthorityError::AuthorizationUnavailable) => "authorization_unavailable",
            Err(AuthorityError::Unknown(_)) => "unknown",
        };
        self.emit(outcome);
        self.completed = true;
    }
    fn emit(&self, outcome: &'static str) {
        #[cfg(not(target_arch = "wasm32"))]
        let (elapsed, clock) = (
            u64::try_from(self.started.elapsed().as_millis())
                .ok()
                .filter(|n| *n <= MAX_ELAPSED_MS),
            "native_monotonic",
        );
        #[cfg(not(target_arch = "wasm32"))]
        let occurred_at = Some(chrono::Utc::now().timestamp());
        #[cfg(target_arch = "wasm32")]
        let (elapsed, clock, occurred_at) = {
            let ended = js_sys::Date::now();
            (
                host_elapsed(self.started, ended),
                "worker_host",
                host_epoch(ended),
            )
        };
        if let Some(elapsed_ms) = elapsed {
            tracing::info!(
                event = "admission_rpc_completed",
                outcome,
                clock,
                clock_valid = true,
                elapsed_ms,
                occurred_at
            );
        } else {
            // No zero-duration fiction for backwards/out-of-range host time.
            tracing::warn!(
                event = "admission_rpc_completed",
                outcome,
                clock,
                clock_valid = false,
                occurred_at
            );
        }
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if !self.completed {
            // Future cancellation cannot establish the admission outcome.
            self.emit("interrupted");
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn host_elapsed(start: f64, end: f64) -> Option<u64> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end < start {
        return None;
    }
    let elapsed = end - start;
    if !elapsed.is_finite() || elapsed > MAX_ELAPSED_MS as f64 {
        return None;
    }
    Some(elapsed as u64)
}

#[cfg(any(target_arch = "wasm32", test))]
fn host_epoch(milliseconds: f64) -> Option<i64> {
    // Preserve time uncertainty instead of coercing NaN/infinity into epoch0.
    if !milliseconds.is_finite() || !(0.0..=9_007_199_254_740_991.0).contains(&milliseconds) {
        return None;
    }
    Some((milliseconds / 1000.0) as i64)
}

#[cfg(test)]
#[path = "admission_telemetry/http_tests.rs"]
mod http_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_host_clock_rejects_regression_overflow_and_large_jumps() {
        assert_eq!(host_epoch(f64::NAN), None);
        assert_eq!(host_epoch(f64::INFINITY), None);
        assert_eq!(host_epoch(-1.0), None);
        assert_eq!(host_epoch(9_007_199_254_740_992.0), None);
        assert_eq!(host_epoch(1_700_000_000_000.0), Some(1_700_000_000));
        assert_eq!(host_elapsed(20.0, 19.0), None);
        assert_eq!(host_elapsed(f64::NAN, 20.0), None);
        assert_eq!(host_elapsed(20.0, f64::NAN), None);
        assert_eq!(host_elapsed(20.0, f64::INFINITY), None);
        assert_eq!(host_elapsed(-1.0, 20.0), None);
        assert_eq!(host_elapsed(20.1, 20.0), None);
        assert_eq!(host_elapsed(f64::NEG_INFINITY, f64::INFINITY), None);
        assert_eq!(host_elapsed(0.0, 60_001.0), None);
        assert_eq!(host_elapsed(20.0, 20.0), Some(0));
        assert_eq!(host_elapsed(0.0, 60_000.0), Some(60_000));
    }
}
