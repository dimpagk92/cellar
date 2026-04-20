//! Fuzzy-dedup cache for accessibility tree snapshots.
//!
//! Pattern adapted from screenpipe-a11y (MIT). Keyed on (app_name,
//! window_title). Stores the last SimHash plus a TTL so identical content
//! is re-stored once per minute and near-identical content (Hamming ≤ 10)
//! is skipped.
//!
//! Use in continuous-perception loops to avoid piling up duplicate
//! snapshots during scroll/idle.

use crate::simhash::hamming_distance;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 100;
const DEFAULT_TTL: Duration = Duration::from_secs(60);
/// Hamming distance threshold below which two snapshots count as duplicates.
const SIMHASH_THRESHOLD: u32 = 10;

struct Entry {
    simhash: u64,
    last_stored: Instant,
}

pub struct SnapshotCache {
    entries: HashMap<(String, String), Entry>,
    ttl: Duration,
    threshold: u32,
}

impl SnapshotCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            ttl: DEFAULT_TTL,
            threshold: SIMHASH_THRESHOLD,
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
            threshold: SIMHASH_THRESHOLD,
        }
    }

    pub fn with_threshold(mut self, threshold: u32) -> Self {
        self.threshold = threshold;
        self
    }

    pub fn should_store(&self, app: &str, window: &str, simhash: u64) -> bool {
        let key = (app.to_string(), window.to_string());
        match self.entries.get(&key) {
            Some(e) => {
                hamming_distance(e.simhash, simhash) > self.threshold
                    || e.last_stored.elapsed() >= self.ttl
            }
            None => true,
        }
    }

    pub fn record(&mut self, app: &str, window: &str, simhash: u64) {
        let key = (app.to_string(), window.to_string());
        self.entries.insert(
            key,
            Entry {
                simhash,
                last_stored: Instant::now(),
            },
        );
        if self.entries.len() > MAX_ENTRIES {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(k) = self
            .entries
            .iter()
            .min_by_key(|(_, v)| v.last_stored)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&k);
        }
    }
}

impl Default for SnapshotCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simhash::simhash;

    #[test]
    fn dedup_identical_snapshot() {
        let mut c = SnapshotCache::new();
        let h = simhash("hello world foo bar baz");
        assert!(c.should_store("Chrome", "Tab 1", h));
        c.record("Chrome", "Tab 1", h);
        assert!(!c.should_store("Chrome", "Tab 1", h));
    }

    #[test]
    fn dedup_fuzzy_rejects_near_duplicate() {
        let mut c = SnapshotCache::new();
        let base = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu \
                    xi omicron pi rho sigma tau upsilon phi chi psi omega";
        let near = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu \
                    xi omicron pi rho sigma tau upsilon phi chi psi finalword";
        c.record("App", "Win", simhash(base));
        assert!(!c.should_store("App", "Win", simhash(near)));
    }

    #[test]
    fn accepts_very_different_content() {
        let mut c = SnapshotCache::new();
        c.record(
            "App",
            "Win",
            simhash(
                "the cat sat on the mat while the dog ran outside chasing squirrels in \
                 the yard under the bright afternoon sun",
            ),
        );
        let other = simhash(
            "database transactions ensure atomicity consistency isolation durability \
             across concurrent access patterns in distributed systems worldwide",
        );
        assert!(c.should_store("App", "Win", other));
    }

    #[test]
    fn ttl_forces_refresh_of_identical_content() {
        let mut c = SnapshotCache::with_ttl(Duration::from_millis(0));
        let h = simhash("hello");
        c.record("App", "Win", h);
        std::thread::sleep(Duration::from_millis(1));
        assert!(c.should_store("App", "Win", h));
    }

    #[test]
    fn eviction_caps_memory() {
        let mut c = SnapshotCache::new();
        for i in 0..=MAX_ENTRIES + 5 {
            c.record(&format!("App{i}"), "Win", i as u64);
        }
        assert!(c.entries.len() <= MAX_ENTRIES + 1);
    }

    #[test]
    fn different_windows_tracked_independently() {
        let mut c = SnapshotCache::new();
        let h = simhash("same content here");
        c.record("Chrome", "Tab 1", h);
        assert!(c.should_store("Chrome", "Tab 2", h));
    }
}
