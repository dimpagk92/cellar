//! Filesystem storage for screenshots, run transcripts, and binary data.
//!
//! Layout:
//!   ~/.cellar/
//!     captures/{run_id}/{step_index}.png       — per-step screenshots
//!     runs/{run_id}/transcript.jsonl            — append-only run transcript
//!     workflows/{name}.json                     — workflow definitions
//!     cel-store.db                              — SQLite database

use crate::StoreError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Filesystem-backed storage for binary data and transcripts.
pub struct FsStore {
    base_dir: PathBuf,
}

/// A single entry in a JSONL run transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub timestamp_ms: u64,
    pub entry_type: TranscriptEntryType,
    pub step_index: Option<u32>,
    pub step_id: Option<String>,
    pub data: serde_json::Value,
}

/// Types of transcript entries (inspired by OpenClaw's JSONL format).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptEntryType {
    /// Run started
    RunStart,
    /// Context captured before a step
    ContextCapture,
    /// Action executed
    ActionExecuted,
    /// Step completed successfully
    StepComplete,
    /// Step failed
    StepFailed,
    /// Agent paused (low confidence)
    Paused,
    /// User intervention
    Intervention,
    /// Run completed
    RunComplete,
    /// Observation generated from this run
    ObservationGenerated,
}

impl FsStore {
    /// Create a new FsStore at the given base directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Create with the default ~/.cellar directory.
    pub fn default_dir() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Self::new(Path::new(&home).join(".cellar"))
    }

    /// Ensure the directory structure exists.
    pub fn init(&self) -> Result<(), StoreError> {
        fs::create_dir_all(self.base_dir.join("captures"))?;
        fs::create_dir_all(self.base_dir.join("runs"))?;
        fs::create_dir_all(self.base_dir.join("workflows"))?;
        Ok(())
    }

    /// Get the base directory path.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Get the path to the SQLite database.
    pub fn db_path(&self) -> PathBuf {
        self.base_dir.join("cel-store.db")
    }

    // --- Screenshots ---

    /// Save a screenshot PNG for a specific run step.
    /// Returns the path where it was saved.
    pub fn save_screenshot(
        &self,
        run_id: i64,
        step_index: u32,
        png_data: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let dir = self.base_dir.join("captures").join(run_id.to_string());
        fs::create_dir_all(&dir)?;

        let path = dir.join(format!("{}.png", step_index));
        let mut file = fs::File::create(&path)?;
        file.write_all(png_data)?;
        Ok(path)
    }

    /// Load a screenshot for a specific run step.
    pub fn load_screenshot(
        &self,
        run_id: i64,
        step_index: u32,
    ) -> Result<Vec<u8>, StoreError> {
        let path = self
            .base_dir
            .join("captures")
            .join(run_id.to_string())
            .join(format!("{}.png", step_index));
        if !path.exists() {
            return Err(StoreError::NotFound(format!(
                "Screenshot not found: run {} step {}",
                run_id, step_index
            )));
        }
        Ok(fs::read(&path)?)
    }

    /// List all screenshots for a run.
    pub fn list_screenshots(&self, run_id: i64) -> Result<Vec<u32>, StoreError> {
        let dir = self.base_dir.join("captures").join(run_id.to_string());
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut steps = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Some(stem) = name.strip_suffix(".png") {
                    if let Ok(idx) = stem.parse::<u32>() {
                        steps.push(idx);
                    }
                }
            }
        }
        steps.sort();
        Ok(steps)
    }

    // --- JSONL Run Transcripts ---

    /// Append an entry to the run transcript.
    pub fn append_transcript(
        &self,
        run_id: i64,
        entry: &TranscriptEntry,
    ) -> Result<(), StoreError> {
        let dir = self.base_dir.join("runs").join(run_id.to_string());
        fs::create_dir_all(&dir)?;

        let path = dir.join("transcript.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let json = serde_json::to_string(entry)?;
        writeln!(file, "{}", json)?;
        Ok(())
    }

    /// Read all entries from a run transcript.
    pub fn read_transcript(&self, run_id: i64) -> Result<Vec<TranscriptEntry>, StoreError> {
        let path = self
            .base_dir
            .join("runs")
            .join(run_id.to_string())
            .join("transcript.jsonl");
        if !path.exists() {
            return Ok(vec![]);
        }

        let content = fs::read_to_string(&path)?;
        let mut entries = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: TranscriptEntry = serde_json::from_str(line)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Get the size of a run's transcript in bytes.
    pub fn transcript_size(&self, run_id: i64) -> Result<u64, StoreError> {
        let path = self
            .base_dir
            .join("runs")
            .join(run_id.to_string())
            .join("transcript.jsonl");
        if !path.exists() {
            return Ok(0);
        }
        Ok(fs::metadata(&path)?.len())
    }

    /// Delete all data for a run (screenshots + transcript).
    pub fn delete_run_data(&self, run_id: i64) -> Result<(), StoreError> {
        let captures_dir = self.base_dir.join("captures").join(run_id.to_string());
        if captures_dir.exists() {
            fs::remove_dir_all(&captures_dir)?;
        }
        let runs_dir = self.base_dir.join("runs").join(run_id.to_string());
        if runs_dir.exists() {
            fs::remove_dir_all(&runs_dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (FsStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path());
        store.init().unwrap();
        (store, dir)
    }

    #[test]
    fn test_init_creates_directories() {
        let (store, _dir) = temp_store();
        assert!(store.base_dir.join("captures").exists());
        assert!(store.base_dir.join("runs").exists());
        assert!(store.base_dir.join("workflows").exists());
    }

    #[test]
    fn test_screenshot_save_and_load() {
        let (store, _dir) = temp_store();
        let png_data = b"\x89PNG\r\n\x1a\nfake_png_data";
        let path = store.save_screenshot(1, 0, png_data).unwrap();
        assert!(path.exists());

        let loaded = store.load_screenshot(1, 0).unwrap();
        assert_eq!(loaded, png_data);
    }

    #[test]
    fn test_screenshot_not_found() {
        let (store, _dir) = temp_store();
        let result = store.load_screenshot(999, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_screenshots() {
        let (store, _dir) = temp_store();
        store.save_screenshot(1, 0, b"a").unwrap();
        store.save_screenshot(1, 2, b"b").unwrap();
        store.save_screenshot(1, 1, b"c").unwrap();

        let steps = store.list_screenshots(1).unwrap();
        assert_eq!(steps, vec![0, 1, 2]);
    }

    #[test]
    fn test_list_screenshots_empty_run() {
        let (store, _dir) = temp_store();
        let steps = store.list_screenshots(999).unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn test_transcript_append_and_read() {
        let (store, _dir) = temp_store();
        let entry1 = TranscriptEntry {
            timestamp_ms: 1000,
            entry_type: TranscriptEntryType::RunStart,
            step_index: None,
            step_id: None,
            data: serde_json::json!({"workflow": "daily-po", "steps": 5}),
        };
        let entry2 = TranscriptEntry {
            timestamp_ms: 1500,
            entry_type: TranscriptEntryType::ContextCapture,
            step_index: Some(0),
            step_id: Some("step-1".into()),
            data: serde_json::json!({"app": "Excel", "elements": 12}),
        };

        store.append_transcript(1, &entry1).unwrap();
        store.append_transcript(1, &entry2).unwrap();

        let entries = store.read_transcript(1).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].entry_type, TranscriptEntryType::RunStart));
        assert_eq!(entries[1].step_index, Some(0));
    }

    #[test]
    fn test_transcript_empty_run() {
        let (store, _dir) = temp_store();
        let entries = store.read_transcript(999).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_transcript_size() {
        let (store, _dir) = temp_store();
        assert_eq!(store.transcript_size(1).unwrap(), 0);

        let entry = TranscriptEntry {
            timestamp_ms: 1000,
            entry_type: TranscriptEntryType::RunStart,
            step_index: None,
            step_id: None,
            data: serde_json::json!({}),
        };
        store.append_transcript(1, &entry).unwrap();
        assert!(store.transcript_size(1).unwrap() > 0);
    }

    #[test]
    fn test_delete_run_data() {
        let (store, _dir) = temp_store();
        store.save_screenshot(1, 0, b"png").unwrap();
        let entry = TranscriptEntry {
            timestamp_ms: 1000,
            entry_type: TranscriptEntryType::RunStart,
            step_index: None,
            step_id: None,
            data: serde_json::json!({}),
        };
        store.append_transcript(1, &entry).unwrap();

        store.delete_run_data(1).unwrap();
        assert!(store.list_screenshots(1).unwrap().is_empty());
        assert!(store.read_transcript(1).unwrap().is_empty());
    }

    #[test]
    fn test_db_path() {
        let (store, _dir) = temp_store();
        assert!(store.db_path().to_str().unwrap().ends_with("cel-store.db"));
    }

    #[test]
    fn test_transcript_entry_serialization() {
        let entry = TranscriptEntry {
            timestamp_ms: 12345,
            entry_type: TranscriptEntryType::ActionExecuted,
            step_index: Some(3),
            step_id: Some("click-submit".into()),
            data: serde_json::json!({"type": "click", "x": 100, "y": 200}),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: TranscriptEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timestamp_ms, 12345);
        assert_eq!(back.step_index, Some(3));
    }
}
