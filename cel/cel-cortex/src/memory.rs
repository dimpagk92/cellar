//! Cross-goal rolling memory (Phase 3B).
//!
//! Append-only JSONL at `~/.cellar/memory/<machine_id>.jsonl`. Each entry
//! records the terminal state of a completed goal: what was attempted, on
//! which cortex, against which apps, and how it ended. The planner reads
//! three overlapping "lenses" on each new goal — same cortex, same
//! machine, similar goal_type — to give the LLM a short tail of relevant
//! prior attempts without dumping unfiltered history.
//!
//! Design trade-offs:
//! - Local file only — no network, no sync. User can delete freely.
//! - No key material or API values are persisted.
//! - Schema version `v: 1` on every entry so future changes can migrate.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const MEMORY_DIR_SEGMENT: &str = ".cellar/memory";
/// Hard cap on lines kept in the file; oldest lines get trimmed on next
/// append when this threshold is exceeded.
const MAX_RETAINED_ENTRIES: usize = 200;
/// Trim target once the cap is hit — we chop down to this to amortize.
const TRIM_TARGET: usize = 150;

/// A single completed-goal record.
///
/// `#[serde(default)]` on optional fields is intentional: we want the
/// reader to survive older log entries that might lack new fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Schema version. Hard-coded on write; reader checks on load.
    #[serde(default = "default_v")]
    pub v: u32,
    /// UNIX epoch milliseconds of goal completion.
    pub ts_ms: u64,
    /// Stable per-machine identifier. Not user-sensitive (local file).
    pub machine_id: String,
    /// Cortex instance ID at run time.
    pub cortex_id: String,
    /// Coarse goal classification (from `detect_task_type`). Used for the
    /// "similar tasks" lens.
    pub goal_type: String,
    /// Original natural-language goal. Bounded to 256 chars on write.
    pub goal: String,
    /// Terminal status — achieved / failed / max_steps / timeout / cancelled.
    pub status: String,
    /// Step count at goal end.
    pub steps: u32,
    /// Wall-clock duration from start to finish.
    pub duration_ms: u64,
    /// Last-known browser URL, if any. 128 chars max.
    #[serde(default)]
    pub last_url: Option<String>,
    /// Apps active during the run (top 3 by frequency).
    #[serde(default)]
    pub top_apps: Vec<String>,
    /// Terminal error / summary message when `status` is not `achieved`.
    #[serde(default)]
    pub last_error: Option<String>,
    /// The last CdpEval expression that preceded a successful `Done` on
    /// this run — effectively "the selector / JS that got the data the
    /// planner declared victory on". Stored so future goals on the same
    /// host can short-circuit selector discovery (eval smoke saw Gemini
    /// burn 7 LLM calls iterating through HN selectors before landing on
    /// `.titleline > a` — this field lets the next run start from there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_cdp_eval: Option<String>,
}

fn default_v() -> u32 {
    SCHEMA_VERSION
}

/// The three-lens retrospective the planner prompt receives.
#[derive(Debug, Clone, Default)]
pub struct MemoryLenses {
    /// Recent runs on the current cortex — highest relevance.
    pub same_cortex: Vec<MemoryEntry>,
    /// Recent runs on the current machine (any cortex).
    pub same_machine: Vec<MemoryEntry>,
    /// Runs with the same `goal_type` across all cortexes — pattern match.
    pub similar_goal: Vec<MemoryEntry>,
}

/// Memory errors. All are recoverable — the runner degrades to "no
/// memory available" if any operation fails, and logs a warning.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Could not determine HOME directory")]
    NoHome,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde error: {0}")]
    Json(#[from] serde_json::Error),
}

/// File-backed memory store. Construct with `Memory::open(cortex_id)`.
pub struct Memory {
    path: PathBuf,
    machine_id: String,
    cortex_id: String,
}

impl Memory {
    /// Open (or create) the memory store for this cortex, anchored at
    /// `$HOME/.cellar/memory/`. Inexpensive — does not read the file.
    pub fn open(cortex_id: impl Into<String>) -> Result<Self, MemoryError> {
        let home = std::env::var("HOME").map_err(|_| MemoryError::NoHome)?;
        let dir = PathBuf::from(home).join(MEMORY_DIR_SEGMENT);
        Self::open_in(dir, cortex_id)
    }

    /// Same as `open` but with an explicit base directory. Used in tests
    /// so each test can have its own isolated memory dir without racing
    /// on the process-wide `HOME` env var.
    pub fn open_in(dir: PathBuf, cortex_id: impl Into<String>) -> Result<Self, MemoryError> {
        let machine_id = stable_machine_id();
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
        let filename = format!("{}.jsonl", machine_id);
        let path = dir.join(filename);
        Ok(Self {
            path,
            machine_id,
            cortex_id: cortex_id.into(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// Append an entry. Auto-trims the file if it exceeds the retention
    /// cap. `v`, `ts_ms`, `machine_id`, `cortex_id` are filled in here so
    /// callers don't have to — they pass the interesting fields.
    pub fn append(&self, mut entry: MemoryEntry) -> Result<(), MemoryError> {
        entry.v = SCHEMA_VERSION;
        if entry.ts_ms == 0 {
            entry.ts_ms = now_ms();
        }
        if entry.machine_id.is_empty() {
            entry.machine_id = self.machine_id.clone();
        }
        if entry.cortex_id.is_empty() {
            entry.cortex_id = self.cortex_id.clone();
        }
        // Bound field sizes so a pathological goal string can't bloat the
        // file. We truncate on char boundaries so unicode stays valid.
        entry.goal = bound_chars(&entry.goal, 256);
        if let Some(ref mut url) = entry.last_url {
            *url = bound_chars(url, 128);
        }
        if let Some(ref mut err) = entry.last_error {
            *err = bound_chars(err, 256);
        }
        if let Some(ref mut eval) = entry.winning_cdp_eval {
            // CdpEval expressions tend to be one-liners; 512 chars is plenty
            // for typical selectors + data-extraction JS. Anything longer
            // is probably signal the LLM won't benefit from re-using.
            *eval = bound_chars(eval, 512);
        }

        let line = serde_json::to_string(&entry)?;
        {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            writeln!(f, "{}", line)?;
        }

        // Amortized trim: on each append we check line count. Checking is
        // cheap (a single read) and only happens once per goal completion.
        self.trim_if_oversize()?;
        Ok(())
    }

    /// Load all entries (newest last). Corrupt lines are skipped with a
    /// single warning per call.
    pub fn all(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let f = fs::File::open(&self.path)?;
        let mut entries = Vec::new();
        let mut corrupt = 0usize;
        for line in BufReader::new(f).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => {
                    corrupt += 1;
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MemoryEntry>(&line) {
                Ok(e) if e.v == SCHEMA_VERSION => entries.push(e),
                Ok(_) => {
                    // Future-version entry we don't know how to read yet.
                    corrupt += 1;
                }
                Err(_) => {
                    corrupt += 1;
                }
            }
        }
        if corrupt > 0 {
            tracing::warn!(
                path = %self.path.display(),
                count = corrupt,
                "Skipped {} unreadable memory lines", corrupt
            );
        }
        Ok(entries)
    }

    /// Build the three lenses for a given cortex + goal_type. Each lens
    /// is sorted newest-first and capped at `per_lens`. The same entry
    /// may appear in multiple lenses — that's intentional; the prompt
    /// renderer deduplicates by line-id if it wants to.
    pub fn lens(&self, goal_type: &str, per_lens: usize) -> Result<MemoryLenses, MemoryError> {
        let mut all = self.all()?;
        all.sort_by_key(|a| std::cmp::Reverse(a.ts_ms));

        let same_cortex: Vec<_> = all
            .iter()
            .filter(|e| e.cortex_id == self.cortex_id)
            .take(per_lens)
            .cloned()
            .collect();

        let same_machine: Vec<_> = all
            .iter()
            .filter(|e| e.machine_id == self.machine_id && e.cortex_id != self.cortex_id)
            .take(per_lens)
            .cloned()
            .collect();

        let similar_goal: Vec<_> = all
            .iter()
            .filter(|e| {
                e.goal_type.eq_ignore_ascii_case(goal_type) && e.cortex_id != self.cortex_id
            })
            .take(per_lens)
            .cloned()
            .collect();

        Ok(MemoryLenses {
            same_cortex,
            same_machine,
            similar_goal,
        })
    }

    fn trim_if_oversize(&self) -> Result<(), MemoryError> {
        let entries = self.all()?;
        if entries.len() <= MAX_RETAINED_ENTRIES {
            return Ok(());
        }
        // Keep newest TRIM_TARGET entries. JSONL order is append order
        // (oldest first), so newest live at the end — slice from the tail.
        let keep_from = entries.len() - TRIM_TARGET.min(entries.len());
        let keep: Vec<&MemoryEntry> = entries[keep_from..].iter().collect();

        // Write to tmp then rename so we never leave a torn file.
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = fs::File::create(&tmp)?;
            for e in keep {
                let line = serde_json::to_string(e)?;
                writeln!(out, "{}", line)?;
            }
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn stable_machine_id() -> String {
    // `hostname` is available on both macOS and Linux, doesn't require
    // root, and returns within a few ms. Fallback is a constant so the
    // memory file still has a deterministic name in headless / sandbox
    // environments.
    let raw = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown-host".to_string());
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    if cleaned.is_empty() {
        "unknown-host".to_string()
    } else {
        cleaned
    }
}

fn bound_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
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
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Monotonic counter so parallel tests don't collide on directory names.
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_dir(name: &str) -> PathBuf {
        let seq = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("cel-memory-test-{}-{}-{}", name, now_ms(), seq));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn sample(cortex_id: &str, goal: &str, goal_type: &str) -> MemoryEntry {
        MemoryEntry {
            v: 0,
            ts_ms: 0,
            machine_id: String::new(),
            cortex_id: cortex_id.into(),
            goal_type: goal_type.into(),
            goal: goal.into(),
            status: "achieved".into(),
            steps: 3,
            duration_ms: 500,
            last_url: None,
            top_apps: vec![],
            last_error: None,
            winning_cdp_eval: None,
        }
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = tmp_dir("round-trip");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        mem.append(sample("c1", "hello", "navigation")).unwrap();
        let all = mem.all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].goal, "hello");
        assert_eq!(all[0].v, SCHEMA_VERSION);
        assert!(all[0].ts_ms > 0);
        assert!(!all[0].machine_id.is_empty());
        cleanup(&dir);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tmp_dir("corrupt");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        mem.append(sample("c1", "real", "navigation")).unwrap();

        let path = dir.join(format!("{}.jsonl", mem.machine_id()));
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{not-json-at-all}}").unwrap();
        writeln!(f).unwrap(); // blank line should be OK
        drop(f);

        let all = mem.all().unwrap();
        assert_eq!(all.len(), 1, "only the real entry should survive");
        assert_eq!(all[0].goal, "real");
        cleanup(&dir);
    }

    #[test]
    fn lenses_split_correctly() {
        let dir = tmp_dir("lenses");
        let mem1 = Memory::open_in(dir.clone(), "c1").unwrap();
        mem1.append(sample("c1", "nav1", "navigation")).unwrap();
        mem1.append(sample("c1", "nav2", "navigation")).unwrap();
        mem1.append(sample("c2", "extract1", "extraction")).unwrap();
        mem1.append(sample("c2", "nav3", "navigation")).unwrap();

        let lenses = mem1.lens("navigation", 5).unwrap();
        assert_eq!(lenses.same_cortex.len(), 2);
        assert!(lenses.same_cortex.iter().all(|e| e.cortex_id == "c1"));
        // same_machine excludes same_cortex entries — c2 runs only.
        assert_eq!(lenses.same_machine.len(), 2);
        assert!(lenses.same_machine.iter().all(|e| e.cortex_id == "c2"));
        // similar_goal excludes current cortex — c2's navigation run.
        assert_eq!(lenses.similar_goal.len(), 1);
        assert_eq!(lenses.similar_goal[0].goal, "nav3");
        cleanup(&dir);
    }

    #[test]
    fn goal_is_truncated_on_write() {
        let dir = tmp_dir("truncate");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        let long_goal = "a".repeat(1000);
        mem.append(sample("c1", &long_goal, "navigation")).unwrap();
        let all = mem.all().unwrap();
        assert_eq!(all[0].goal.chars().count(), 256);
        cleanup(&dir);
    }

    #[test]
    fn winning_cdp_eval_round_trips() {
        let dir = tmp_dir("winning-eval");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        let mut entry = sample("c1", "extract headlines", "extraction");
        entry.winning_cdp_eval =
            Some("Array.from(document.querySelectorAll('.titleline > a')).slice(0,5)".into());
        mem.append(entry).unwrap();

        let all = mem.all().unwrap();
        assert_eq!(all.len(), 1);
        let e = &all[0];
        assert_eq!(
            e.winning_cdp_eval.as_deref(),
            Some("Array.from(document.querySelectorAll('.titleline > a')).slice(0,5)")
        );
        cleanup(&dir);
    }

    #[test]
    fn winning_cdp_eval_is_bounded() {
        let dir = tmp_dir("bound-eval");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        let mut entry = sample("c1", "g", "navigation");
        entry.winning_cdp_eval = Some("x".repeat(2000));
        mem.append(entry).unwrap();
        let all = mem.all().unwrap();
        assert_eq!(
            all[0].winning_cdp_eval.as_ref().unwrap().chars().count(),
            512
        );
        cleanup(&dir);
    }

    #[test]
    fn missing_winning_cdp_eval_serializes_without_field() {
        // Forward-compat: older code that reads the file shouldn't have
        // to deal with a `winning_cdp_eval: null` blob every line.
        // serde's skip_serializing_if elides the key entirely when None.
        let dir = tmp_dir("omit-eval");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        mem.append(sample("c1", "g", "navigation")).unwrap();
        let path = dir.join(format!("{}.jsonl", mem.machine_id()));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("winning_cdp_eval"),
            "None-valued winning_cdp_eval should be elided from JSONL, got:\n{raw}"
        );
        cleanup(&dir);
    }

    #[test]
    fn trim_caps_retained_entries() {
        let dir = tmp_dir("trim");
        let mem = Memory::open_in(dir.clone(), "c1").unwrap();
        for i in 0..(MAX_RETAINED_ENTRIES + 10) {
            mem.append(sample("c1", &format!("g{i}"), "navigation"))
                .unwrap();
        }
        let all = mem.all().unwrap();
        assert!(
            all.len() <= MAX_RETAINED_ENTRIES,
            "file must respect retention cap, got {}",
            all.len()
        );
        cleanup(&dir);
    }
}
