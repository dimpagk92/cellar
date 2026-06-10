//! Gateway-side [`ExecutionReceipt`] emission for governed actions.
//!
//! The gateway is the daemon's dispatch chokepoint: every governed action (and,
//! once the daemon hosts a Cortex, every UI action too) flows through
//! [`crate::gateway::Gateway::intercept`]. This module turns each `(action,
//! outcome)` into a canonical [`ExecutionReceipt`] and appends it to the run
//! timeline (`~/.cellar/runs/<run_id>.jsonl`, Receipt-Backed Run Timeline) so
//! `cellar timeline` and the cel-brief `ReceiptSource` surface the daemon
//! agent's actions. Best-effort: no run id (agent_session_id) → no timeline
//! write; I/O errors are swallowed so persistence never breaks dispatch.

use cel_contracts::{DispatchRoute, ExecutionReceipt, ObservedEffect, ReceiptStatus};
use serde_json::Value;

use crate::action::{ActionOutcome, ProposedAction};

/// Build a receipt for a governed-action dispatch. The governance verdict maps
/// onto [`ReceiptStatus`] (which carries `Vetoed` / `Denied` / `TimedOut` for
/// exactly this); the route is `Other { detail: "gateway" }` because governed
/// system actions are not a UI dispatch. Native-path observed-effect
/// verification doesn't apply, so `observed_effect` is `NotChecked`.
pub(crate) fn build_receipt(action: &ProposedAction, outcome: &ActionOutcome) -> ExecutionReceipt {
    let (status, error) = match outcome {
        ActionOutcome::Executed { .. } => (ReceiptStatus::Ok, None),
        ActionOutcome::Vetoed {
            rule_name,
            soft_block,
            ..
        } => (
            ReceiptStatus::Vetoed,
            Some(format!(
                "vetoed by rule '{rule_name}'{}",
                if *soft_block { " (soft_block)" } else { "" }
            )),
        ),
        ActionOutcome::ConfirmationDenied { rule_name, .. } => (
            ReceiptStatus::Denied,
            Some(format!("denied via rule '{rule_name}'")),
        ),
        ActionOutcome::ConfirmationTimedOut {
            rule_name,
            timeout_s,
            ..
        } => (
            ReceiptStatus::TimedOut,
            Some(format!(
                "confirmation timed out after {timeout_s}s (rule '{rule_name}')"
            )),
        ),
    };
    let now = now_ms();
    ExecutionReceipt {
        receipt_id: new_receipt_id(),
        run_id: action.agent_session_id.clone(),
        trace_id: None,
        action_kind: action.action_type.clone(),
        target: target_of(&action.action_args),
        route: DispatchRoute::Other {
            detail: "gateway".to_string(),
        },
        observed_effect: ObservedEffect::not_checked(),
        evidence: Vec::new(),
        requested_at_ms: now,
        completed_at_ms: now,
        // Gateway v1 records a coarse receipt; per-dispatch timing is a follow-up.
        duration_ms: 0,
        status,
        error,
    }
}

/// Append a run-scoped receipt to `~/.cellar/runs/<run_id>.jsonl`. No-op when
/// the action has no run scope (agent_session_id).
pub(crate) fn record_receipt(receipt: &ExecutionReceipt) {
    let Some(run_id) = receipt.run_id.as_deref() else {
        return;
    };
    let Some(dir) = runs_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{}.jsonl", sanitize_run_id(run_id)));
    let Ok(line) = serde_json::to_string(receipt) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Best-effort target extraction from the action args (for the timeline view).
fn target_of(args: &Value) -> Option<String> {
    for key in ["target_id", "path", "target", "app", "url"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn runs_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cellar").join("runs"))
}

/// Mirror the cortex writer + CLI reader sanitization.
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

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_receipt_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "rcpt_gw_{}_{}",
        now_ms(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action() -> ProposedAction {
        ProposedAction {
            caller: "embedded".into(),
            action_type: "shell.run".into(),
            action_args: serde_json::json!({ "path": "/tmp/x" }),
            agent_session_id: Some("sess-1".into()),
            project_root: None,
        }
    }

    #[test]
    fn executed_maps_to_ok() {
        let r = build_receipt(
            &action(),
            &ActionOutcome::Executed {
                result: Value::Null,
            },
        );
        assert_eq!(r.status, ReceiptStatus::Ok);
        assert_eq!(r.action_kind, "shell.run");
        assert_eq!(r.run_id.as_deref(), Some("sess-1"));
        assert_eq!(r.target.as_deref(), Some("/tmp/x"));
        assert!(matches!(r.route, DispatchRoute::Other { .. }));
        assert!(r.error.is_none());
    }

    #[test]
    fn vetoed_maps_to_vetoed_with_reason() {
        let r = build_receipt(
            &action(),
            &ActionOutcome::Vetoed {
                rule_id: "r1".into(),
                rule_name: "no-shell".into(),
                soft_block: false,
            },
        );
        assert_eq!(r.status, ReceiptStatus::Vetoed);
        assert!(r.error.as_deref().unwrap().contains("no-shell"));
    }

    #[test]
    fn timed_out_maps_to_timed_out() {
        let r = build_receipt(
            &action(),
            &ActionOutcome::ConfirmationTimedOut {
                rule_id: "r2".into(),
                rule_name: "confirm-delete".into(),
                timeout_s: 60,
            },
        );
        assert_eq!(r.status, ReceiptStatus::TimedOut);
    }
}
