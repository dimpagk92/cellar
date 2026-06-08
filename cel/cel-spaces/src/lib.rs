//! CEL Spaces — virtual-desktop ("Spaces" / Mission Control) control on macOS.
//!
//! ⚠️ **Private API.** This wraps undocumented SkyLight `CGS*` functions
//! (`CGSMainConnectionID`, `CGSGetActiveSpace`, `CGSCopyManagedDisplaySpaces`,
//! `CGSManagedDisplaySetCurrentSpace`, `CGSMoveWindowsToManagedSpace`). They
//! are not part of any public SDK, can change between macOS releases, and are
//! the **highest-fragility component** in the tree. Two safety nets gate them:
//!
//! 1. **Cargo feature `spaces`** (default OFF). Without it — and on any
//!    non-macOS target — this crate is pure safe stubs that return
//!    [`SpacesError::NotCompiled`] and never link the private framework.
//! 2. **Runtime probe** [`spaces_available`] — a zero connection id (or an
//!    unsupported build) reports unavailable so callers degrade gracefully.
//!
//! The real path is compile- and link-verified (building with `--features
//! spaces` resolves the private symbols against the SkyLight framework), but
//! its *runtime behaviour is unverified here* — it needs a live macOS desktop.

use serde::Serialize;

/// Errors from Spaces operations.
#[derive(Debug, thiserror::Error)]
pub enum SpacesError {
    /// The `spaces` feature was not enabled at build time (or non-macOS).
    #[error("Spaces support not compiled in — build cel-spaces with --features spaces on macOS")]
    NotCompiled,
    /// The SkyLight connection / APIs are unavailable on this system.
    #[error("Spaces unavailable on this macOS (no SkyLight connection)")]
    Unavailable,
    /// A SkyLight call failed or returned an unexpected shape.
    #[error("SkyLight call failed: {0}")]
    CallFailed(String),
}

/// Result alias for Spaces operations.
pub type Result<T> = std::result::Result<T, SpacesError>;

/// A managed Space on a display.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpaceInfo {
    /// The 64-bit managed-space id (what `switch`/`move-window` take).
    pub space_id: u64,
    /// The display this space belongs to (UUID string, or "Main").
    pub display_uuid: String,
    /// Whether this is the currently-visible space on its display.
    pub is_current: bool,
}

#[cfg(all(target_os = "macos", feature = "spaces"))]
mod imp;
#[cfg(all(target_os = "macos", feature = "spaces"))]
pub use imp::{active_space, list_spaces, move_window_to_space, spaces_available, switch_to_space};

#[cfg(not(all(target_os = "macos", feature = "spaces")))]
mod stub;
#[cfg(not(all(target_os = "macos", feature = "spaces")))]
pub use stub::{
    active_space, list_spaces, move_window_to_space, spaces_available, switch_to_space,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_info_serializes() {
        let s = SpaceInfo {
            space_id: 7,
            display_uuid: "Main".into(),
            is_current: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"space_id\":7"));
        assert!(json.contains("\"is_current\":true"));
    }

    // Without the `spaces` feature the stubs report unavailable and never
    // touch the private framework. This is the default build path.
    #[cfg(not(all(target_os = "macos", feature = "spaces")))]
    #[test]
    fn stubs_are_unavailable_by_default() {
        assert!(!spaces_available());
        assert!(matches!(active_space(), Err(SpacesError::NotCompiled)));
        assert!(matches!(list_spaces(), Err(SpacesError::NotCompiled)));
        assert!(matches!(
            switch_to_space("Main", 1),
            Err(SpacesError::NotCompiled)
        ));
        assert!(matches!(
            move_window_to_space(1, 1),
            Err(SpacesError::NotCompiled)
        ));
    }
}
