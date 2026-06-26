//! [`ReceiptSource`] — surfaces a run's recent execution receipts into the brief.
//!
//! A runtime can append each run-scoped execution receipt to
//! `<runs_dir>/<run_id>.jsonl`. This source reads the last `limit` for a given
//! run and contributes a compact "recent actions" summary, so the next turn's
//! brief reflects what the agent just did and whether effects were observed.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::source::{Contribution, ContributionContent, Source, SourceError};
use crate::types::{BriefContext, Priority, SourceId};

/// Contributes a run's recent execution receipts as a compact System summary.
pub struct ReceiptSource {
    id: SourceId,
    runs_dir: PathBuf,
    run_id: String,
    limit: usize,
}

impl ReceiptSource {
    /// Read receipts for `run_id` from `runs_dir` (`<runs_dir>/<run_id>.jsonl`),
    /// surfacing the last `limit`.
    pub fn new(runs_dir: impl Into<PathBuf>, run_id: impl Into<String>, limit: usize) -> Self {
        Self {
            id: SourceId::new("receipts"),
            runs_dir: runs_dir.into(),
            run_id: run_id.into(),
            limit,
        }
    }

    /// Convenience constructor pointing at `~/.cel/brief/runs`.
    /// Returns `None` when `$HOME` is unset.
    pub fn for_run(run_id: impl Into<String>, limit: usize) -> Option<Self> {
        default_runs_dir().map(|dir| Self::new(dir, run_id, limit))
    }
}

#[async_trait]
impl Source for ReceiptSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::Normal
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        let receipts = read_recent(&self.runs_dir, &self.run_id, self.limit);
        if receipts.is_empty() {
            return Ok(Vec::new());
        }
        let mut text = format!("Recent actions this run ({}):", self.run_id);
        for r in &receipts {
            text.push('\n');
            text.push_str(&format_receipt(r));
        }
        let estimated_tokens = text.len() / 4;
        Ok(vec![Contribution {
            content: ContributionContent::System { text },
            estimated_tokens,
            importance: 0.6,
            redactable: true,
            tags: vec!["receipts".to_string(), "recent".to_string()],
        }])
    }
}

fn read_recent(runs_dir: &Path, run_id: &str, limit: usize) -> Vec<serde_json::Value> {
    let path = runs_dir.join(format!("{}.jsonl", sanitize_run_id(run_id)));
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut all: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if all.len() > limit {
        all = all.split_off(all.len() - limit);
    }
    all
}

fn format_receipt(r: &serde_json::Value) -> String {
    let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("?");
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
    format!(
        "- {} via {} → {} (effect: {}, {}ms) {}",
        s("action_kind"),
        route,
        s("status"),
        effect,
        dur,
        target
    )
}

fn default_runs_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cel").join("brief").join("runs"))
}

/// File-safe run id used by the JSONL receipt source.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BriefContext, TokenBudget};

    fn ctx() -> BriefContext {
        BriefContext::new(TokenBudget::new(1000, 0))
    }

    fn write_run(dir: &Path, run_id: &str, lines: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{run_id}.jsonl")), lines.join("\n")).unwrap();
    }

    #[tokio::test]
    async fn surfaces_recent_receipts() {
        let dir = std::env::temp_dir().join("cel_brief_receipt_src_a");
        let _ = std::fs::remove_dir_all(&dir);
        write_run(
            &dir,
            "run1",
            &[
                r#"{"receipt_id":"r1","action_kind":"set_value","route":{"route":"cdp"},"observed_effect":{"status":"observed"},"status":"ok","duration_ms":42,"target":"dom:input:x"}"#,
                r#"{"receipt_id":"r2","action_kind":"click","route":{"route":"cdp"},"observed_effect":{"status":"timed_out"},"status":"timed_out","duration_ms":2000,"target":"dom:button:y"}"#,
            ],
        );
        let src = ReceiptSource::new(&dir, "run1", 8);
        let out = src.contribute(&ctx()).await.unwrap();
        assert_eq!(out.len(), 1);
        let ContributionContent::System { text } = &out[0].content else {
            panic!("expected a system contribution");
        };
        assert!(text.contains("set_value via cdp → ok"));
        assert!(text.contains("click via cdp → timed_out"));
        assert!(text.contains("effect: observed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn empty_when_no_file() {
        let dir = std::env::temp_dir().join("cel_brief_receipt_src_missing");
        let _ = std::fs::remove_dir_all(&dir);
        let src = ReceiptSource::new(&dir, "nope", 8);
        assert!(src.contribute(&ctx()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn respects_limit() {
        let dir = std::env::temp_dir().join("cel_brief_receipt_src_limit");
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = (0..10)
            .map(|i| {
                format!(
                    r#"{{"action_kind":"click","route":{{"route":"cdp"}},"status":"ok","duration_ms":{i}}}"#
                )
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_run(&dir, "run2", &refs);
        let src = ReceiptSource::new(&dir, "run2", 3);
        let out = src.contribute(&ctx()).await.unwrap();
        let ContributionContent::System { text } = &out[0].content else {
            panic!("expected a system contribution");
        };
        // 1 header line + 3 receipt lines.
        assert_eq!(text.lines().count(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
