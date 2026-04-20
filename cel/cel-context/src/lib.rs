//! CEL Unified Context API
//!
//! Merges all five context streams (display, input, accessibility, vision, network)
//! into a single structured world model. This is the core API that agents consume.

mod element;
mod confidence;
pub mod events;
mod merge;
mod resolve;
pub mod watchdog;

pub use element::{
    Bounds, BoundsRegion, ContentRole, ContextElement, ContextReference, ContextSource,
    ElementState, FocusedContext, ScreenContext, TranscriptEntry, classify_content_role,
};
pub use confidence::{ConfidenceBehavior, ConfidenceThresholds};
pub use events::CelEvent;
pub use merge::{
    ContextMerger, build_from_external, score_element_confidence,
    aria_role_to_cel_type, assign_default_actions, is_actionable_type, StreamStatus,
};
pub use resolve::resolve_reference;
pub use watchdog::ContextWatchdog;
