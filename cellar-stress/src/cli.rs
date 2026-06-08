//! Command-line surface.
//!
//! Parsed by [`clap`]. The binary uses [`Args::parse`] and feeds the result
//! into [`crate::harness::run`]; the integration test constructs the same
//! shape programmatically.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};

/// Top-level CLI args.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "cellar-stress",
    version,
    about = "Synthetic load harness for the Cellar daemon.",
    long_about = "Drives the in-process daemon with file-system, gateway, \
                  agent-chat, and memory load. Samples daemon health every \
                  minute and exits non-zero if any threshold tripped."
)]
pub struct Args {
    /// How long to run the harness for. Accepts durations like `30s`, `15m`,
    /// `2h`, `24h`. Required.
    #[arg(long)]
    pub duration: humantime::Duration,

    /// Load profile — sets the per-second rate of each generator.
    #[arg(long, value_enum, default_value_t = LoadProfileArg::Medium)]
    pub load_profile: LoadProfileArg,

    /// How often to emit a metric sample. Default: one sample per minute.
    #[arg(long, default_value = "60s")]
    pub sample_interval: humantime::Duration,

    /// Output directory for the JSONL metrics log and summary report. The
    /// harness writes `metrics.jsonl` and `summary.json` here. Created if
    /// missing. Default: a fresh temp dir.
    #[arg(long)]
    pub output_dir: Option<PathBuf>,

    /// Override the RSS threshold (in MiB). The harness exits non-zero if
    /// the daemon's resident memory exceeds this at any sample.
    #[arg(long, default_value_t = 500.0)]
    pub max_rss_mib: f64,

    /// Override the `retrieve` p95 latency threshold (in milliseconds).
    /// Per `cellar-memory-manager.md` §14.4 the budget is 200 ms.
    #[arg(long, default_value_t = 200.0)]
    pub max_retrieve_p95_ms: f64,

    /// Override the IPC error-rate threshold (errors per minute). The
    /// harness counts failed gateway / memory / IPC calls and exits
    /// non-zero if the per-minute rate exceeds this.
    #[arg(long, default_value_t = 0.1)]
    pub max_error_rate_per_min: f64,

    /// Verbose tracing output (default: warn + cellar_stress=info).
    #[arg(long)]
    pub verbose: bool,
}

impl Args {
    /// Convenience: the configured run duration as a `std::time::Duration`.
    pub fn duration_std(&self) -> Duration {
        *self.duration
    }

    /// Convenience: the configured sample interval as a `std::time::Duration`.
    pub fn sample_interval_std(&self) -> Duration {
        *self.sample_interval
    }
}

/// The three preset load profiles.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum LoadProfileArg {
    /// ~1 op/s per generator. Suitable for the in-test smoke run.
    Low,
    /// ~10 ops/s per generator. The default for ad-hoc multi-hour runs.
    Medium,
    /// ~50 ops/s per generator. Approximates a worst-case burst.
    High,
}

impl From<LoadProfileArg> for crate::load::LoadProfile {
    fn from(arg: LoadProfileArg) -> Self {
        match arg {
            LoadProfileArg::Low => crate::load::LoadProfile::low(),
            LoadProfileArg::Medium => crate::load::LoadProfile::medium(),
            LoadProfileArg::High => crate::load::LoadProfile::high(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn args_parses_basic_invocation() {
        let args = Args::try_parse_from([
            "cellar-stress",
            "--duration",
            "30s",
            "--load-profile",
            "low",
        ])
        .expect("parses");
        assert_eq!(args.duration_std(), Duration::from_secs(30));
        assert_eq!(args.load_profile, LoadProfileArg::Low);
        assert_eq!(args.sample_interval_std(), Duration::from_secs(60));
        assert_eq!(args.max_rss_mib, 500.0);
        assert_eq!(args.max_retrieve_p95_ms, 200.0);
    }

    #[test]
    fn args_accepts_24h_duration() {
        let args = Args::try_parse_from([
            "cellar-stress",
            "--duration",
            "24h",
            "--load-profile",
            "medium",
        ])
        .expect("parses");
        assert_eq!(args.duration_std(), Duration::from_secs(24 * 3600));
    }

    #[test]
    fn cli_definition_is_well_formed() {
        // Clap will panic at runtime if anything is misconfigured; force the
        // definition build path.
        Args::command().debug_assert();
    }

    #[test]
    fn load_profile_arg_maps_to_real_profile() {
        let low: crate::load::LoadProfile = LoadProfileArg::Low.into();
        let med: crate::load::LoadProfile = LoadProfileArg::Medium.into();
        let high: crate::load::LoadProfile = LoadProfileArg::High.into();
        assert!(low.fs_ops_per_sec < med.fs_ops_per_sec);
        assert!(med.fs_ops_per_sec < high.fs_ops_per_sec);
    }
}
