//! `cellar-stress` binary entry point.
//!
//! Brings up an in-process Cellar daemon, drives synthetic load against
//! every subsystem, samples health every minute, and emits JSONL + summary.
//!
//! Exit codes:
//! - `0` — all thresholds passed.
//! - `2` — one or more thresholds breached during the run.
//! - `1` — the harness itself errored (couldn't write the JSONL file,
//!   couldn't wire the daemon, etc.).
//!
//! See `cellar_stress` crate docs for what the harness can and can't drive.

use std::sync::Arc;

use anyhow::Result;
use cellar_stress::{harness, Args};
use clap::Parser;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    harness::init_tracing(args.verbose);

    // SIGINT cancellation. The harness honors `Notify::notify_one` to stop
    // sampling and drain its generators.
    let cancel = Arc::new(Notify::new());
    let cancel_for_signal = Arc::clone(&cancel);
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            tracing::warn!("SIGINT received, requesting harness shutdown");
            cancel_for_signal.notify_one();
        }
    });

    let outcome = harness::run(&args, cancel).await?;
    println!("{}", outcome.summary.render_text());
    println!("metrics: {}", outcome.metrics_path.display());
    println!("summary: {}", outcome.summary_path.display());

    std::process::exit(outcome.exit().code());
}
