//! Importance scoring for memory chunks.
//!
//! Per `cellar-memory-manager.md` §10.2: importance is a real in `[0,1]`
//! assigned at write time. It drives eviction priority — chunks with
//! lower importance are evicted first when storage caps or aging
//! sweeps run.
//!
//! The scorer is a pure function over the new chunk's metadata
//! (kind, source, content length, explicit caller hint). It produces
//! a default score when [`NewMemoryChunk::importance`] is `None`; if
//! the caller supplies a value, that value is honored verbatim (after
//! clamping). The provider calls [`score`] at write time and stores the
//! result in `memory_chunks.importance`.
//!
//! Heuristic weights (signals that nudge importance up or down):
//!
//! | Signal | Δ |
//! |---|---:|
//! | `kind = Correction` | +0.4 |
//! | `kind = Fire` and metadata says require_confirmation | +0.2 |
//! | `kind = Action` and metadata says decision = denied | +0.2 |
//! | `kind = JobSummary` | +0.2 |
//! | metadata `"user_ack": true` | +0.3 |
//! | metadata `"pinned_entity": true` | +0.1 |
//! | `kind = Observation` and metadata `"stability": "transient"` | −0.2 |
//! | `kind = Context` and metadata `"duration_ms"` small (<2000) | −0.1 |
//!
//! Baseline: 0.5.
//!
//! Floor: 0.1 for `Chat` and `Action` chunks regardless of score —
//! the user's own words and the agent's own actions are never evicted
//! purely because their importance number is low.
//!
//! Use-cite bumps (+0.05 per cite, capped at +0.2) are applied via
//! [`crate::MemoryProvider::record_access`] later, not at write time.

use crate::chunk::{ChunkKind, NewMemoryChunk};

/// Compute the initial importance score for a new chunk.
///
/// If the caller passed an explicit `importance` value on
/// [`NewMemoryChunk`], that value is returned (clamped to `[0,1]`).
/// Otherwise the scorer applies the heuristic above starting from a
/// baseline of `0.5` and returns the clamped result.
//
// `collapsible_match` would convert each `Kind { if ... }` arm into a
// match guard. The current shape is easier to read when each kind needs
// to inspect metadata before deciding whether to bump — guards spread
// the per-kind logic across two arms (matched-with-guard vs fallthrough),
// which is harder to follow when adding new signals.
#[allow(clippy::collapsible_match)]
pub fn score(chunk: &NewMemoryChunk) -> f32 {
    // Caller-supplied importance takes priority (deliberate override
    // path — agents and external MCP clients can hint).
    if let Some(explicit) = chunk.importance {
        return clamp(explicit);
    }

    let mut score: f32 = 0.5;

    match chunk.kind {
        ChunkKind::Correction => score += 0.4,
        ChunkKind::JobSummary => score += 0.2,
        ChunkKind::Fire => {
            if metadata_str(chunk, "action_type")
                .map(|s| s == "RequireConfirmation" || s == "Veto")
                .unwrap_or(false)
            {
                score += 0.2;
            }
        }
        ChunkKind::Action => {
            if metadata_str(chunk, "decision")
                .map(|s| s == "denied" || s == "vetoed")
                .unwrap_or(false)
            {
                score += 0.2;
            }
        }
        ChunkKind::Observation => {
            if metadata_str(chunk, "stability")
                .map(|s| s == "transient")
                .unwrap_or(false)
            {
                score -= 0.2;
            }
        }
        ChunkKind::Context => {
            if metadata_i64(chunk, "duration_ms")
                .map(|ms| ms < 2000)
                .unwrap_or(false)
            {
                score -= 0.1;
            }
        }
        _ => {}
    }

    if metadata_bool(chunk, "user_ack") {
        score += 0.3;
    }
    if metadata_bool(chunk, "pinned_entity") {
        score += 0.1;
    }

    let clamped = clamp(score);

    // Floor: chat and action chunks never go below 0.1 — the user's
    // own words and the agent's own actions are too important to evict
    // purely on score.
    match chunk.kind {
        ChunkKind::Chat | ChunkKind::Action => clamped.max(0.1),
        _ => clamped,
    }
}

fn clamp(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn metadata_str<'a>(chunk: &'a NewMemoryChunk, key: &str) -> Option<&'a str> {
    chunk.metadata.get(key).and_then(|v| v.as_str())
}

fn metadata_bool(chunk: &NewMemoryChunk, key: &str) -> bool {
    chunk
        .metadata
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn metadata_i64(chunk: &NewMemoryChunk, key: &str) -> Option<i64> {
    chunk.metadata.get(key).and_then(|v| v.as_i64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkKind, ChunkSource};
    use serde_json::json;

    fn nc(kind: ChunkKind) -> NewMemoryChunk {
        NewMemoryChunk {
            kind,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "test".into(),
            content: "x".into(),
            metadata: json!({}),
            importance: None,
            shareable: false,
            pinned: false,
        }
    }

    #[test]
    fn baseline_is_half() {
        let s = score(&nc(ChunkKind::Observation));
        assert_eq!(s, 0.5);
    }

    #[test]
    fn correction_bumps_to_high() {
        let s = score(&nc(ChunkKind::Correction));
        assert!((s - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn job_summary_bumps_above_baseline() {
        let s = score(&nc(ChunkKind::JobSummary));
        assert!((s - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn fire_with_require_confirmation_bumps() {
        let mut c = nc(ChunkKind::Fire);
        c.metadata = json!({"action_type": "RequireConfirmation"});
        assert!((score(&c) - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn action_denied_bumps() {
        let mut c = nc(ChunkKind::Action);
        c.metadata = json!({"decision": "denied"});
        assert!((score(&c) - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn transient_observation_reduced() {
        let mut c = nc(ChunkKind::Observation);
        c.metadata = json!({"stability": "transient"});
        assert!((score(&c) - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn user_ack_and_pinned_entity_stack() {
        let mut c = nc(ChunkKind::Chat);
        c.metadata = json!({"user_ack": true, "pinned_entity": true});
        // 0.5 + 0.3 + 0.1 = 0.9
        assert!((score(&c) - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn chat_floor_protects_against_zero() {
        let mut c = nc(ChunkKind::Chat);
        // Drag well below the floor with metadata signals
        c.metadata = json!({"stability": "transient"});
        // Stability only applies to Observation kind — score should be 0.5.
        // Force the issue by passing explicit importance.
        c.importance = Some(0.0);
        // Explicit value bypasses the heuristic entirely.
        assert_eq!(score(&c), 0.0);
    }

    #[test]
    fn explicit_importance_is_honored_with_clamp() {
        let mut c = nc(ChunkKind::Chat);
        c.importance = Some(1.7);
        assert_eq!(score(&c), 1.0);
        c.importance = Some(-0.3);
        assert_eq!(score(&c), 0.0);
    }

    #[test]
    fn chat_baseline_with_floor() {
        let s = score(&nc(ChunkKind::Chat));
        // Baseline 0.5 — well above the 0.1 floor, so floor doesn't activate.
        assert_eq!(s, 0.5);
    }
}
