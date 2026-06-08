//! Metric collection, latency tracking, and JSONL serialisation.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// One snapshot in time. Serialised as a JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    /// When the sample was taken.
    pub ts: DateTime<Utc>,
    /// Seconds since harness start.
    pub uptime_s: u64,
    /// Daemon resident memory in MiB (best-effort via `sysinfo`).
    pub rss_mib: f64,
    /// Daemon CPU usage as a percentage (best-effort via `sysinfo`).
    pub cpu_pct: f64,
    /// Per-method latency distributions over the last sample window.
    /// Empty methods are absent (no fixed slot per method).
    pub latencies_ms: HashMap<String, LatencyDistribution>,
    /// Per-method success counts over the last sample window.
    pub ok_counts: HashMap<String, u64>,
    /// Per-method error counts over the last sample window.
    pub err_counts: HashMap<String, u64>,
    /// Cumulative error count from start.
    pub cumulative_errors: u64,
    /// Cumulative success count from start.
    pub cumulative_ok: u64,
    /// Memory subsystem stats — chunk count, session count, db size.
    /// Read from `MemoryProvider::stats()`.
    pub memory: MemoryGauge,
    /// Daemon rule + watchlist + recent-fire counts from `daemon.status`.
    pub daemon: DaemonGauge,
}

/// Latency distribution snapshot for a single method. All times in
/// milliseconds. `count == 0` indicates no samples.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct LatencyDistribution {
    /// Number of samples this window.
    pub count: u64,
    /// Minimum latency seen.
    pub min_ms: f64,
    /// Maximum latency seen.
    pub max_ms: f64,
    /// Mean latency.
    pub mean_ms: f64,
    /// 50th-percentile latency.
    pub p50_ms: f64,
    /// 95th-percentile latency.
    pub p95_ms: f64,
    /// 99th-percentile latency.
    pub p99_ms: f64,
}

impl LatencyDistribution {
    /// Build a distribution from a slice of millisecond samples. Returns the
    /// default (empty) when `samples` is empty so JSONL stays uniform.
    pub fn from_ms(mut samples: Vec<f64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let count = samples.len() as u64;
        let min_ms = samples[0];
        let max_ms = *samples.last().unwrap();
        let mean_ms = samples.iter().sum::<f64>() / samples.len() as f64;
        Self {
            count,
            min_ms,
            max_ms,
            mean_ms,
            p50_ms: percentile(&samples, 0.50),
            p95_ms: percentile(&samples, 0.95),
            p99_ms: percentile(&samples, 0.99),
        }
    }
}

/// Linear-interpolated percentile. Internal helper, public for unit tests.
pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let q = q.clamp(0.0, 1.0);
    let idx = q * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = idx - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Memory subsystem gauge snapshot, derived from `MemoryProvider::stats()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryGauge {
    /// Total chunks across both tiers.
    pub total_chunks: u64,
    /// Open agent sessions.
    pub open_sessions: u64,
    /// On-disk DB size in bytes (0 for in-memory backend).
    pub db_bytes: u64,
}

/// Daemon-status gauge snapshot. Pulled from
/// [`cellar_ipc::results::daemon::DaemonStatusResult`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DaemonGauge {
    /// Total rules in the store.
    pub rules_total: u64,
    /// Total watchlists in the store.
    pub watchlists_total: u64,
    /// Recent fires (last 24h, as reported by the daemon).
    pub recent_fires_24h: u64,
    /// Active agent sessions (per the daemon's own count).
    pub agent_sessions_active: u64,
    /// Pending confirmations.
    pub pending_confirmations: u64,
}

/// In-process latency accumulator. Concurrent producers push samples; one
/// consumer drains them once per sample window.
#[derive(Debug, Default)]
pub struct MetricStream {
    inner: Mutex<StreamInner>,
}

#[derive(Debug, Default)]
struct StreamInner {
    /// Per-method samples for the current window.
    samples: HashMap<String, Vec<f64>>,
    /// Per-method success count for the current window.
    ok: HashMap<String, u64>,
    /// Per-method error count for the current window.
    err: HashMap<String, u64>,
    /// Cumulative since harness start.
    cum_ok: u64,
    cum_err: u64,
}

impl MetricStream {
    /// New empty stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one successful call's latency.
    pub fn record_ok(&self, method: &str, dur: Duration) {
        let mut g = self.inner.lock().expect("stream poisoned");
        let ms = duration_to_ms(dur);
        g.samples.entry(method.to_string()).or_default().push(ms);
        *g.ok.entry(method.to_string()).or_default() += 1;
        g.cum_ok += 1;
    }

    /// Record one failed call. Errors don't contribute to latency — they
    /// often complete fast (e.g., validation refusal) and would skew p95.
    pub fn record_err(&self, method: &str) {
        let mut g = self.inner.lock().expect("stream poisoned");
        *g.err.entry(method.to_string()).or_default() += 1;
        g.cum_err += 1;
    }

    /// Consume the current window into distributions and counts; reset the
    /// window. Cumulative counts persist.
    pub fn drain_window(&self) -> WindowSummary {
        let mut g = self.inner.lock().expect("stream poisoned");
        let samples = std::mem::take(&mut g.samples);
        let ok = std::mem::take(&mut g.ok);
        let err = std::mem::take(&mut g.err);
        let latencies: HashMap<String, LatencyDistribution> = samples
            .into_iter()
            .map(|(k, v)| (k, LatencyDistribution::from_ms(v)))
            .collect();
        WindowSummary {
            latencies,
            ok_counts: ok,
            err_counts: err,
            cumulative_ok: g.cum_ok,
            cumulative_err: g.cum_err,
        }
    }
}

/// What [`MetricStream::drain_window`] returns. Helper shape.
#[derive(Debug, Default)]
pub struct WindowSummary {
    /// Per-method latency distributions for the drained window.
    pub latencies: HashMap<String, LatencyDistribution>,
    /// Per-method ok counts.
    pub ok_counts: HashMap<String, u64>,
    /// Per-method err counts.
    pub err_counts: HashMap<String, u64>,
    /// Cumulative ok count (does not reset).
    pub cumulative_ok: u64,
    /// Cumulative err count (does not reset).
    pub cumulative_err: u64,
}

fn duration_to_ms(d: Duration) -> f64 {
    (d.as_secs_f64()) * 1000.0
}

/// Convert a `MetricSample` to a JSONL line (no trailing newline; the writer
/// adds one).
pub fn sample_to_json_line(sample: &MetricSample) -> serde_json::Result<String> {
    serde_json::to_string(sample)
}

/// Append a JSONL line to the writer. Splits out so the harness can swap in
/// a `Vec<u8>` for testing without touching tokio fs.
pub async fn append_jsonl<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    sample: &MetricSample,
) -> std::io::Result<()> {
    let line = sample_to_json_line(sample).map_err(std::io::Error::other)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_basics() {
        let s = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&s, 0.0), 1.0);
        assert_eq!(percentile(&s, 1.0), 5.0);
        assert_eq!(percentile(&s, 0.5), 3.0);
    }

    #[test]
    fn percentile_interpolates() {
        // Two-element series: q=0.5 should land exactly between.
        let s = vec![10.0, 20.0];
        assert!((percentile(&s, 0.5) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn percentile_empty_is_zero() {
        let s: Vec<f64> = vec![];
        assert_eq!(percentile(&s, 0.95), 0.0);
    }

    #[test]
    fn distribution_from_uniform_samples() {
        let d = LatencyDistribution::from_ms(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(d.count, 5);
        assert!((d.min_ms - 1.0).abs() < 1e-9);
        assert!((d.max_ms - 5.0).abs() < 1e-9);
        assert!((d.mean_ms - 3.0).abs() < 1e-9);
        assert!((d.p50_ms - 3.0).abs() < 1e-9);
        // 95th of 5 samples (linear interp): 4*0.05 + 5*0.95 = 4.95… let me
        // recompute: idx = 0.95 * 4 = 3.8 → 4*(1-0.8) + 5*0.8 = 0.8 + 4.0 = 4.8.
        assert!((d.p95_ms - 4.8).abs() < 1e-9);
    }

    #[test]
    fn distribution_from_empty_is_default() {
        let d = LatencyDistribution::from_ms(vec![]);
        assert_eq!(d, LatencyDistribution::default());
        assert_eq!(d.count, 0);
    }

    #[test]
    fn stream_records_and_drains() {
        let s = MetricStream::new();
        s.record_ok("retrieve", Duration::from_millis(10));
        s.record_ok("retrieve", Duration::from_millis(20));
        s.record_err("retrieve");
        s.record_ok("write", Duration::from_millis(5));

        let win = s.drain_window();
        assert_eq!(*win.ok_counts.get("retrieve").unwrap(), 2);
        assert_eq!(*win.err_counts.get("retrieve").unwrap(), 1);
        assert_eq!(*win.ok_counts.get("write").unwrap(), 1);
        assert_eq!(win.cumulative_ok, 3);
        assert_eq!(win.cumulative_err, 1);

        // Latencies look right.
        let d = win.latencies.get("retrieve").unwrap();
        assert_eq!(d.count, 2);
        assert!((d.min_ms - 10.0).abs() < 1e-6);
        assert!((d.max_ms - 20.0).abs() < 1e-6);

        // Drained — next call to drain returns empty windows but cumulative
        // persists.
        let win2 = s.drain_window();
        assert!(win2.latencies.is_empty());
        assert!(win2.ok_counts.is_empty());
        assert_eq!(win2.cumulative_ok, 3);
        assert_eq!(win2.cumulative_err, 1);
    }

    #[test]
    fn sample_round_trips() {
        let mut lat = HashMap::new();
        lat.insert(
            "retrieve".into(),
            LatencyDistribution {
                count: 1,
                min_ms: 5.0,
                max_ms: 5.0,
                mean_ms: 5.0,
                p50_ms: 5.0,
                p95_ms: 5.0,
                p99_ms: 5.0,
            },
        );
        let s = MetricSample {
            ts: Utc::now(),
            uptime_s: 60,
            rss_mib: 42.5,
            cpu_pct: 1.2,
            latencies_ms: lat,
            ok_counts: HashMap::new(),
            err_counts: HashMap::new(),
            cumulative_errors: 0,
            cumulative_ok: 1,
            memory: MemoryGauge::default(),
            daemon: DaemonGauge::default(),
        };
        let line = sample_to_json_line(&s).unwrap();
        let back: MetricSample = serde_json::from_str(&line).unwrap();
        assert_eq!(back.uptime_s, 60);
        assert_eq!(back.cumulative_ok, 1);
    }
}
