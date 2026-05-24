//! `watchlists.*` response types.

use cellar_types::Watchlist;
use serde::{Deserialize, Serialize};

/// Result of `watchlists.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistsListResult {
    /// All watchlists, alphabetically.
    pub watchlists: Vec<Watchlist>,
}

/// Result of `watchlists.get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistsGetResult {
    /// The watchlist, or `None` if not found.
    pub watchlist: Option<Watchlist>,
}
