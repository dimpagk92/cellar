//! `{}` ok-response for methods that don't return useful payloads.

use serde::{Deserialize, Serialize};

/// Empty success result. Serialised as `{}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OkResult {}
