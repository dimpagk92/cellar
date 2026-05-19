//! `watchlists.*` request parameters.

use serde::{Deserialize, Serialize};

/// Params for `watchlists.list`. Currently empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistsListParams {}

/// Params for `watchlists.get` and `watchlists.remove`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistNameParams {
    /// Watchlist name.
    pub name: String,
}

/// Params for `watchlists.set` — replace the whole list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistsSetParams {
    /// Watchlist name.
    pub name: String,
    /// Replacement item list.
    pub items: Vec<String>,
}

/// Params for `watchlists.add_item` and `watchlists.remove_item`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistsItemParams {
    /// Watchlist name.
    pub name: String,
    /// The single item to add or remove.
    pub item: String,
}
