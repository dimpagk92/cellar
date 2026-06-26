//! Portable Context Snapshot API
//!
//! Normalizes arbitrary streams into [`ContextElement`]s and merges them into a
//! point-in-time [`ContextSnapshot`] an agent can consume.

mod confidence;
mod element;
pub mod events;
mod merge;
mod resolve;
pub mod watchdog;

pub use confidence::{ConfidenceBehavior, ConfidenceThresholds};
#[allow(deprecated)]
pub use element::ScreenContext;
pub use element::{
    classify_content_role, AudioState, Bounds, BoundsRegion, ClipboardState, ConnectionEvent,
    ContentRole, ContextElement, ContextReference, ContextSnapshot, ContextSource, ElementState,
    FocusedContext, HttpEvent, PowerState, RecentFile, RunningApp, TranscriptEntry, WindowState,
};
pub use events::CelEvent;
pub use merge::{
    aria_role_to_cel_type, assign_default_actions, build_from_external, is_actionable_type,
    score_element_confidence, ContextContribution, ContextMerger, StreamStatus,
};
pub use resolve::resolve_reference;
pub use watchdog::ContextWatchdog;
