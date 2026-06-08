//! Acceptance-gate thresholds. The harness exits non-zero if any sample
//! tripped a threshold.
//!
//! These are the metric-classification helpers the task brief calls out for
//! unit-test coverage — every threshold is a pure function over a
//! [`crate::metrics::MetricSample`] plus the configured limits, so they're
//! easy to test in isolation.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::metrics::MetricSample;

/// The metric-name we track `retrieve` latency under.
pub const RETRIEVE_METHOD: &str = "retrieve";

/// Acceptance limits the harness is held to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Thresholds {
    /// Maximum resident memory in MiB. Exceeded → breach.
    pub max_rss_mib: f64,
    /// Maximum `retrieve` p95 latency in milliseconds. Exceeded → breach.
    /// Default: 200 ms per `cellar-memory-manager.md` §14.4.
    pub max_retrieve_p95_ms: f64,
    /// Maximum daemon error rate per minute. Exceeded → breach.
    /// Default: 0.1/min.
    pub max_error_rate_per_min: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            max_rss_mib: 500.0,
            max_retrieve_p95_ms: 200.0,
            max_error_rate_per_min: 0.1,
        }
    }
}

/// A single breach recorded against a sample.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThresholdBreach {
    /// Which limit was tripped.
    pub kind: ThresholdViolation,
    /// Sample uptime (seconds) when the breach was observed.
    pub at_uptime_s: u64,
    /// Observed value.
    pub observed: f64,
    /// Configured limit.
    pub limit: f64,
}

/// The kind of breach.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdViolation {
    /// `daemon.rss_mib` > `max_rss_mib`.
    RssExceeded,
    /// `retrieve` p95 > `max_retrieve_p95_ms`.
    RetrieveP95Exceeded,
    /// Error rate over the sample window > `max_error_rate_per_min`.
    ErrorRateExceeded,
}

impl Thresholds {
    /// Classify a single sample against the limits. Returns the (possibly
    /// empty) list of breaches.
    ///
    /// `window` is the wall-clock time the sample covers — needed to convert
    /// the window's error count to a per-minute rate. Pass the harness's
    /// sample interval here.
    pub fn classify(&self, sample: &MetricSample, window: Duration) -> Vec<ThresholdBreach> {
        let mut out = Vec::new();
        if sample.rss_mib > self.max_rss_mib {
            out.push(ThresholdBreach {
                kind: ThresholdViolation::RssExceeded,
                at_uptime_s: sample.uptime_s,
                observed: sample.rss_mib,
                limit: self.max_rss_mib,
            });
        }
        if let Some(d) = sample.latencies_ms.get(RETRIEVE_METHOD) {
            if d.p95_ms > self.max_retrieve_p95_ms {
                out.push(ThresholdBreach {
                    kind: ThresholdViolation::RetrieveP95Exceeded,
                    at_uptime_s: sample.uptime_s,
                    observed: d.p95_ms,
                    limit: self.max_retrieve_p95_ms,
                });
            }
        }
        let total_errs: u64 = sample.err_counts.values().sum();
        let rate = error_rate_per_min(total_errs, window);
        if rate > self.max_error_rate_per_min {
            out.push(ThresholdBreach {
                kind: ThresholdViolation::ErrorRateExceeded,
                at_uptime_s: sample.uptime_s,
                observed: rate,
                limit: self.max_error_rate_per_min,
            });
        }
        out
    }
}

/// Convert an error count over a window into a per-minute rate. Pure
/// function — exported for unit tests.
pub fn error_rate_per_min(errors: u64, window: Duration) -> f64 {
    if window.is_zero() {
        return 0.0;
    }
    let mins = window.as_secs_f64() / 60.0;
    if mins <= 0.0 {
        return 0.0;
    }
    errors as f64 / mins
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{DaemonGauge, LatencyDistribution, MemoryGauge, MetricSample};
    use chrono::Utc;
    use std::collections::HashMap;

    fn empty_sample() -> MetricSample {
        MetricSample {
            ts: Utc::now(),
            uptime_s: 60,
            rss_mib: 0.0,
            cpu_pct: 0.0,
            latencies_ms: HashMap::new(),
            ok_counts: HashMap::new(),
            err_counts: HashMap::new(),
            cumulative_errors: 0,
            cumulative_ok: 0,
            memory: MemoryGauge::default(),
            daemon: DaemonGauge::default(),
        }
    }

    #[test]
    fn defaults_match_acceptance_gate() {
        let t = Thresholds::default();
        assert_eq!(t.max_rss_mib, 500.0);
        assert_eq!(t.max_retrieve_p95_ms, 200.0);
        assert!((t.max_error_rate_per_min - 0.1).abs() < 1e-9);
    }

    #[test]
    fn green_sample_has_no_breaches() {
        let mut s = empty_sample();
        s.rss_mib = 100.0;
        s.latencies_ms.insert(
            "retrieve".into(),
            LatencyDistribution {
                count: 10,
                min_ms: 1.0,
                max_ms: 50.0,
                mean_ms: 10.0,
                p50_ms: 8.0,
                p95_ms: 40.0,
                p99_ms: 48.0,
            },
        );
        let t = Thresholds::default();
        let b = t.classify(&s, Duration::from_secs(60));
        assert!(b.is_empty(), "expected no breaches, got {:?}", b);
    }

    #[test]
    fn rss_breach_detected() {
        let mut s = empty_sample();
        s.rss_mib = 600.0;
        let t = Thresholds::default();
        let b = t.classify(&s, Duration::from_secs(60));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, ThresholdViolation::RssExceeded);
        assert_eq!(b[0].observed, 600.0);
        assert_eq!(b[0].limit, 500.0);
    }

    #[test]
    fn retrieve_p95_breach_detected() {
        let mut s = empty_sample();
        s.latencies_ms.insert(
            "retrieve".into(),
            LatencyDistribution {
                count: 100,
                min_ms: 1.0,
                max_ms: 500.0,
                mean_ms: 150.0,
                p50_ms: 50.0,
                p95_ms: 300.0,
                p99_ms: 480.0,
            },
        );
        let t = Thresholds::default();
        let b = t.classify(&s, Duration::from_secs(60));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, ThresholdViolation::RetrieveP95Exceeded);
        assert_eq!(b[0].observed, 300.0);
    }

    #[test]
    fn retrieve_p95_under_budget_is_green() {
        let mut s = empty_sample();
        s.latencies_ms.insert(
            "retrieve".into(),
            LatencyDistribution {
                count: 100,
                min_ms: 1.0,
                max_ms: 199.0,
                mean_ms: 80.0,
                p50_ms: 50.0,
                p95_ms: 199.9,
                p99_ms: 199.95,
            },
        );
        let t = Thresholds::default();
        let b = t.classify(&s, Duration::from_secs(60));
        assert!(b.is_empty());
    }

    #[test]
    fn error_rate_breach_detected() {
        let mut s = empty_sample();
        // 2 errors in a 60 s window → 2/min, > 0.1/min limit.
        s.err_counts.insert("retrieve".into(), 2);
        let t = Thresholds::default();
        let b = t.classify(&s, Duration::from_secs(60));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, ThresholdViolation::ErrorRateExceeded);
        assert!((b[0].observed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn zero_errors_is_zero_rate() {
        assert_eq!(error_rate_per_min(0, Duration::from_secs(60)), 0.0);
    }

    #[test]
    fn zero_window_is_zero_rate() {
        assert_eq!(error_rate_per_min(5, Duration::from_secs(0)), 0.0);
    }

    #[test]
    fn rate_scales_with_window() {
        // 6 errors in 2 minutes → 3/min.
        let r = error_rate_per_min(6, Duration::from_secs(120));
        assert!((r - 3.0).abs() < 1e-9);
    }

    #[test]
    fn multiple_simultaneous_breaches() {
        let mut s = empty_sample();
        s.rss_mib = 1024.0;
        s.latencies_ms.insert(
            "retrieve".into(),
            LatencyDistribution {
                count: 1,
                min_ms: 250.0,
                max_ms: 250.0,
                mean_ms: 250.0,
                p50_ms: 250.0,
                p95_ms: 250.0,
                p99_ms: 250.0,
            },
        );
        s.err_counts.insert("gateway".into(), 10);
        let t = Thresholds::default();
        let b = t.classify(&s, Duration::from_secs(60));
        assert_eq!(b.len(), 3);
        let kinds: Vec<_> = b.iter().map(|x| x.kind).collect();
        assert!(kinds.contains(&ThresholdViolation::RssExceeded));
        assert!(kinds.contains(&ThresholdViolation::RetrieveP95Exceeded));
        assert!(kinds.contains(&ThresholdViolation::ErrorRateExceeded));
    }

    #[test]
    fn custom_thresholds_respected() {
        let mut s = empty_sample();
        s.rss_mib = 100.0;
        let lenient = Thresholds {
            max_rss_mib: 50.0,
            ..Thresholds::default()
        };
        assert_eq!(lenient.classify(&s, Duration::from_secs(60)).len(), 1);
        let generous = Thresholds {
            max_rss_mib: 1000.0,
            ..Thresholds::default()
        };
        assert!(generous.classify(&s, Duration::from_secs(60)).is_empty());
    }
}
