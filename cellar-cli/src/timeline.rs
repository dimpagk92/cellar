//! `cellar timeline <run_id>` — render the execution-receipt timeline for a run.
//!
//! Receipts are appended to `~/.cellar/runs/<run_id>.jsonl` by the cortex as it
//! dispatches actions during a run (a `cel_perceive` session scopes the run id).
//! This command reads that file directly — no daemon needed — and renders the
//! `intent → dispatch route → observed effect → evidence` spine for each step.

use anyhow::Result;
use std::io::BufRead;

/// Read and render the timeline for `run_id`. With `json`, prints the raw
/// receipt array instead of the human table.
pub fn show(run_id: &str, json: bool) -> Result<()> {
    let path = run_file(run_id);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => {
            if json {
                println!("[]");
            } else {
                println!(
                    "No timeline for run \"{run_id}\" (looked in {}).",
                    path.display()
                );
                println!("Runs are recorded while a `cel_perceive` session is active.");
            }
            return Ok(());
        }
    };

    let receipts: Vec<serde_json::Value> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&receipts)?);
        return Ok(());
    }

    println!("Run {run_id} — {} receipt(s)", receipts.len());
    for r in &receipts {
        let status = field(r, "status");
        let kind = field(r, "action_kind");
        let route = r
            .get("route")
            .and_then(|v| v.get("route"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let effect = r
            .get("observed_effect")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let dur = r.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let target = r.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let id = r.get("receipt_id").and_then(|v| v.as_str()).unwrap_or("");
        println!(
            "  {status:<8} {kind:<12} route={route:<13} effect={effect:<12} {dur:>5}ms  {target}  [{id}]"
        );
    }
    Ok(())
}

fn field<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("?")
}

fn run_file(run_id: &str) -> std::path::PathBuf {
    runs_dir().join(format!("{}.jsonl", sanitize_run_id(run_id)))
}

fn runs_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".cellar")
        .join("runs")
}

/// Must match the cortex writer's sanitization (`cel-cortex` `receipt.rs`).
fn sanitize_run_id(run_id: &str) -> String {
    run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
