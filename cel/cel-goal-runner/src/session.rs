//! Resumable goal sessions (WS9).
//!
//! Persists enough of a goal run — the goal text, the ordered action log, and
//! accumulated metrics — to disk so that a run interrupted by a crash, a kill,
//! or a deliberate suspend can be reloaded and *continued* instead of restarted
//! from scratch.
//!
//! This module is deliberately the **persistence + state** layer only. Wiring
//! it into the [`crate::CanonicalGoalRunner`] loop (snapshot-after-each-step,
//! resume-from-cursor on startup) is the remaining integration step — see the
//! "Resuming a run" design notes at the bottom of this file. Keeping the
//! persistence layer standalone makes it unit-testable without standing up a
//! Cortex or a live device, which is why it lands first.

use crate::outcome::{ActionRecord, GoalMetrics, GoalStatus};
use cel_contracts::GoalOutcome;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Lifecycle of a persisted session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// Actively executing.
    Running,
    /// Persisted mid-run, awaiting resume.
    Suspended,
    /// Reached a terminal [`GoalStatus`].
    Completed(GoalStatus),
}

/// A serializable snapshot of a goal run, sufficient to resume it.
///
/// The resume cursor is implicit: `action_log.len()` is the number of steps
/// already executed, so [`SessionState::next_step_index`] is exactly that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// Stable session id (also the on-disk filename stem).
    pub id: String,
    /// The goal prompt being pursued.
    pub goal: String,
    /// Lifecycle state.
    pub status: SessionStatus,
    /// Ordered record of steps executed so far.
    #[serde(default)]
    pub action_log: Vec<ActionRecord>,
    /// Planner attempt history, for *history-exact* resume — the runner seeds
    /// its in-memory `Vec<AttemptRecord>` from this so a resumed run continues
    /// the planner's reasoning rather than re-planning from scratch. Default
    /// empty (older sessions / terminal `Completed` persists omit it).
    #[serde(default)]
    pub attempt_history: Vec<cel_contracts::AttemptRecord>,
    /// Accumulated run metrics.
    #[serde(default)]
    pub metrics: GoalMetrics,
    /// Creation time (ms since the Unix epoch).
    pub created_at_ms: u64,
    /// Last-update time (ms since the Unix epoch).
    pub updated_at_ms: u64,
}

impl SessionState {
    /// Start a fresh, `Running` session.
    pub fn new(id: impl Into<String>, goal: impl Into<String>, now_ms: u64) -> Self {
        Self {
            id: id.into(),
            goal: goal.into(),
            status: SessionStatus::Running,
            action_log: Vec::new(),
            attempt_history: Vec::new(),
            metrics: GoalMetrics::default(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        }
    }

    /// The step index to resume from = the number of completed steps.
    pub fn next_step_index(&self) -> u32 {
        self.action_log.len() as u32
    }

    /// Append a completed step and bump the update timestamp.
    pub fn record_step(&mut self, record: ActionRecord, now_ms: u64) {
        self.action_log.push(record);
        self.updated_at_ms = now_ms;
    }

    /// Mark the session suspended (persisted mid-run, still resumable).
    pub fn suspend(&mut self, now_ms: u64) {
        self.status = SessionStatus::Suspended;
        self.updated_at_ms = now_ms;
    }

    /// Mark the session finished with a terminal [`GoalStatus`].
    pub fn complete(&mut self, status: GoalStatus, now_ms: u64) {
        self.status = SessionStatus::Completed(status);
        self.updated_at_ms = now_ms;
    }

    /// Whether the session can be resumed (running or suspended — not completed).
    pub fn is_resumable(&self) -> bool {
        matches!(
            self.status,
            SessionStatus::Running | SessionStatus::Suspended
        )
    }

    /// Build a `Completed` session from a finished run. The runner's
    /// [`GoalOutcome`] (cel-contracts) maps to a terminal [`GoalStatus`]; the
    /// caller supplies the executor's action log (`CortexStepExecutor::snapshot_log`)
    /// and metrics. This is the bridge from runner output to a persistable
    /// session: `GoalOutcome` at the `run()` boundary carries only status +
    /// summary, while the action log lives in the executor — so the caller,
    /// which holds both, owns the `save_session` call. Mid-run checkpointing
    /// and cursor-based resume inside the runner loop is the larger follow-up
    /// (see the "Resuming a run" notes below).
    pub fn from_outcome(
        id: impl Into<String>,
        goal: impl Into<String>,
        outcome: &GoalOutcome,
        action_log: Vec<ActionRecord>,
        metrics: GoalMetrics,
        now_ms: u64,
    ) -> Self {
        let status = match outcome {
            GoalOutcome::Succeeded { .. } => GoalStatus::Achieved,
            GoalOutcome::Failed(_) => GoalStatus::Failed,
            GoalOutcome::Refused { .. } => GoalStatus::Refused,
        };
        Self {
            id: id.into(),
            goal: goal.into(),
            status: SessionStatus::Completed(status),
            action_log,
            attempt_history: Vec::new(),
            metrics,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        }
    }
}

/// Current wall-clock in ms since the Unix epoch (for real callers; tests pass
/// fixed timestamps for determinism).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Directory sessions are persisted in: `$CELLAR_SESSION_DIR`, else
/// `~/.cellar/sessions`, else a temp fallback.
pub fn default_session_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CELLAR_SESSION_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cellar").join("sessions");
    }
    std::env::temp_dir().join("cellar-sessions")
}

fn session_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Persist a session as pretty JSON to `<dir>/<id>.json`, creating `dir` if
/// needed. Returns the written path.
pub fn save_session(dir: &Path, state: &SessionState) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = session_path(dir, &state.id);
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Load a session by id from `<dir>/<id>.json`.
pub fn load_session(dir: &Path, id: &str) -> std::io::Result<SessionState> {
    let path = session_path(dir, id);
    let json = std::fs::read_to_string(&path)?;
    serde_json::from_str(&json).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// List the ids of resumable sessions found in `dir` (sorted). A missing
/// directory is not an error — it yields an empty list. Unparseable or
/// completed sessions are skipped.
pub fn list_resumable_sessions(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut ids = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(json) = std::fs::read_to_string(&path) {
            if let Ok(state) = serde_json::from_str::<SessionState>(&json) {
                if state.is_resumable() {
                    ids.push(state.id);
                }
            }
        }
    }
    ids.sort();
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Resuming a run — wiring status (WS9).
//
// WIRED into `CanonicalGoalRunner` (env-gated, default-off — see `run` /
// `run_inner`):
//   • `CELLAR_SESSION_DIR` (+ optional `CELLAR_SESSION_ID`) → the runner
//     checkpoints a `Running` snapshot at each loop iteration and persists a
//     terminal `Completed` session (via `SessionState::from_outcome`) when the
//     run ends. An interrupted run leaves a resumable file.
//   • `CELLAR_RESUME` → on start the runner advances the step counter to
//     `next_step_index()`, seeds the executor's action log
//     (`StepExecutor::seed_action_log`), AND seeds the planner's in-memory
//     `Vec<AttemptRecord>` from the checkpoint's `attempt_history` — so resume
//     is HISTORY-EXACT: the planner continues its prior reasoning rather than
//     re-planning from scratch. The loop checkpoint persists both the executor
//     `ActionRecord` log and the planner `AttemptRecord` history each iteration.
//
// REMAINING (needs live-host verification):
//   • Idempotency. Resume replays from the *action* boundary, so an action
//     whose side effect landed but whose post-state verification didn't persist
//     could be re-issued. Safe resume wants idempotency keys or a
//     re-perceive-and-verify pass before re-issuing the cursor step. Plus a
//     live agent run to validate the end-to-end resume.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir per test (avoids a `tempfile` dev-dep; tests in this
    /// module use distinct tags so they never collide within one process).
    fn unique_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cellar-ws9-{}-{}", tag, std::process::id()))
    }

    fn sample(id: &str) -> SessionState {
        let mut s = SessionState::new(id, "book a flight to Athens", 1_000);
        s.record_step(
            ActionRecord {
                step_index: 0,
                kind: "click".into(),
                subtype: None,
                target_id: Some("btn-1".into()),
                args: None,
                planner_confidence: Some(0.9),
                succeeded: true,
                verified: true,
                latency_ms: 42,
                error: None,
            },
            2_000,
        );
        s
    }

    #[test]
    fn next_step_index_tracks_action_log() {
        let s = sample("s1");
        assert_eq!(s.next_step_index(), 1);
    }

    #[test]
    fn lifecycle_transitions() {
        let mut s = SessionState::new("s2", "goal", 0);
        assert!(s.is_resumable());
        s.suspend(10);
        assert_eq!(s.status, SessionStatus::Suspended);
        assert!(s.is_resumable());
        s.complete(GoalStatus::Achieved, 20);
        assert_eq!(s.status, SessionStatus::Completed(GoalStatus::Achieved));
        assert!(!s.is_resumable());
        assert_eq!(s.updated_at_ms, 20);
    }

    #[test]
    fn save_load_round_trip_is_lossless() {
        let dir = unique_dir("roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let s = sample("round");
        let path = save_session(&dir, &s).unwrap();
        assert!(path.exists());
        let loaded = load_session(&dir, "round").unwrap();
        // ActionRecord / GoalMetrics don't derive PartialEq, so compare via the
        // serialized form — a lossless round-trip yields byte-identical JSON.
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            serde_json::to_string(&loaded).unwrap()
        );
        assert_eq!(loaded.goal, "book a flight to Athens");
        assert_eq!(loaded.next_step_index(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_resumable_filters_completed() {
        let dir = unique_dir("list");
        let _ = std::fs::remove_dir_all(&dir);
        let running = sample("alive");
        let mut done = sample("done");
        done.complete(GoalStatus::Achieved, 3_000);
        save_session(&dir, &running).unwrap();
        save_session(&dir, &done).unwrap();
        let ids = list_resumable_sessions(&dir).unwrap();
        assert_eq!(ids, vec!["alive".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_missing_dir_is_empty() {
        let dir = unique_dir("missing").join("nope");
        let ids = list_resumable_sessions(&dir).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn from_outcome_maps_status_and_preserves_log() {
        let log = sample("ignored").action_log; // reuse the one ActionRecord
        let succeeded = GoalOutcome::Succeeded {
            summary: "done".into(),
            extracted_data: serde_json::json!({ "price": 42 }),
        };
        let s = SessionState::from_outcome(
            "run-1",
            "buy milk",
            &succeeded,
            log.clone(),
            GoalMetrics::default(),
            7_000,
        );
        assert_eq!(s.status, SessionStatus::Completed(GoalStatus::Achieved));
        assert!(!s.is_resumable());
        assert_eq!(s.action_log.len(), 1);
        assert_eq!(s.created_at_ms, 7_000);

        let failed = GoalOutcome::Failed(cel_contracts::FailureReport {
            failing_sub_goal: "checkout".into(),
            failing_step: "click pay".into(),
            attempts: vec!["timeout".into()],
        });
        let f =
            SessionState::from_outcome("run-2", "g", &failed, vec![], GoalMetrics::default(), 1);
        assert_eq!(f.status, SessionStatus::Completed(GoalStatus::Failed));

        let refused = GoalOutcome::Refused {
            summary: "too vague".into(),
        };
        let r =
            SessionState::from_outcome("run-3", "g", &refused, vec![], GoalMetrics::default(), 1);
        assert_eq!(r.status, SessionStatus::Completed(GoalStatus::Refused));
    }
}
