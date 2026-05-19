//! Watchlist type and an in-memory implementation for tests.
//!
//! The daemon persists watchlists in SQLite and implements `WatchlistLookup`
//! against that. This module provides the data shape and a test helper.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

use crate::matcher::WatchlistLookup;

/// A named list referenced by rules via `in_watchlist` / `not_in_watchlist`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Watchlist {
    /// Unique name (used as the value in `Operator::InWatchlist`).
    pub name: String,
    /// Optional description for the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Member items. Strings only in v1.
    pub items: BTreeSet<String>,
    /// Last modified.
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

/// In-memory `WatchlistLookup` for tests. Cheap and obvious.
#[derive(Debug, Default, Clone)]
pub struct InMemoryWatchlists {
    by_name: HashMap<String, BTreeSet<String>>,
}

impl InMemoryWatchlists {
    /// Replace a watchlist's items.
    pub fn set<I, S>(&mut self, name: &str, items: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set: BTreeSet<String> = items.into_iter().map(Into::into).collect();
        self.by_name.insert(name.to_string(), set);
    }

    /// Drop a watchlist.
    pub fn remove(&mut self, name: &str) -> bool {
        self.by_name.remove(name).is_some()
    }
}

impl WatchlistLookup for InMemoryWatchlists {
    fn contains(&self, watchlist_name: &str, item: &str) -> bool {
        self.by_name
            .get(watchlist_name)
            .is_some_and(|set| set.contains(item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_lookup() {
        let mut w = InMemoryWatchlists::default();
        w.set(
            "approved_apps",
            ["com.apple.Safari", "com.anthropic.claude"],
        );
        assert!(w.contains("approved_apps", "com.apple.Safari"));
        assert!(!w.contains("approved_apps", "com.example.malware"));
        assert!(!w.contains("nonexistent", "anything"));
    }

    #[test]
    fn remove() {
        let mut w = InMemoryWatchlists::default();
        w.set("x", ["a"]);
        assert!(w.remove("x"));
        assert!(!w.remove("x"));
        assert!(!w.contains("x", "a"));
    }
}
