//! CEL Unified Context API
//!
//! Merges all five context streams (display, input, accessibility, vision, network)
//! into a single structured world model. This is the core API that agents consume.

mod confidence;
mod element;
pub mod events;
mod merge;
mod resolve;
pub mod watchdog;

pub use confidence::{ConfidenceBehavior, ConfidenceThresholds};
pub use element::{
    classify_content_role, Bounds, BoundsRegion, ContentRole, ContextElement, ContextReference,
    ContextSource, ElementState, FocusedContext, ScreenContext, TranscriptEntry,
};
pub use events::CelEvent;
pub use merge::{
    aria_role_to_cel_type, assign_default_actions, build_from_external, is_actionable_type,
    score_element_confidence, ContextMerger, StreamStatus,
};
pub use resolve::resolve_reference;
pub use watchdog::ContextWatchdog;
