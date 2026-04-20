//! Checkpoint Manager — snapshot/restore for T3 replanning.
//!
//! When the strategy tracker exhausts all strategies for a milestone (T3),
//! the runner backtracks to the last checkpoint. The checkpoint contains
//! the notebook state at that point, allowing data recovery.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::notebook::NotebookEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub milestone: String,
    pub step_index: u32,
    pub notebook_snapshot: HashMap<String, NotebookEntry>,
    pub timestamp_ms: u64,
}

/// Manages checkpoints for T3 backtracking.
#[derive(Debug, Clone, Default)]
pub struct CheckpointManager {
    checkpoints: Vec<Checkpoint>,
    max_checkpoints: usize,
}

impl CheckpointManager {
    pub fn new() -> Self {
        Self { checkpoints: Vec::new(), max_checkpoints: 10 }
    }

    /// Save a checkpoint at the current milestone.
    pub fn save(
        &mut self,
        milestone: &str,
        step_index: u32,
        notebook_snapshot: HashMap<String, NotebookEntry>,
    ) {
        self.checkpoints.push(Checkpoint {
            milestone: milestone.into(),
            step_index,
            notebook_snapshot,
            timestamp_ms: now_ms(),
        });
        if self.checkpoints.len() > self.max_checkpoints {
            self.checkpoints.remove(0);
        }
    }

    /// Get the most recent checkpoint (for T3 backtracking).
    pub fn get_previous(&self) -> Option<&Checkpoint> {
        self.checkpoints.last()
    }

    /// Get checkpoint for a specific milestone.
    pub fn get_for_milestone(&self, milestone: &str) -> Option<&Checkpoint> {
        self.checkpoints.iter().rev()
            .find(|c| c.milestone == milestone)
    }

    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_retrieve() {
        let mut mgr = CheckpointManager::new();
        let mut snap = HashMap::new();
        snap.insert("price".into(), NotebookEntry {
            value: "$99".into(),
            category: "data".into(),
            step_recorded: 2,
        });
        mgr.save("search", 5, snap);

        let cp = mgr.get_previous().unwrap();
        assert_eq!(cp.milestone, "search");
        assert_eq!(cp.step_index, 5);
        assert_eq!(cp.notebook_snapshot.get("price").unwrap().value, "$99");
    }

    #[test]
    fn test_milestone_lookup() {
        let mut mgr = CheckpointManager::new();
        mgr.save("search", 3, HashMap::new());
        mgr.save("checkout", 8, HashMap::new());

        assert_eq!(mgr.get_for_milestone("search").unwrap().step_index, 3);
        assert_eq!(mgr.get_for_milestone("checkout").unwrap().step_index, 8);
        assert!(mgr.get_for_milestone("nonexistent").is_none());
    }

    #[test]
    fn test_max_checkpoints() {
        let mut mgr = CheckpointManager::new();
        for i in 0..15 {
            mgr.save(&format!("m{i}"), i as u32, HashMap::new());
        }
        assert_eq!(mgr.len(), 10); // capped at max
    }
}
