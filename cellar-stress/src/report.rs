//! End-of-run summary report.
//!
//! Written to `<output_dir>/summary.json` and echoed to stdout. Carries
//! everything a human (or CI) needs to know: total samples, peak RSS, p95
//! retrieve latency, error counts, and a flat list of every threshold breach.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::metrics::MetricSample;
use crate::thresholds::ThresholdBreach;

/// Final verdict — the binary returns 0 only if [`Summary::green`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    /// Harness start.
    pub started_at: DateTime<Utc>,
    /// Harness end.
    pub ended_at: DateTime<Utc>,
    /// Total run length (= `ended_at - started_at`).
    pub duration_s: u64,
    /// Number of `MetricSample`s collected.
    pub sample_count: u64,
    /// Peak resident memory in MiB across all samples.
    pub peak_rss_mib: f64,
    /// Mean resident memory in MiB across all samples.
    pub mean_rss_mib: f64,
    /// Worst-case `retrieve` p95 across all samples.
    pub worst_retrieve_p95_ms: f64,
    /// Cumulative success count across all generators.
    pub cumulative_ok: u64,
    /// Cumulative error count across all generators.
    pub cumulative_errors: u64,
    /// All threshold breaches observed during the run. Empty = green.
    pub breaches: Vec<ThresholdBreach>,
    /// True iff `breaches.is_empty()`.
    pub green: bool,
}

impl Summary {
    /// Aggregate a sequence of samples into a final summary plus accumulated
    /// breaches. Pure function over its inputs.
    pub fn aggregate(
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
        samples: &[MetricSample],
        breaches: Vec<ThresholdBreach>,
    ) -> Self {
        let duration_s = (ended_at - started_at).num_seconds().max(0) as u64;
        let peak_rss_mib = samples.iter().map(|s| s.rss_mib).fold(0.0_f64, f64::max);
        let mean_rss_mib = if samples.is_empty() {
            0.0
        } else {
            samples.iter().map(|s| s.rss_mib).sum::<f64>() / samples.len() as f64
        };
        let worst_retrieve_p95_ms = samples
            .iter()
            .flat_map(|s| s.latencies_ms.get(crate::thresholds::RETRIEVE_METHOD))
            .map(|d| d.p95_ms)
            .fold(0.0_f64, f64::max);
        let last = samples.last();
        let cumulative_ok = last.map(|s| s.cumulative_ok).unwrap_or(0);
        let cumulative_errors = last.map(|s| s.cumulative_errors).unwrap_or(0);
        let green = breaches.is_empty();
        Self {
            started_at,
            ended_at,
            duration_s,
            sample_count: samples.len() as u64,
            peak_rss_mib,
            mean_rss_mib,
            worst_retrieve_p95_ms,
            cumulative_ok,
            cumulative_errors,
            breaches,
            green,
        }
    }

    /// Compact human-readable rendering (multi-line). Used for the
    /// stdout echo at end-of-run.
    pub fn render_text(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "cellar-stress run summary");
        let _ = writeln!(
            out,
            "  duration:                  {} ({}s)",
            humantime::format_duration(Duration::from_secs(self.duration_s)),
            self.duration_s
        );
        let _ = writeln!(out, "  samples:                   {}", self.sample_count);
        let _ = writeln!(
            out,
            "  peak rss:                  {:.1} MiB",
            self.peak_rss_mib
        );
        let _ = writeln!(
            out,
            "  mean rss:                  {:.1} MiB",
            self.mean_rss_mib
        );
        let _ = writeln!(
            out,
            "  worst retrieve p95:        {:.2} ms",
            self.worst_retrieve_p95_ms
        );
        let _ = writeln!(out, "  cumulative ok:             {}", self.cumulative_ok);
        let _ = writeln!(
            out,
            "  cumulative errors:         {}",
            self.cumulative_errors
        );
        if self.green {
            let _ = writeln!(
                out,
                "  verdict:                   GREEN (all thresholds passed)"
            );
        } else {
            let _ = writeln!(
                out,
                "  verdict:                   RED ({} breach(es))",
                self.breaches.len()
            );
            for (i, b) in self.breaches.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "    [{:>2}] {:?} @ uptime={}s observed={:.2} limit={:.2}",
                    i, b.kind, b.at_uptime_s, b.observed, b.limit
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{DaemonGauge, LatencyDistribution, MemoryGauge, MetricSample};
    use crate::thresholds::ThresholdViolation;
    use std::collections::HashMap;

    fn sample(uptime_s: u64, rss: f64, retrieve_p95: Option<f64>) -> MetricSample {
        let mut latencies_ms = HashMap::new();
        if let Some(p) = retrieve_p95 {
            latencies_ms.insert(
                "retrieve".into(),
                LatencyDistribution {
                    count: 10,
                    min_ms: 1.0,
                    max_ms: p,
                    mean_ms: p / 2.0,
                    p50_ms: p / 4.0,
                    p95_ms: p,
                    p99_ms: p,
                },
            );
        }
        MetricSample {
            ts: Utc::now(),
            uptime_s,
            rss_mib: rss,
            cpu_pct: 0.0,
            latencies_ms,
            ok_counts: HashMap::new(),
            err_counts: HashMap::new(),
            cumulative_errors: 0,
            cumulative_ok: 100,
            memory: MemoryGauge::default(),
            daemon: DaemonGauge::default(),
        }
    }

    #[test]
    fn aggregate_green_run() {
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(180);
        let samples = vec![
            sample(60, 50.0, Some(20.0)),
            sample(120, 60.0, Some(30.0)),
            sample(180, 80.0, Some(25.0)),
        ];
        let s = Summary::aggregate(start, end, &samples, vec![]);
        assert!(s.green);
        assert_eq!(s.sample_count, 3);
        assert!((s.peak_rss_mib - 80.0).abs() < 1e-9);
        assert!((s.mean_rss_mib - (50.0 + 60.0 + 80.0) / 3.0).abs() < 1e-9);
        assert!((s.worst_retrieve_p95_ms - 30.0).abs() < 1e-9);
        assert_eq!(s.cumulative_ok, 100);
        assert_eq!(s.duration_s, 180);
    }

    #[test]
    fn aggregate_red_run() {
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(60);
        let samples = vec![sample(60, 800.0, Some(500.0))];
        let breaches = vec![ThresholdBreach {
            kind: ThresholdViolation::RssExceeded,
            at_uptime_s: 60,
            observed: 800.0,
            limit: 500.0,
        }];
        let s = Summary::aggregate(start, end, &samples, breaches);
        assert!(!s.green);
        assert_eq!(s.breaches.len(), 1);
    }

    #[test]
    fn render_text_mentions_verdict() {
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(10);
        let s = Summary::aggregate(start, end, &[sample(10, 1.0, Some(1.0))], vec![]);
        let txt = s.render_text();
        assert!(txt.contains("GREEN"));
        assert!(txt.contains("samples:"));
    }

    #[test]
    fn render_text_includes_each_breach() {
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(10);
        let breach = ThresholdBreach {
            kind: ThresholdViolation::RetrieveP95Exceeded,
            at_uptime_s: 5,
            observed: 250.0,
            limit: 200.0,
        };
        let s = Summary::aggregate(start, end, &[sample(10, 1.0, Some(250.0))], vec![breach]);
        let txt = s.render_text();
        assert!(txt.contains("RED"));
        assert!(txt.contains("RetrieveP95Exceeded"));
        assert!(txt.contains("limit=200.00"));
    }

    #[test]
    fn empty_samples_dont_panic() {
        let start = Utc::now();
        let end = start;
        let s = Summary::aggregate(start, end, &[], vec![]);
        assert_eq!(s.sample_count, 0);
        assert_eq!(s.peak_rss_mib, 0.0);
        assert_eq!(s.mean_rss_mib, 0.0);
        assert_eq!(s.worst_retrieve_p95_ms, 0.0);
        assert!(s.green);
    }
}
