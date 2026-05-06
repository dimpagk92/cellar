//! CEL boundary contracts.
//!
//! This crate holds the types that span the cortex/planner boundary so neither
//! side depends on the other. Cortex builds and executes these contract types;
//! planners produce them as output.
//!
//! **Strict policy:** depends only on `cel-context`, `serde`, `serde_json`, and
//! `async-trait`. No runtime, no prompt logic, no LLM dependencies. Anything
//! that needs more belongs in `cel-planner` (planner-side) or `cel-cortex`
//! (cortex-side).

pub mod actions;
pub mod canonical;
pub mod producer;
pub mod view;

pub use actions::{CellWrite, PlannedAction};
pub use canonical::{
    AttemptRecord, FailureReport, GoalOutcome, NextMove, RunLimits, RuntimeCaps, Step, StepKind,
    StepResult,
};
pub use producer::{DoneVerdict, PlanProducer};
pub use view::{
    AdapterFactRef, AnomalyRef, Blocker, CapabilityRef, EventRef, EvidenceRef, KnowledgeRef,
    MemoryRef, OmittedCounts, PlanningBudget, PlanningElement, PlanningElementState,
    PlanningScreen, PlanningView, RunProgress,
};
