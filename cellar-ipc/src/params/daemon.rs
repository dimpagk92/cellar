//! `daemon.*` request parameters.
//!
//! `daemon.status` has no params; included here as a marker for symmetry.

use serde::{Deserialize, Serialize};

/// Params for `daemon.status`. Currently empty; the response carries
/// everything (see [`crate::results::daemon::DaemonStatusResult`]).
///
/// Represented as an empty struct (not a unit struct) so the JSON wire form
/// `{}` round-trips cleanly through serde.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatusParams {}
