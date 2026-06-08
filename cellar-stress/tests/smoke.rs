//! Integration smoke test for the stress harness.
//!
//! Runs the full harness against an in-process daemon for 30 seconds with
//! `--load-profile low` and asserts the verdict is GREEN. This is the
//! acceptance gate covering scenario 6 in the task brief: "an integration
//! test that runs the harness against an in-process daemon for 30 seconds
//! with `--load-profile low` and asserts everything stays green."
//!
//! Why 30 s and not 24 h: the task explicitly forbids a 24 h test in the
//! suite ("the 30-second smoke test in the test suite is sufficient"), so
//! we use the shortest run that still produces ≥1 metric sample.

use std::sync::Arc;
use std::time::Duration;

use cellar_stress::cli::{Args, LoadProfileArg};
use cellar_stress::harness;
use tempfile::TempDir;
use tokio::sync::Notify;

fn make_args(output_dir: std::path::PathBuf) -> Args {
    Args {
        duration: humantime::Duration::from(Duration::from_secs(30)),
        load_profile: LoadProfileArg::Low,
        // Sample every 10 s so we get at least 2 samples in a 30 s run.
        sample_interval: humantime::Duration::from(Duration::from_secs(10)),
        output_dir: Some(output_dir),
        // Be generous on the RSS threshold — CI runners with shared
        // memory pressure sometimes report higher numbers than a clean
        // workstation. The point of the smoke test is to assert "harness
        // is healthy" not "every threshold is tightly tuned."
        max_rss_mib: 1024.0,
        max_retrieve_p95_ms: 200.0,
        max_error_rate_per_min: 5.0,
        verbose: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn smoke_runs_thirty_seconds_low_profile_all_green() {
    let dir = TempDir::new().expect("temp dir");
    let args = make_args(dir.path().to_path_buf());
    let cancel = Arc::new(Notify::new());

    let outcome = harness::run(&args, cancel)
        .await
        .expect("harness should run cleanly under low load");

    // 30 s / 10 s = up to 3 samples; allow for early breaks.
    assert!(
        outcome.samples.len() >= 2,
        "expected ≥ 2 samples, got {}",
        outcome.samples.len()
    );

    // Sanity: the JSONL log and summary both exist.
    assert!(outcome.metrics_path.exists(), "metrics jsonl must exist");
    assert!(outcome.summary_path.exists(), "summary json must exist");
    let metrics_bytes = std::fs::metadata(&outcome.metrics_path).unwrap().len();
    assert!(metrics_bytes > 0, "metrics jsonl must be non-empty");

    // Sanity: at least some operations actually went through.
    assert!(
        outcome.summary.cumulative_ok > 0,
        "expected successful operations, got none (summary: {:#?})",
        outcome.summary
    );

    // Acceptance: green verdict. If this trips, the harness or the
    // daemon regressed and the failure list points at which threshold.
    assert!(
        outcome.summary.green,
        "smoke run was not green; breaches: {:#?}\nsummary text:\n{}",
        outcome.summary.breaches,
        outcome.summary.render_text(),
    );

    // Exit code maps as documented.
    assert_eq!(outcome.exit().code(), 0);

    // At least one sample should have recorded `retrieve` latency since the
    // retrieve generator is on in the low profile.
    assert!(
        outcome
            .samples
            .iter()
            .any(|s| s.latencies_ms.contains_key("retrieve")),
        "expected at least one sample with retrieve latency, got none"
    );

    // And at least one sample should have recorded gateway latency.
    assert!(
        outcome
            .samples
            .iter()
            .any(|s| s.latencies_ms.contains_key("gateway.intercept")),
        "expected at least one sample with gateway.intercept latency"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_respects_cancel_notify() {
    // Same setup but a much longer nominal duration; cancel after 5 s and
    // verify the harness still returns a valid outcome.
    let dir = TempDir::new().unwrap();
    let mut args = make_args(dir.path().to_path_buf());
    args.duration = humantime::Duration::from(Duration::from_secs(600));
    args.sample_interval = humantime::Duration::from(Duration::from_secs(2));

    let cancel = Arc::new(Notify::new());
    let cancel_for_signal = Arc::clone(&cancel);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        cancel_for_signal.notify_one();
    });

    let outcome = harness::run(&args, cancel)
        .await
        .expect("harness should honor cancellation");

    // We canceled at ~5 s with samples every 2 s, so expect at least one.
    assert!(
        !outcome.samples.is_empty(),
        "expected at least one sample even after cancel"
    );
    // The run should have stopped well short of the 600 s nominal duration.
    assert!(outcome.summary.duration_s < 600);
}
