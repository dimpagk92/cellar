//! Stub implementation used when the `spaces` feature is off or the target is
//! not macOS. Every operation reports "not compiled in" — no private framework
//! is linked and nothing can crash.

use crate::{Result, SpaceInfo, SpacesError};

/// Always `false`: Spaces support is not built in.
pub fn spaces_available() -> bool {
    false
}

pub fn active_space() -> Result<u64> {
    Err(SpacesError::NotCompiled)
}

pub fn list_spaces() -> Result<Vec<SpaceInfo>> {
    Err(SpacesError::NotCompiled)
}

pub fn switch_to_space(_display_uuid: &str, _space_id: u64) -> Result<()> {
    Err(SpacesError::NotCompiled)
}

pub fn move_window_to_space(_window_id: u32, _space_id: u64) -> Result<()> {
    Err(SpacesError::NotCompiled)
}
