//! Load profiles — the per-second target rate for each generator.
//!
//! A "1 op/s" generator emits one operation roughly every second; we don't
//! pace down to the millisecond because the daemon's hot paths absorb bursts.
//! The integration test uses `LoadProfile::low()` so it can finish in 30 s
//! without inflating the test runner's wall-clock.

use std::time::Duration;

/// Per-generator target rates. All fields are operations per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadProfile {
    /// File-system create/modify/delete cycles per second.
    pub fs_ops_per_sec: f64,
    /// Synthetic `ProcessStarted` / `ProcessStopped` events per second.
    pub process_events_per_sec: f64,
    /// `cel_act` gateway intercepts per second.
    pub gateway_calls_per_sec: f64,
    /// Synthetic agent chat messages (memory writes with `ChunkKind::Chat`)
    /// per second.
    pub agent_chats_per_sec: f64,
    /// Pure memory writes per second (separate from chats — these are
    /// `ChunkKind::Observation` to exercise a different code path).
    pub memory_writes_per_sec: f64,
    /// Memory retrieves per second. Each retrieve is timed and contributes
    /// to the p95 latency check.
    pub memory_retrieves_per_sec: f64,
}

impl LoadProfile {
    /// ~1 op/s per generator. Test-suite-friendly load.
    pub const fn low() -> Self {
        Self {
            fs_ops_per_sec: 2.0,
            process_events_per_sec: 1.0,
            gateway_calls_per_sec: 1.0,
            agent_chats_per_sec: 1.0,
            memory_writes_per_sec: 2.0,
            memory_retrieves_per_sec: 2.0,
        }
    }

    /// ~10 ops/s per generator. Default for ad-hoc multi-hour runs.
    pub const fn medium() -> Self {
        Self {
            fs_ops_per_sec: 20.0,
            process_events_per_sec: 10.0,
            gateway_calls_per_sec: 10.0,
            agent_chats_per_sec: 5.0,
            memory_writes_per_sec: 10.0,
            memory_retrieves_per_sec: 10.0,
        }
    }

    /// ~50 ops/s per generator. Worst-case burst we'd expect under load.
    pub const fn high() -> Self {
        Self {
            fs_ops_per_sec: 100.0,
            process_events_per_sec: 50.0,
            gateway_calls_per_sec: 50.0,
            agent_chats_per_sec: 25.0,
            memory_writes_per_sec: 50.0,
            memory_retrieves_per_sec: 50.0,
        }
    }

    /// Convert a "per second" target into a tick interval. Returns `None`
    /// for non-positive rates so callers can skip spawning that generator.
    ///
    /// For high rates (≥ 1 op/s) this rounds up to the nearest millisecond
    /// since tokio's timer resolution is millisecond-granular on the
    /// default current-thread + multi-thread runtimes. Sub-ms rates would
    /// just spin.
    pub fn interval(rate_per_sec: f64) -> Option<Duration> {
        if rate_per_sec <= 0.0 || !rate_per_sec.is_finite() {
            return None;
        }
        let ms = (1000.0 / rate_per_sec).max(1.0);
        // round up so we never violate the rate cap
        Some(Duration::from_millis(ms.ceil() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_is_quieter_than_high() {
        let l = LoadProfile::low();
        let h = LoadProfile::high();
        assert!(l.fs_ops_per_sec < h.fs_ops_per_sec);
        assert!(l.gateway_calls_per_sec < h.gateway_calls_per_sec);
        assert!(l.memory_retrieves_per_sec < h.memory_retrieves_per_sec);
    }

    #[test]
    fn interval_zero_returns_none() {
        assert!(LoadProfile::interval(0.0).is_none());
        assert!(LoadProfile::interval(-1.0).is_none());
        assert!(LoadProfile::interval(f64::NAN).is_none());
    }

    #[test]
    fn interval_one_per_sec_is_one_sec() {
        assert_eq!(LoadProfile::interval(1.0), Some(Duration::from_secs(1)));
    }

    #[test]
    fn interval_high_rate_clamps_to_min() {
        // 10_000/s → 0.1 ms → clamp to 1 ms.
        assert_eq!(
            LoadProfile::interval(10_000.0),
            Some(Duration::from_millis(1))
        );
    }
}
