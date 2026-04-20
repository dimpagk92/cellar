//! Cognitive Trail — narrative log of runner decisions.
//!
//! Records why the runner made each decision (plan choice, replan trigger,
//! checkpoint restore, etc.). Used for debugging and replanning context.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrailEntry {
    pub step: u32,
    pub phase: String,
    pub message: String,
    pub timestamp_ms: u64,
}

/// Narrative decision log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CognitiveTrail {
    entries: Vec<TrailEntry>,
    max_entries: usize,
}

impl CognitiveTrail {
    pub fn new() -> Self {
        Self { entries: Vec::new(), max_entries: 100 }
    }

    /// Add a trail entry.
    pub fn add(&mut self, step: u32, phase: &str, message: &str) {
        self.entries.push(TrailEntry {
            step,
            phase: phase.into(),
            message: message.into(),
            timestamp_ms: now_ms(),
        });
        if self.entries.len() > self.max_entries {
            self.entries.drain(0..self.entries.len() - self.max_entries);
        }
    }

    /// Get recent entries as a formatted string for injection into replan prompts.
    pub fn to_prompt_context(&self, last_n: usize) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let recent: Vec<&TrailEntry> = self.entries.iter().rev().take(last_n).collect();
        let mut lines = vec!["## Decision Trail (recent)".to_string()];
        for entry in recent.iter().rev() {
            lines.push(format!("  Step {}: [{}] {}", entry.step, entry.phase, entry.message));
        }
        lines.join("\n")
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
