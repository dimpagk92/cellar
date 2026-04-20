//! Notebook — key-value store that survives replans.
//!
//! Records data discovered during execution (prices, URLs, confirmation numbers).
//! Persists across T2/T3 replanning tiers. Restored from checkpoints on T3 backtrack.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Persistent notebook for cross-replan data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Notebook {
    entries: HashMap<String, NotebookEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookEntry {
    pub value: String,
    pub category: String,
    pub step_recorded: u32,
}

impl Notebook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a key-value pair to the notebook.
    pub fn write(&mut self, key: &str, value: &str, category: &str, step: u32) {
        self.entries.insert(key.to_string(), NotebookEntry {
            value: value.to_string(),
            category: category.to_string(),
            step_recorded: step,
        });
    }

    /// Read a value from the notebook.
    pub fn read(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|e| e.value.as_str())
    }

    /// Get all entries as a formatted string for injection into prompts.
    pub fn to_prompt_context(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut lines = vec!["## Notebook (data discovered so far)".to_string()];
        for (key, entry) in &self.entries {
            lines.push(format!("- {key}: {} [{}]", entry.value, entry.category));
        }
        lines.join("\n")
    }

    /// Snapshot for checkpoint restore.
    pub fn snapshot(&self) -> HashMap<String, NotebookEntry> {
        self.entries.clone()
    }

    /// Restore from a checkpoint snapshot.
    pub fn restore(&mut self, snapshot: HashMap<String, NotebookEntry>) {
        self.entries = snapshot;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_read() {
        let mut nb = Notebook::new();
        nb.write("price", "$149", "data", 3);
        assert_eq!(nb.read("price"), Some("$149"));
        assert_eq!(nb.len(), 1);
    }

    #[test]
    fn test_overwrite() {
        let mut nb = Notebook::new();
        nb.write("price", "$149", "data", 3);
        nb.write("price", "$129", "data", 5);
        assert_eq!(nb.read("price"), Some("$129"));
    }

    #[test]
    fn test_snapshot_restore() {
        let mut nb = Notebook::new();
        nb.write("url", "https://example.com", "nav", 1);
        nb.write("price", "$99", "data", 2);

        let snap = nb.snapshot();
        nb.write("extra", "gone", "data", 3);
        assert_eq!(nb.len(), 3);

        nb.restore(snap);
        assert_eq!(nb.len(), 2);
        assert!(nb.read("extra").is_none());
        assert_eq!(nb.read("price"), Some("$99"));
    }

    #[test]
    fn test_prompt_context() {
        let mut nb = Notebook::new();
        nb.write("hotel", "Marriott Amsterdam", "data", 1);
        let ctx = nb.to_prompt_context();
        assert!(ctx.contains("Notebook"));
        assert!(ctx.contains("Marriott Amsterdam"));
    }
}
