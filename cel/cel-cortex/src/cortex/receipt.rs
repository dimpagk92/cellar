//! Shared execution-receipt assembly for the cortex dispatch paths.
//!
//! PR1 emitted receipts from the CDP path (`cdp.rs`); this module hoists the
//! timing / id / transport helpers so the native `execute` path reuses them,
//! and adds route classification for non-CDP dispatch. See
//! `~/.claude/plans/cellar-receipt-timeline.md` (Receipt-Backed Run Timeline).

use super::dispatch::{action_dom_target, action_type_str};
use cel_contracts::{
    DispatchRoute, ExecutionReceipt, ObservedEffect, PlannedAction, ReceiptStatus,
};

/// Reserved `ActionResult.data` key the receipt is transported under (PR1
/// rides in `data` rather than a first-class field — see `attach_receipt`).
pub(crate) const RECEIPT_DATA_KEY: &str = "_cel_receipt";

/// Process-global "current run" id. Set by the run owner (e.g. an MCP
/// `cel_perceive` session) so the receipts emitted during a run group under one
/// id in the timeline. `None` for one-off actions outside a run.
///
/// v1 assumes a single active run per process — the warm Cortex is
/// one-per-process and the MCP perceive session is the run scope. A per-action
/// `run_id` override and a finer `trace_id` are follow-ups (plan Open Decision #1).
static CURRENT_RUN_ID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Set (or replace) the current run id. Pass `None` to clear.
pub fn set_run_id(run_id: Option<String>) {
    if let Ok(mut guard) = CURRENT_RUN_ID.lock() {
        *guard = run_id;
    }
}

/// Clear the current run id (run ended).
pub fn clear_run_id() {
    set_run_id(None);
}

/// The current run id, stamped onto every receipt the cortex emits.
pub fn current_run_id() -> Option<String> {
    CURRENT_RUN_ID.lock().ok().and_then(|g| g.clone())
}

/// Milliseconds since the Unix epoch (receipt timing).
pub(crate) fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Process-unique receipt id: monotonic counter + wall-clock ms. Good enough
/// for in-process correlation until run/trace scoping lands (plan Phase 1).
pub(crate) fn new_receipt_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("rcpt_{}_{}", now_ms(), n)
}

/// Whether a result already carries a receipt — the CDP path stamps its own,
/// so the native `execute` wrapper must not double-attach.
pub(crate) fn has_receipt(result: &crate::adapter::ActionResult) -> bool {
    result
        .data
        .as_ref()
        .and_then(|d| d.as_object())
        .is_some_and(|m| m.contains_key(RECEIPT_DATA_KEY))
}

/// Transport the typed [`ExecutionReceipt`] on the result's `data` under the
/// reserved key.
///
/// PR1 rides in `data` (additive, non-breaking) rather than a first-class
/// `ActionResult.receipt` field: adding that field would force a mechanical
/// edit across ~30 struct-literal construction sites in every adapter crate.
/// The receipt TYPE is canonical (`cel-contracts`); only its transport is via
/// `data` for now. Promoting to a field is a deliberate follow-up.
pub(crate) fn attach_receipt(
    mut result: crate::adapter::ActionResult,
    receipt: ExecutionReceipt,
) -> crate::adapter::ActionResult {
    let Ok(rv) = serde_json::to_value(&receipt) else {
        return result;
    };
    let mut map = match result.data.take() {
        Some(serde_json::Value::Object(map)) => map,
        Some(other) => {
            // Preserve any prior non-object payload under a sibling key.
            let mut m = serde_json::Map::new();
            m.insert("result".to_string(), other);
            m
        }
        None => serde_json::Map::new(),
    };
    map.insert(RECEIPT_DATA_KEY.to_string(), rv);
    result.data = Some(serde_json::Value::Object(map));
    result
}

/// Classify the dispatch route for a non-CDP action.
///
/// Runs AFTER the CDP fall-through, so a `click` reaching here really is native
/// input (a `dom:*` click would have been CDP-handled and already carry its own
/// receipt). Returns `None` for control / data / wrapper actions that are not a
/// device dispatch (Wait, Done, Fail, Extract, Batch, Act, Custom,
/// NotebookWrites) — those carry no receipt.
pub(crate) fn native_route_for(action: &PlannedAction) -> Option<DispatchRoute> {
    match action {
        PlannedAction::Click { .. }
        | PlannedAction::Type { .. }
        | PlannedAction::Key { .. }
        | PlannedAction::KeyCombo { .. }
        | PlannedAction::Scroll { .. }
        | PlannedAction::Drag { .. } => Some(DispatchRoute::NativeInput),
        PlannedAction::AxAction { .. }
        | PlannedAction::SetValue { .. }
        | PlannedAction::Select { .. }
        | PlannedAction::Window { .. }
        | PlannedAction::Dialog { .. }
        | PlannedAction::Dock { .. }
        | PlannedAction::MenuExtra { .. } => Some(DispatchRoute::Accessibility),
        PlannedAction::ActivateApp { .. }
        | PlannedAction::LaunchApp { .. }
        | PlannedAction::QuitApp { .. } => Some(DispatchRoute::Focus),
        PlannedAction::WriteCells { .. } | PlannedAction::ReadCells { .. } => {
            Some(DispatchRoute::Adapter {
                name: String::new(),
                op: action_type_str(action).to_string(),
            })
        }
        // CDP-backed but not a `dom:*` target (so it fell through
        // `try_cdp_dispatch` and is dispatched on the native path).
        PlannedAction::CdpEval { .. } | PlannedAction::Navigate { .. } => Some(DispatchRoute::Cdp),
        // Control / data / wrapper actions — not a device dispatch.
        PlannedAction::Wait { .. }
        | PlannedAction::Done { .. }
        | PlannedAction::Fail { .. }
        | PlannedAction::Extract { .. }
        | PlannedAction::ExtractWithFallback { .. }
        | PlannedAction::NotebookWrites { .. }
        | PlannedAction::Custom { .. }
        | PlannedAction::Act { .. }
        | PlannedAction::Batch { .. } => None,
    }
}

/// Build a receipt for a native-path dispatch. Observed-effect verification on
/// the native path (adapter readback / AX re-read) is a later phase, so this
/// records `NotChecked`.
pub(crate) fn build_native_receipt(
    action: &PlannedAction,
    route: DispatchRoute,
    requested_at_ms: u64,
    completed_at_ms: u64,
    result: &crate::adapter::ActionResult,
) -> ExecutionReceipt {
    ExecutionReceipt {
        receipt_id: new_receipt_id(),
        run_id: current_run_id(),
        trace_id: None,
        action_kind: action_type_str(action).to_string(),
        target: action_dom_target(action).map(str::to_string),
        route,
        observed_effect: ObservedEffect::not_checked(),
        evidence: Vec::new(),
        requested_at_ms,
        completed_at_ms,
        duration_ms: completed_at_ms.saturating_sub(requested_at_ms),
        status: if result.success {
            ReceiptStatus::Ok
        } else {
            ReceiptStatus::Failed
        },
        error: result.error.clone(),
    }
}

/// Append a run-scoped receipt to `~/.cellar/runs/<run_id>.jsonl` so
/// `cellar timeline <run_id>` can render the run. Best-effort: receipts with no
/// run id are skipped, and any I/O error is swallowed (persistence must never
/// break dispatch). Mirrors the `~/.cellar/observations/` pattern.
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

fn runs_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cellar").join("runs"))
}

/// Restrict a run id to filename-safe characters (mirrored by the CLI reader).
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
